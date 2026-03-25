"""
Pass 2: Collect references (Call, Read, Write, Decorate) and use a pluggable
resolver backend to resolve target symbols where possible.
"""

from __future__ import annotations

import ast
import os
import re
from dataclasses import dataclass
from typing import Optional

from .reference_index import (
    DefinitionIndex,
    add_parents,
    definition_symbol_id_from_target,
    full_name_from_target,
)
from .resolver_backend import DocumentResolver, ResolvedTarget
from .schema import (
    DocumentSemantics,
    FunctionDetails,
    Mutability,
    ReferenceRole,
    SourceLocation,
    SourceSpan,
    SymbolDefinition,
    SymbolKind,
    SymbolReference,
    TypeDetails,
    TypeKind,
    VariableDetails,
)

_BUILTIN_BEHAVIORAL_DECORATORS = {"classmethod", "staticmethod", "property"}
_BUILTIN_INTRINSIC_CALLS = {"object"}
_BUILTIN_BEHAVIORAL_BOUNDARIES = {f"builtins.{name}" for name in _BUILTIN_BEHAVIORAL_DECORATORS}
_NOISE_EXTERNAL_SYMBOLS = {"import"}
_UNKNOWN_HOVER_PREFIX = "Unknown | "
_PATHLIKE_RECEIVER_TYPES = {
    "pathlib.Path",
    "pathlib.PurePath",
    "pathlib.PosixPath",
    "pathlib.PurePosixPath",
    "pathlib.WindowsPath",
    "pathlib.PureWindowsPath",
}
_BOUND_METHOD_OWNER_RE = re.compile(r"(?:bound method |\(\s*bound method )([A-Za-z_][A-Za-z0-9_\.]*)\.")
_FUNCTION_SIGNATURE_RE = re.compile(r"^(?:async\s+)?def\s+[A-Za-z_][A-Za-z0-9_]*\(.*\)\s*(?:->\s*(.+))?$")
_INVALID_TYPE_REFS = {
    "Any",
    "Unknown",
    "None",
    "Never",
    "typing.Any",
    "t.Any",
}


@dataclass(frozen=True, slots=True)
class _ImportBinding:
    imported_as: str
    qualified_name: str
    line: int
    column: int


@dataclass(frozen=True, slots=True)
class _LocalAssignment:
    line: int
    column: int
    annotation: ast.AST | None = None
    value: ast.AST | None = None


class ReferenceCollector(ast.NodeVisitor):
    """Collects references and uses the injected resolver to resolve targets."""

    def __init__(
        self,
        file_path: str,
        source: str,
        project_root: str,
        all_definitions: list[SymbolDefinition],
        definition_index: DefinitionIndex,
        module_symbol_id: str,
        resolver: DocumentResolver,
    ):
        self.file_path = file_path
        self.rel_path = os.path.relpath(file_path, project_root).replace("\\", "/")
        self.source = source
        self.project_root = project_root
        self.all_definitions = all_definitions
        self.definition_index = definition_index
        self.module_symbol_id = module_symbol_id
        self.resolver = resolver
        self.references: list[SymbolReference] = []
        self.external_symbols: dict[str, SymbolDefinition] = {}
        self._func_spans: list[tuple[int, int, str]] = []
        self._goto_cache: dict[tuple[int, int, bool], list[ResolvedTarget]] = {}
        self._enclosing_defined_cache: dict[str, Optional[str]] = {}
        self._class_prefix_cache: dict[str, Optional[str]] = {}
        self._import_bindings: list[_ImportBinding] | None = None
        self._import_bindings_by_name: dict[str, list[_ImportBinding]] | None = None
        self._narrow_by_import_cache: dict[tuple[str, tuple[str, ...]], Optional[str]] = {}
        self._local_type_cache: dict[tuple[str, str, int, int], Optional[str]] = {}
        self._local_type_in_progress: set[tuple[str, str, int, int]] = set()
        self._function_local_assignments: dict[int, dict[str, list[_LocalAssignment]]] = {}
        self._variable_value_type_cache: dict[str, Optional[str]] = {}
        self._variable_value_type_in_progress: set[str] = set()
        self._internal_roots = {
            definition.symbol_id.split(".", 1)[0]
            for definition in all_definitions
            if definition.symbol_id
        }
        self.tree: Optional[ast.AST] = None

    def _kind_rank(self, kind: SymbolKind) -> int:
        if kind == SymbolKind.Variable:
            return 0
        if kind == SymbolKind.Function:
            return 1
        return 2

    def _collect_external_symbol(
        self,
        target_sym: str,
        resolved: ResolvedTarget,
        *,
        kind_hint: SymbolKind = SymbolKind.Variable,
    ) -> None:
        name = resolved.name or target_sym.rsplit(".", 1)[-1]
        if not name:
            return

        file_path = resolved.path or "unknown"
        line = max(0, resolved.line)
        column = max(0, resolved.column)
        loc = SourceLocation(file_path=file_path, line=line, column=column)
        span = SourceSpan(
            start_line=line,
            start_column=column,
            end_line=line + 1,
            end_column=column,
        )

        kind = kind_hint
        details: FunctionDetails | VariableDetails | TypeDetails
        if kind == SymbolKind.Type:
            details = TypeDetails(kind=TypeKind.Class)
        elif kind == SymbolKind.Function:
            details = FunctionDetails()
        else:
            details = VariableDetails(mutability=Mutability.Immutable)

        existing = self.external_symbols.get(target_sym)
        if existing and self._kind_rank(existing.kind) >= self._kind_rank(kind):
            return

        self.external_symbols[target_sym] = SymbolDefinition(
            symbol_id=target_sym,
            kind=kind,
            name=name,
            display_name=resolved.full_name or name,
            location=loc,
            span=span,
            enclosing_symbol=None,
            is_external=True,
            documentation=resolved.documentation,
            details=details,
        )

    def _resolve_symbol_from_targets_with_hint(
        self,
        defs: list[ResolvedTarget],
        *,
        kind_hint: SymbolKind,
    ) -> Optional[str]:
        if not defs:
            return None

        target_sym = definition_symbol_id_from_target(self.definition_index, defs[0])
        if not target_sym:
            target_sym = full_name_from_target(defs[0])
            if target_sym and self._should_externalize(defs[0]):
                self._collect_external_symbol(target_sym, defs[0], kind_hint=kind_hint)
            else:
                target_sym = None
        return target_sym

    def _should_externalize(self, resolved: ResolvedTarget) -> bool:
        full_name = full_name_from_target(resolved)
        if full_name in _NOISE_EXTERNAL_SYMBOLS or resolved.name in _NOISE_EXTERNAL_SYMBOLS:
            return False
        if full_name and full_name.startswith("builtins."):
            return False
        if resolved.kind in {"param", "statement"}:
            return False
        if resolved.path:
            abs_path = os.path.abspath(resolved.path)
            try:
                common = os.path.commonpath([self.project_root, abs_path])
            except ValueError:
                common = None
            if common == self.project_root:
                return False
        return bool(resolved.full_name or resolved.name)

    def _resolve_symbol_at_with_hint(
        self,
        line: int,
        column: int,
        *,
        follow_imports: bool = False,
        kind_hint: SymbolKind,
    ) -> Optional[str]:
        return self._resolve_symbol_from_targets_with_hint(
            self._resolve_at(line, column, follow_imports=follow_imports),
            kind_hint=kind_hint,
        )

    def _should_keep_target(self, target_symbol: str | None, *, role: ReferenceRole) -> bool:
        if not target_symbol:
            return True
        if target_symbol in _NOISE_EXTERNAL_SYMBOLS:
            return False
        if not target_symbol.startswith("builtins."):
            return True
        return role == ReferenceRole.Decorate and target_symbol in _BUILTIN_BEHAVIORAL_BOUNDARIES

    def _synthetic_target(self, symbol_id: str, node: ast.AST, *, kind: str) -> ResolvedTarget:
        return ResolvedTarget(
            path=self.rel_path,
            line=node.lineno - 1,
            column=getattr(node, "col_offset", 0) or 0,
            name=symbol_id.rsplit(".", 1)[-1],
            full_name=symbol_id,
            kind=kind,
            documentation=[],
        )

    def _append_reference(
        self,
        *,
        target_symbol: str | None,
        location: SourceLocation,
        enclosing_symbol: str | None,
        role: ReferenceRole,
        receiver: str | None = None,
        method_name: str | None = None,
        assigned_to: str | None = None,
    ) -> None:
        if not self._should_keep_target(target_symbol, role=role):
            return
        self.references.append(
            SymbolReference(
                target_symbol=target_symbol,
                location=location,
                enclosing_symbol=enclosing_symbol,
                role=role,
                receiver=receiver,
                method_name=method_name,
                assigned_to=assigned_to,
            )
        )

    def _resolve_internal_symbol_from_targets(self, defs: list[ResolvedTarget]) -> Optional[str]:
        if not defs:
            return None
        return definition_symbol_id_from_target(self.definition_index, defs[0])

    def _resolve_internal_symbol_at(
        self,
        line: int,
        column: int,
        *,
        follow_imports: bool = False,
    ) -> Optional[str]:
        return self._resolve_internal_symbol_from_targets(
            self._resolve_at(line, column, follow_imports=follow_imports)
        )

    def _resolve_internal_variable_at(
        self,
        line: int,
        column: int,
        *,
        follow_imports: bool = False,
    ) -> Optional[str]:
        symbol = self._resolve_internal_symbol_at(line, column, follow_imports=follow_imports)
        definition = self._definition(symbol)
        if definition and definition.kind == SymbolKind.Variable:
            return symbol
        return None

    def _definition(self, symbol_id: str | None) -> Optional[SymbolDefinition]:
        if not symbol_id:
            return None
        return self.definition_index.by_symbol_id.get(symbol_id) or self.external_symbols.get(symbol_id)

    def _is_internal_kind(self, symbol_id: str | None, kind: SymbolKind) -> bool:
        definition = self._definition(symbol_id)
        return bool(definition and definition.kind == kind and not definition.is_external)

    def _is_external_symbol(self, symbol_id: str | None) -> bool:
        definition = self._definition(symbol_id)
        return bool(definition and definition.is_external)

    def _is_read_target(self, symbol_id: str | None) -> bool:
        return (
            self._is_internal_kind(symbol_id, SymbolKind.Variable)
            or self._is_internal_kind(symbol_id, SymbolKind.Function)
            or self._is_external_symbol(symbol_id)
        )

    def _is_write_target(self, symbol_id: str | None) -> bool:
        return self._is_internal_kind(symbol_id, SymbolKind.Variable) or self._is_external_symbol(symbol_id)

    def _type_candidates(self, name: str) -> list[SymbolDefinition]:
        return self.definition_index.types_by_name.get(name, [])

    def _find_type_for_enclosing_symbol(self, enc: str) -> Optional[str]:
        cached = self._class_prefix_cache.get(enc)
        if cached is not None:
            return cached

        for definition in self.definition_index.type_by_symbol_id.values():
            if enc.startswith(definition.symbol_id + "."):
                self._class_prefix_cache[enc] = definition.symbol_id
                return definition.symbol_id

        self._class_prefix_cache[enc] = None
        return None

    def _enclosing_symbol(self, node: ast.AST) -> Optional[str]:
        line_1 = node.lineno
        for start, end, sym_id in reversed(self._func_spans):
            if start <= line_1 <= end:
                return sym_id
        return self.module_symbol_id

    def _enclosing_symbol_defined(self, node: ast.AST) -> Optional[str]:
        enc = self._enclosing_symbol(node)
        if enc in self._enclosing_defined_cache:
            return self._enclosing_defined_cache[enc]

        while enc and "." in enc:
            if enc in self.definition_index.by_symbol_id:
                self._enclosing_defined_cache[self._enclosing_symbol(node)] = enc
                return enc
            enc = enc.rsplit(".", 1)[0]
        result = enc if (enc and enc in self.definition_index.by_symbol_id) else (
            self.module_symbol_id if self.module_symbol_id in self.definition_index.by_symbol_id else enc
        )
        self._enclosing_defined_cache[self._enclosing_symbol(node)] = result
        return result

    def _loc(self, node: ast.AST) -> SourceLocation:
        return SourceLocation(
            file_path=self.rel_path,
            line=node.lineno - 1,
            column=getattr(node, "col_offset", 0) or 0,
        )

    def _query_position(self, node: ast.AST) -> tuple[int, int]:
        line = node.lineno
        column = getattr(node, "col_offset", 0) or 0
        if isinstance(node, ast.Attribute):
            end_column = getattr(node, "end_col_offset", column) or column
            attr_start = end_column - len(node.attr)
            if attr_start >= column:
                column = attr_start
        return line, column

    def _resolve_symbol_for_node_with_hint(
        self,
        node: ast.AST,
        *,
        follow_imports: bool = False,
        kind_hint: SymbolKind,
    ) -> Optional[str]:
        line, column = self._query_position(node)
        return self._resolve_symbol_at_with_hint(
            line,
            column,
            follow_imports=follow_imports,
            kind_hint=kind_hint,
        )

    def _resolve_internal_symbol_for_node(
        self,
        node: ast.AST,
        *,
        follow_imports: bool = False,
    ) -> Optional[str]:
        line, column = self._query_position(node)
        return self._resolve_internal_symbol_at(
            line,
            column,
            follow_imports=follow_imports,
        )

    def _prefer_same_module_variable(self, target_sym: str, bare_name: str) -> str:
        if target_sym in self.definition_index.by_symbol_id:
            return target_sym
        same_module = [
            definition
            for definition in self.definition_index.variables_or_functions_by_name.get(bare_name, [])
            if definition.kind == SymbolKind.Variable
            and (
                definition.symbol_id == f"{self.module_symbol_id}.{bare_name}"
                or definition.symbol_id.startswith(self.module_symbol_id + ".")
            )
        ]
        if len(same_module) == 1:
            return same_module[0].symbol_id
        return target_sym

    def _same_module_variable_symbol(self, bare_name: str) -> Optional[str]:
        same_module = [
            definition
            for definition in self.definition_index.variables_or_functions_by_name.get(bare_name, [])
            if definition.kind == SymbolKind.Variable
            and (
                definition.symbol_id == f"{self.module_symbol_id}.{bare_name}"
                or definition.symbol_id.startswith(self.module_symbol_id + ".")
            )
        ]
        if len(same_module) == 1:
            return same_module[0].symbol_id
        return None

    def _resolve_at(self, line: int, column: int, *, follow_imports: bool = False) -> list[ResolvedTarget]:
        cache_key = (line, column, follow_imports)
        if cache_key in self._goto_cache:
            return self._goto_cache[cache_key]
        resolved = self.resolver.goto(line, column, follow_imports=follow_imports)
        self._goto_cache[cache_key] = resolved
        return resolved

    def _is_inside_default_value(self, node: ast.AST) -> bool:
        n = node
        while getattr(n, "parent", None) is not None:
            par = n.parent
            if isinstance(par, ast.arguments):
                for d in par.defaults or []:
                    if any(n is x for x in ast.walk(d)):
                        return True
                for k in par.kw_defaults or []:
                    if k is not None and any(n is x for x in ast.walk(k)):
                        return True
                return False
            n = par
        return False

    def _is_inside_annotation(self, node: ast.AST) -> bool:
        current = node
        while getattr(current, "parent", None) is not None:
            parent = current.parent
            if isinstance(parent, ast.arg) and parent.annotation is current:
                return True
            if isinstance(parent, ast.AnnAssign) and parent.annotation is current:
                return True
            if isinstance(parent, (ast.FunctionDef, ast.AsyncFunctionDef)) and parent.returns is current:
                return True
            if isinstance(parent, ast.ClassDef) and current in parent.bases:
                return True
            current = parent
        return False

    def _looks_like_type_alias_name(self, name: str) -> bool:
        return bool(name) and name[0].isupper()

    def _is_type_expression(self, node: ast.AST) -> bool:
        if isinstance(node, (ast.Name, ast.Attribute)):
            return True
        if isinstance(node, ast.Constant):
            return node.value is None or node.value is Ellipsis or isinstance(node.value, str)
        if isinstance(node, ast.Subscript):
            return self._is_type_expression(node.value) and self._is_type_expression(node.slice)
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
            return self._is_type_expression(node.left) and self._is_type_expression(node.right)
        if isinstance(node, ast.Tuple):
            return all(self._is_type_expression(elt) for elt in node.elts)
        if isinstance(node, ast.List):
            return all(self._is_type_expression(elt) for elt in node.elts)
        return False

    def _is_inside_type_alias_value(self, node: ast.AST) -> bool:
        child = node
        current = node
        while getattr(current, "parent", None) is not None:
            parent = current.parent
            if isinstance(parent, ast.Assign) and parent.value is child:
                if len(parent.targets) != 1 or not isinstance(parent.targets[0], ast.Name):
                    return False
                return (
                    self._looks_like_type_alias_name(parent.targets[0].id)
                    and self._is_type_expression(parent.value)
                )
            if isinstance(parent, ast.AnnAssign) and parent.value is child and isinstance(parent.target, ast.Name):
                return (
                    self._looks_like_type_alias_name(parent.target.id)
                    and self._is_type_expression(parent.value)
                )
            child = parent
            current = parent
        return False

    def _is_call_callee(self, node: ast.AST) -> bool:
        parent = getattr(node, "parent", None)
        return isinstance(parent, ast.Call) and parent.func is node

    def _is_decorator_expr(self, node: ast.AST) -> bool:
        current = node
        while getattr(current, "parent", None) is not None:
            parent = current.parent
            if isinstance(parent, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and current in parent.decorator_list:
                return True
            current = parent
        return False

    def _builtin_external_symbol_id(self, name: str, *, kind: SymbolKind, node: ast.AST) -> str:
        symbol_id = f"builtins.{name}"
        self._collect_external_symbol(
            symbol_id,
            ResolvedTarget(
                path="builtins",
                line=node.lineno - 1,
                column=getattr(node, "col_offset", 0) or 0,
                name=name,
                full_name=symbol_id,
                kind="function",
                documentation=[],
            ),
            kind_hint=kind,
        )
        return symbol_id

    def _is_builtin_intrinsic_call(self, node: ast.Call) -> bool:
        if not isinstance(node.func, ast.Name):
            return False
        if node.func.id not in _BUILTIN_INTRINSIC_CALLS:
            return False
        return self._resolve_internal_symbol_at(node.func.lineno, node.func.col_offset) is None

    def _collect_read_ref_from_expr(self, expr: ast.AST, enclosing_symbol: str) -> None:
        if isinstance(expr, ast.Name) and isinstance(expr.ctx, ast.Load):
            target_sym = self._resolve_name_to_read_target(expr)
            if target_sym:
                self._append_reference(
                    target_symbol=target_sym,
                    location=self._loc(expr),
                    enclosing_symbol=enclosing_symbol,
                    role=ReferenceRole.Read,
                )
            return
        if isinstance(expr, ast.Attribute) and isinstance(expr.ctx, ast.Load):
            target_sym = self._resolve_symbol_for_node_with_hint(expr, kind_hint=SymbolKind.Variable)
            target_sym = self._prefer_import_fallback(target_sym, expr, kind_hint=SymbolKind.Variable)
            if target_sym and self._is_read_target(target_sym):
                receiver_sym = None
                if isinstance(expr.value, ast.Name):
                    receiver_sym = self._resolve_internal_variable_at(
                        expr.value.lineno,
                        expr.value.col_offset,
                    )
                self._append_reference(
                    target_symbol=target_sym,
                    location=self._loc(expr),
                    enclosing_symbol=enclosing_symbol,
                    role=ReferenceRole.Read,
                    receiver=receiver_sym,
                )
            return
        for child in ast.iter_child_nodes(expr):
            self._collect_read_ref_from_expr(child, enclosing_symbol)

    def _resolve_name_to_read_target(self, node: ast.Name) -> Optional[str]:
        target_sym = self._resolve_symbol_at_with_hint(
            node.lineno,
            node.col_offset,
            kind_hint=SymbolKind.Variable,
        )
        if not target_sym:
            target_sym = self._resolve_symbol_at_with_hint(
                node.lineno,
                node.col_offset,
                follow_imports=True,
                kind_hint=SymbolKind.Variable,
            )
        target_sym = self._prefer_import_fallback(target_sym, node, kind_hint=SymbolKind.Variable)
        if target_sym and self._is_read_target(target_sym):
            return target_sym

        same_file = self.definition_index.variables_or_functions_by_file_name.get((self.rel_path, node.id), [])
        if len(same_file) == 1:
            return same_file[0].symbol_id
        if same_file:
            return same_file[0].symbol_id

        by_name = self.definition_index.variables_or_functions_by_name.get(node.id, [])
        if len(by_name) == 1:
            return by_name[0].symbol_id
        if len(by_name) > 1 and self.tree is not None:
            narrowed = self._narrow_by_import(node.id, by_name)
            if narrowed:
                return narrowed
        return None

    def _fallback_imported_symbol(self, name: str) -> Optional[str]:
        by_name = self.definition_index.variables_or_functions_by_name.get(name, [])
        if len(by_name) == 1:
            return by_name[0].symbol_id
        if len(by_name) > 1 and self.tree is not None:
            return self._narrow_by_import(name, by_name)
        return None

    def _resolve_import_module_prefix(self, node: ast.ImportFrom) -> Optional[str]:
        if node.level == 0:
            return node.module
        module_path = self.rel_path.replace("\\", "/")
        if module_path.endswith(".py"):
            module_path = module_path[:-3]
        parts = module_path.split("/")
        if node.level > len(parts):
            return None
        parent_parts = parts[: len(parts) - node.level]
        parent_module = ".".join(parent_parts)
        if node.module:
            return f"{parent_module}.{node.module}" if parent_module else node.module
        return parent_module

    def _sorted_import_nodes(self) -> list[ast.Import | ast.ImportFrom]:
        if self.tree is None:
            return []
        nodes = [node for node in ast.walk(self.tree) if isinstance(node, (ast.Import, ast.ImportFrom))]
        return sorted(nodes, key=lambda node: (getattr(node, "lineno", 0), getattr(node, "col_offset", 0)))

    def _all_import_bindings(self) -> list[_ImportBinding]:
        if self._import_bindings is not None:
            return self._import_bindings

        bindings: list[_ImportBinding] = []
        for node in self._sorted_import_nodes():
            if isinstance(node, ast.ImportFrom):
                module_prefix = self._resolve_import_module_prefix(node)
                if not module_prefix:
                    continue
                for alias in node.names:
                    if alias.name == "*":
                        continue
                    imported_as = alias.asname or alias.name
                    bindings.append(
                        _ImportBinding(
                            imported_as=imported_as,
                            qualified_name=f"{module_prefix}.{alias.name}",
                            line=(alias.lineno or node.lineno) - 1,
                            column=alias.col_offset or node.col_offset or 0,
                        )
                    )
                continue

            for alias in node.names:
                imported_as = alias.asname or alias.name.split(".", 1)[0]
                qualified_name = alias.name if alias.asname else alias.name.split(".", 1)[0]
                bindings.append(
                    _ImportBinding(
                        imported_as=imported_as,
                        qualified_name=qualified_name,
                        line=(alias.lineno or node.lineno) - 1,
                        column=alias.col_offset or node.col_offset or 0,
                    )
                )

        bindings_by_name: dict[str, list[_ImportBinding]] = {}
        for binding in bindings:
            bindings_by_name.setdefault(binding.imported_as, []).append(binding)
        self._import_bindings = bindings
        self._import_bindings_by_name = bindings_by_name
        return bindings

    def _binding_for_name(self, name: str, *, line: int, column: int) -> Optional[_ImportBinding]:
        best: _ImportBinding | None = None
        for binding in self._all_import_bindings():
            if binding.imported_as != name:
                continue
            if (binding.line, binding.column) > (line, column):
                continue
            if best is None or (binding.line, binding.column) > (best.line, best.column):
                best = binding
        return best

    def _qualified_name_from_expr(self, expr: ast.AST) -> Optional[str]:
        if isinstance(expr, ast.Name):
            binding = self._binding_for_name(
                expr.id,
                line=expr.lineno - 1,
                column=getattr(expr, "col_offset", 0) or 0,
            )
            return binding.qualified_name if binding else None
        if isinstance(expr, ast.Attribute):
            base = self._qualified_name_from_expr(expr.value)
            if base:
                return f"{base}.{expr.attr}"
        return None

    def _symbol_id_from_qualified_name(self, qualified_name: str) -> Optional[str]:
        exact = self.definition_index.by_symbol_id.get(qualified_name)
        if exact:
            return exact.symbol_id

        leaf_name = qualified_name.rsplit(".", 1)[-1]
        module_prefix = qualified_name.rsplit(".", 1)[0] if "." in qualified_name else ""
        candidates = self.definition_index.by_name.get(leaf_name, [])
        if not candidates:
            return None

        package_init_symbol_id = f"{module_prefix}.__init__.{leaf_name}" if module_prefix else None
        if package_init_symbol_id:
            init_matches = [
                definition for definition in candidates if definition.symbol_id == package_init_symbol_id
            ]
            if len(init_matches) == 1:
                return init_matches[0].symbol_id

        prefix_matches = [
            definition
            for definition in candidates
            if module_prefix and definition.symbol_id.startswith(module_prefix + ".")
        ]
        if len(prefix_matches) == 1:
            return prefix_matches[0].symbol_id
        return None

    def _stable_symbol_from_import_expr(self, expr: ast.AST, *, kind_hint: SymbolKind) -> Optional[str]:
        qualified_name = self._qualified_name_from_expr(expr)
        if not qualified_name or qualified_name in _NOISE_EXTERNAL_SYMBOLS:
            return None

        symbol_id = self._symbol_id_from_qualified_name(qualified_name)
        if symbol_id:
            return symbol_id

        if qualified_name.startswith("builtins."):
            return None

        self._collect_external_symbol(
            qualified_name,
            self._synthetic_target(
                qualified_name,
                expr,
                kind="function" if kind_hint == SymbolKind.Function else "statement",
            ),
            kind_hint=kind_hint,
        )
        return qualified_name

    def _prefer_import_fallback(
        self,
        current_target: str | None,
        expr: ast.AST,
        *,
        kind_hint: SymbolKind,
    ) -> str | None:
        fallback_target = self._stable_symbol_from_import_expr(expr, kind_hint=kind_hint)
        if not fallback_target:
            return current_target
        if current_target and current_target.startswith("builtins."):
            return current_target
        return fallback_target

    def _literal_type_ref(self, type_ref: str) -> Optional[str]:
        stripped = type_ref.strip()
        if stripped in {"LiteralString", "typing.LiteralString", "t.LiteralString"}:
            return "builtins.str"

        for prefix in ("Literal[", "typing.Literal[", "t.Literal["):
            if not stripped.startswith(prefix) or not stripped.endswith("]"):
                continue
            inner = stripped[len(prefix) : -1].strip()
            if not inner:
                return None
            try:
                literal_value = ast.literal_eval(inner)
            except (SyntaxError, ValueError):
                return None
            if isinstance(literal_value, tuple):
                item_types = {
                    builtin_type
                    for item in literal_value
                    if (builtin_type := self._builtin_receiver_type_from_constant(item))
                }
                if len(item_types) == 1:
                    return next(iter(item_types))
                return None
            return self._builtin_receiver_type_from_constant(literal_value)
        return None

    def _normalize_type_ref(self, type_ref: str | None, *, line: int, column: int) -> Optional[str]:
        if not type_ref:
            return None
        literal_type = self._literal_type_ref(type_ref)
        if literal_type:
            return literal_type
        normalized = type_ref.split("[", 1)[0].strip()
        if not normalized:
            return None
        if not self._is_informative_type_ref(normalized):
            return None
        if "." in normalized:
            return self._symbol_id_from_qualified_name(normalized) or normalized
        binding = self._binding_for_name(normalized, line=line, column=column)
        if binding:
            return self._symbol_id_from_qualified_name(binding.qualified_name) or binding.qualified_name
        candidates = self._type_candidates(normalized)
        if len(candidates) == 1:
            return candidates[0].symbol_id
        return normalized

    def _is_likely_internal_symbol(self, symbol_id: str) -> bool:
        if symbol_id in self.definition_index.by_symbol_id:
            return True
        return symbol_id.split(".", 1)[0] in self._internal_roots

    def _ensure_external_symbol(
        self,
        target_sym: str | None,
        node: ast.AST,
        *,
        kind_hint: SymbolKind,
    ) -> None:
        if not target_sym or target_sym.startswith("builtins."):
            return
        if target_sym in self.definition_index.by_symbol_id or target_sym in self.external_symbols:
            return
        if self._is_likely_internal_symbol(target_sym):
            return

        kind = "function" if kind_hint == SymbolKind.Function else "statement"
        self._collect_external_symbol(
            target_sym,
            self._synthetic_target(target_sym, node, kind=kind),
            kind_hint=kind_hint,
        )

    def _value_type_from_symbol(
        self,
        symbol_id: str | None,
        *,
        line: int,
        column: int,
    ) -> Optional[str]:
        if not symbol_id:
            return None

        definition = self._definition(symbol_id)
        if definition:
            if definition.kind == SymbolKind.Type:
                return definition.symbol_id

            if definition.kind == SymbolKind.Function:
                return self._function_return_type(symbol_id, line=line, column=column) or self._symbol_id_to_receiver_type(
                    symbol_id
                )

            if definition.kind == SymbolKind.Variable and isinstance(definition.details, VariableDetails):
                declared_type = self._normalize_type_ref(
                    definition.details.var_type,
                    line=line,
                    column=column,
                )
                if declared_type:
                    return declared_type
                return self._infer_variable_value_type(definition)

            return None

        return self._symbol_id_to_receiver_type(symbol_id)

    def _target_matches_definition(
        self,
        target: ast.AST,
        definition: SymbolDefinition,
    ) -> bool:
        line = definition.location.line
        column = definition.location.column

        if isinstance(target, ast.Name):
            return (
                target.id == definition.name
                and target.lineno - 1 == line
                and (getattr(target, "col_offset", 0) or 0) == column
            )

        if isinstance(target, ast.Attribute):
            return (
                target.attr == definition.name
                and target.lineno - 1 == line
                and (getattr(target, "col_offset", 0) or 0) == column
            )

        return False

    def _value_expr_for_variable_definition(
        self,
        definition: SymbolDefinition,
    ) -> Optional[ast.AST]:
        if not self.tree or definition.location.file_path != self.rel_path:
            return None

        for node in ast.walk(self.tree):
            if isinstance(node, ast.Assign):
                for target in node.targets:
                    if self._target_matches_definition(target, definition):
                        return node.value
            elif isinstance(node, ast.AnnAssign):
                if self._target_matches_definition(node.target, definition):
                    return node.value
        return None

    def _infer_variable_value_type(self, definition: SymbolDefinition) -> Optional[str]:
        cached = self._variable_value_type_cache.get(definition.symbol_id)
        if definition.symbol_id in self._variable_value_type_cache:
            return cached
        if definition.symbol_id in self._variable_value_type_in_progress:
            return None

        self._variable_value_type_in_progress.add(definition.symbol_id)
        try:
            value_expr = self._value_expr_for_variable_definition(definition)
            if value_expr is None:
                self._variable_value_type_cache[definition.symbol_id] = None
                return None

            enclosing_symbol = (
                self._ast_enclosing_defined_symbol(value_expr)
                or definition.enclosing_symbol
                or self.module_symbol_id
            )
            inferred_type = self._infer_expr_value_type(
                value_expr,
                enclosing_symbol,
            )
            self._variable_value_type_cache[definition.symbol_id] = inferred_type
            return inferred_type
        finally:
            self._variable_value_type_in_progress.discard(definition.symbol_id)

    def _builtin_receiver_type_from_constant(self, value: object) -> Optional[str]:
        if isinstance(value, bool):
            return "builtins.bool"
        if isinstance(value, str):
            return "builtins.str"
        if isinstance(value, bytes):
            return "builtins.bytes"
        if isinstance(value, int):
            return "builtins.int"
        if isinstance(value, float):
            return "builtins.float"
        if isinstance(value, complex):
            return "builtins.complex"
        return None

    def _function_return_type(self, symbol_id: str | None, *, line: int, column: int) -> Optional[str]:
        definition = self.definition_index.by_symbol_id.get(symbol_id or "")
        if not definition or definition.kind != SymbolKind.Function:
            return None
        if not isinstance(definition.details, FunctionDetails):
            return None
        if not definition.details.return_types:
            return None
        return self._normalize_type_ref(definition.details.return_types[0], line=line, column=column)

    def _symbol_id_to_receiver_type(self, symbol_id: str | None) -> Optional[str]:
        if not symbol_id:
            return None
        definition = self._definition(symbol_id)
        if definition and definition.kind == SymbolKind.Type:
            return definition.symbol_id
        if symbol_id.endswith(".__init__"):
            type_symbol = symbol_id.rsplit(".", 1)[0]
            type_def = self.definition_index.type_by_symbol_id.get(type_symbol)
            if type_def:
                return type_def.symbol_id
            leaf = type_symbol.rsplit(".", 1)[-1]
            if leaf and leaf[0].isupper():
                return type_symbol
        prefix, _, leaf = symbol_id.rpartition(".")
        if not prefix:
            return None
        prefix_def = self.definition_index.type_by_symbol_id.get(prefix)
        if prefix_def:
            return prefix_def.symbol_id
        if leaf == "__call__":
            prefix_leaf = prefix.rsplit(".", 1)[-1]
            if prefix_leaf and prefix_leaf[0].isupper():
                return prefix
        prefix_leaf = prefix.rsplit(".", 1)[-1]
        if prefix_leaf and prefix_leaf[0].isupper():
            return prefix
        return None

    def _enclosing_function_node(self, node: ast.AST) -> Optional[ast.FunctionDef | ast.AsyncFunctionDef]:
        current = node
        while getattr(current, "parent", None) is not None:
            current = current.parent
            if isinstance(current, (ast.FunctionDef, ast.AsyncFunctionDef)):
                return current
        return None

    def _ast_enclosing_defined_symbol(self, node: ast.AST) -> Optional[str]:
        scopes: list[tuple[str, int]] = []
        current = node
        while getattr(current, "parent", None) is not None:
            current = current.parent
            if isinstance(current, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
                scopes.append((current.name, current.lineno - 1))

        scopes.reverse()
        matched_symbol = self.module_symbol_id if self.module_symbol_id in self.definition_index.by_symbol_id else None
        prefix = matched_symbol
        for name, line in scopes:
            candidates = [
                definition
                for definition in self.definition_index.by_file_and_name.get((self.rel_path, name), [])
                if definition.location.line == line
            ]
            if not candidates:
                continue

            if prefix:
                scoped = [
                    definition
                    for definition in candidates
                    if definition.symbol_id == f"{prefix}.{name}"
                    or definition.symbol_id.startswith(prefix + "." + name)
                ]
                if scoped:
                    candidates = scoped

            chosen = min(candidates, key=lambda definition: definition.symbol_id.count("."))
            matched_symbol = chosen.symbol_id
            prefix = chosen.symbol_id

        return matched_symbol

    def _is_informative_type_ref(self, type_ref: str | None) -> bool:
        if not type_ref:
            return False
        stripped = type_ref.strip()
        if not stripped or stripped in _INVALID_TYPE_REFS:
            return False
        if stripped.startswith(("def ", "async def ", "class ")):
            return False
        if "@" in stripped:
            return False
        return True

    def _infer_local_name_type(
        self,
        name: str,
        node: ast.AST,
        enclosing_symbol: str,
    ) -> Optional[str]:
        cache_key = (
            enclosing_symbol,
            name,
            node.lineno - 1,
            getattr(node, "col_offset", 0) or 0,
        )
        if cache_key in self._local_type_cache:
            return self._local_type_cache[cache_key]
        if cache_key in self._local_type_in_progress:
            return None

        func_node = self._enclosing_function_node(node)
        if func_node is None:
            self._local_type_cache[cache_key] = None
            return None

        self._local_type_in_progress.add(cache_key)
        best: Optional[str] = None
        try:
            current_pos = (node.lineno - 1, getattr(node, "col_offset", 0) or 0)
            assignments = self._local_assignments_for_function(func_node).get(name, [])
            for assignment in reversed(assignments):
                assignment_pos = (assignment.line, assignment.column)
                if assignment_pos >= current_pos:
                    continue
                if assignment.annotation is not None:
                    best = self._normalize_type_ref(
                        ast.unparse(assignment.annotation),
                        line=assignment.line,
                        column=assignment.column,
                    )
                elif assignment.value is not None:
                    best = self._infer_receiver_type_symbol(assignment.value, enclosing_symbol)
                if best:
                    break
        finally:
            self._local_type_in_progress.discard(cache_key)

        self._local_type_cache[cache_key] = best
        return best

    def _local_assignments_for_function(
        self,
        func_node: ast.FunctionDef | ast.AsyncFunctionDef,
    ) -> dict[str, list[_LocalAssignment]]:
        cache_key = id(func_node)
        cached = self._function_local_assignments.get(cache_key)
        if cached is not None:
            return cached

        assignments: dict[str, list[_LocalAssignment]] = {}
        for candidate in ast.walk(func_node):
            if isinstance(candidate, ast.AnnAssign) and isinstance(candidate.target, ast.Name):
                assignments.setdefault(candidate.target.id, []).append(
                    _LocalAssignment(
                        line=candidate.lineno - 1,
                        column=getattr(candidate, "col_offset", 0) or 0,
                        annotation=candidate.annotation,
                    )
                )
            elif isinstance(candidate, ast.Assign):
                for target in candidate.targets:
                    if isinstance(target, ast.Name):
                        assignments.setdefault(target.id, []).append(
                            _LocalAssignment(
                                line=candidate.lineno - 1,
                                column=getattr(candidate, "col_offset", 0) or 0,
                                value=candidate.value,
                            )
                        )

        for entries in assignments.values():
            entries.sort(key=lambda assignment: (assignment.line, assignment.column))

        self._function_local_assignments[cache_key] = assignments
        return assignments

    def _hover_type_candidates(self, node: ast.AST) -> list[str]:
        candidates: list[str] = []
        line, column = self._query_position(node)
        for hover_text in self.resolver.hover(line, column):
            first_line = hover_text.splitlines()[0].strip()
            if not first_line:
                continue

            method_match = _BOUND_METHOD_OWNER_RE.search(first_line)
            if method_match:
                candidates.append(method_match.group(1))
                continue

            signature_match = _FUNCTION_SIGNATURE_RE.match(first_line)
            if signature_match:
                return_type = (signature_match.group(1) or "").strip()
                if return_type:
                    candidates.append(return_type)
                continue

            cleaned = first_line.removeprefix(_UNKNOWN_HOVER_PREFIX).strip()
            for part in cleaned.split("|"):
                candidate = part.strip().strip("()")
                if not candidate or candidate == "Unknown":
                    continue
                candidate = candidate.split("(", 1)[0].strip()
                if candidate:
                    candidates.append(candidate)

        normalized: list[str] = []
        seen: set[str] = set()
        line = line - 1
        for candidate in candidates:
            if not self._is_informative_type_ref(candidate):
                continue
            normalized_candidate = self._normalize_type_ref(
                candidate,
                line=line,
                column=column,
            )
            if normalized_candidate and normalized_candidate not in seen:
                normalized.append(normalized_candidate)
                seen.add(normalized_candidate)
        return normalized

    def _hover_value_type(self, node: ast.AST) -> Optional[str]:
        candidates = self._hover_type_candidates(node)
        if candidates:
            return candidates[0]
        return None

    def _infer_name_value_type(self, node: ast.Name, enclosing_symbol: str) -> Optional[str]:
        import_target = self._stable_symbol_from_import_expr(node, kind_hint=SymbolKind.Type)
        import_type = self._symbol_id_to_receiver_type(import_target)
        if import_type:
            return import_type

        resolved_type = self._resolve_symbol_at_with_hint(
            node.lineno,
            node.col_offset,
            kind_hint=SymbolKind.Type,
        )
        resolved_type = self._prefer_import_fallback(resolved_type, node, kind_hint=SymbolKind.Type)
        resolved_type = self._symbol_id_to_receiver_type(resolved_type)
        if resolved_type:
            return resolved_type

        enc_def = self.definition_index.by_symbol_id.get(enclosing_symbol)
        if enc_def and enc_def.kind == SymbolKind.Function and hasattr(enc_def.details, "parameters"):
            param = next((p for p in enc_def.details.parameters if p.name == node.id), None)
            if param and param.param_type:
                return self._normalize_type_ref(
                    param.param_type,
                    line=node.lineno - 1,
                    column=getattr(node, "col_offset", 0) or 0,
                )

        if node.id == "self" and "." in enclosing_symbol:
            return self._find_type_for_enclosing_symbol(enclosing_symbol)
        if node.id == "cls" and "." in enclosing_symbol:
            return self._find_type_for_enclosing_symbol(enclosing_symbol)

        local_type = self._infer_local_name_type(node.id, node, enclosing_symbol)
        if local_type:
            return local_type

        return self._hover_value_type(node)

    def _infer_call_value_type(
        self,
        node: ast.Call,
        enclosing_symbol: str,
    ) -> Optional[str]:
        target_sym: str | None = None
        if isinstance(node.func, ast.Name):
            target_sym = self._resolve_symbol_at_with_hint(
                node.func.lineno,
                node.func.col_offset,
                kind_hint=SymbolKind.Function,
            )
            if not target_sym:
                target_sym = self._resolve_symbol_at_with_hint(
                    node.func.lineno,
                    node.func.col_offset,
                    follow_imports=True,
                    kind_hint=SymbolKind.Function,
                )
            if not target_sym:
                target_sym = self._fallback_imported_symbol(node.func.id)
            target_sym = self._prefer_import_fallback(target_sym, node.func, kind_hint=SymbolKind.Function)
            return_type = self._function_return_type(
                target_sym,
                line=node.lineno - 1,
                column=getattr(node, "col_offset", 0) or 0,
            )
            if return_type:
                return return_type
            result_type = self._symbol_id_to_receiver_type(target_sym)
            if result_type:
                return result_type
            imported_type = self._infer_name_value_type(node.func, enclosing_symbol)
            if imported_type:
                return imported_type
            return self._hover_value_type(node)

        if not isinstance(node.func, ast.Attribute):
            return None

        target_sym = self._resolve_symbol_for_node_with_hint(node.func, kind_hint=SymbolKind.Function)
        if not target_sym:
            target_sym = self._resolve_symbol_for_node_with_hint(
                node.func,
                follow_imports=True,
                kind_hint=SymbolKind.Function,
            )
        target_sym = self._prefer_import_fallback(target_sym, node.func, kind_hint=SymbolKind.Function)
        inferred_method_target: str | None = None
        inferred_receiver_type = self._infer_receiver_type_symbol(node.func.value, enclosing_symbol)
        if inferred_receiver_type:
            inferred_method_target = f"{inferred_receiver_type}.{node.func.attr}"
        if inferred_method_target and not self._function_return_type(
            target_sym,
            line=node.lineno - 1,
            column=getattr(node, "col_offset", 0) or 0,
        ):
            target_sym = inferred_method_target
        return_type = self._function_return_type(
            target_sym,
            line=node.lineno - 1,
            column=getattr(node, "col_offset", 0) or 0,
        )
        if return_type:
            return return_type
        result_type = self._value_type_from_symbol(
            target_sym,
            line=node.lineno - 1,
            column=getattr(node, "col_offset", 0) or 0,
        )
        if result_type:
            return result_type
        return self._hover_value_type(node)

    def _infer_expr_value_type(self, expr: ast.AST, enclosing_symbol: str) -> Optional[str]:
        if isinstance(expr, ast.Constant):
            return self._builtin_receiver_type_from_constant(expr.value)
        if isinstance(expr, ast.JoinedStr):
            return "builtins.str"
        if isinstance(expr, (ast.List, ast.ListComp)):
            return "builtins.list"
        if isinstance(expr, (ast.Tuple, ast.GeneratorExp)):
            return "builtins.tuple"
        if isinstance(expr, (ast.Set, ast.SetComp)):
            return "builtins.set"
        if isinstance(expr, (ast.Dict, ast.DictComp)):
            return "builtins.dict"
        if isinstance(expr, ast.Name):
            return self._infer_name_value_type(expr, enclosing_symbol)
        if isinstance(expr, ast.Call):
            return self._infer_call_value_type(expr, enclosing_symbol)
        if isinstance(expr, ast.Attribute):
            target_sym = self._resolve_symbol_for_node_with_hint(expr, kind_hint=SymbolKind.Variable)
            target_sym = self._prefer_import_fallback(target_sym, expr, kind_hint=SymbolKind.Variable)
            resolved_type = self._value_type_from_symbol(
                target_sym,
                line=expr.lineno - 1,
                column=getattr(expr, "col_offset", 0) or 0,
            )
            if resolved_type:
                return resolved_type

            base_type = self._infer_expr_value_type(expr.value, enclosing_symbol)
            if not base_type:
                return self._hover_value_type(expr)

            member_sym = f"{base_type}.{expr.attr}"
            member_type = self._value_type_from_symbol(
                member_sym,
                line=expr.lineno - 1,
                column=getattr(expr, "col_offset", 0) or 0,
            )
            if member_type:
                return member_type
            return self._hover_value_type(expr)
        if isinstance(expr, ast.BinOp):
            left_type = self._infer_expr_value_type(expr.left, enclosing_symbol)
            right_type = self._infer_expr_value_type(expr.right, enclosing_symbol)
            if isinstance(expr.op, ast.Div) and left_type in _PATHLIKE_RECEIVER_TYPES:
                return left_type
            if left_type and left_type == right_type and isinstance(expr.op, (ast.Add, ast.BitOr, ast.Mod)):
                return left_type
            return left_type or right_type
        if isinstance(expr, ast.IfExp):
            body_type = self._infer_expr_value_type(expr.body, enclosing_symbol)
            orelse_type = self._infer_expr_value_type(expr.orelse, enclosing_symbol)
            if body_type and body_type == orelse_type:
                return body_type
            return body_type or orelse_type
        return self._hover_value_type(expr)

    def _infer_name_receiver_type(self, node: ast.Name, enclosing_symbol: str) -> Optional[str]:
        return self._infer_name_value_type(node, enclosing_symbol)

    def _infer_call_result_type(
        self,
        node: ast.Call,
        enclosing_symbol: str,
    ) -> Optional[str]:
        return self._infer_call_value_type(node, enclosing_symbol)

    def _infer_receiver_type_symbol(self, expr: ast.AST, enclosing_symbol: str) -> Optional[str]:
        return self._infer_expr_value_type(expr, enclosing_symbol)

    def _fallback_method_target_from_receiver_type(
        self,
        node: ast.Attribute,
        enclosing_symbol: str,
    ) -> Optional[str]:
        receiver_type = self._infer_receiver_type_symbol(node.value, enclosing_symbol)
        if not receiver_type:
            return None
        target_sym = f"{receiver_type}.{node.attr}"
        if (
            not target_sym.startswith("builtins.")
            and target_sym not in self.definition_index.by_symbol_id
            and target_sym not in self.external_symbols
        ):
            self._collect_external_symbol(
                target_sym,
                self._synthetic_target(target_sym, node, kind="function"),
                kind_hint=SymbolKind.Function,
            )
        return target_sym

    def _fallback_attribute_target_from_receiver_type(
        self,
        node: ast.Attribute,
        enclosing_symbol: str,
    ) -> Optional[str]:
        receiver_type = self._infer_receiver_type_symbol(node.value, enclosing_symbol)
        if not receiver_type:
            return None

        target_sym = f"{receiver_type}.{node.attr}"
        if target_sym in self.definition_index.by_symbol_id or target_sym in self.external_symbols:
            return target_sym

        if receiver_type in self.definition_index.type_by_symbol_id:
            return None

        self._ensure_external_symbol(target_sym, node, kind_hint=SymbolKind.Variable)
        return target_sym

    def _narrow_by_import(self, name: str, candidates: list[SymbolDefinition]) -> Optional[str]:
        if self.tree is None:
            return None
        cache_key = (name, tuple(candidate.symbol_id for candidate in candidates))
        if cache_key in self._narrow_by_import_cache:
            return self._narrow_by_import_cache[cache_key]

        self._all_import_bindings()
        for binding in (self._import_bindings_by_name or {}).get(name, []):
            if "." not in binding.qualified_name:
                continue
            prefix, _, original_name = binding.qualified_name.rpartition(".")
            matched = [
                candidate
                for candidate in candidates
                if candidate.name == original_name
                and (
                    candidate.symbol_id.startswith(prefix + ".")
                    or candidate.symbol_id == prefix
                )
            ]
            if len(matched) == 1:
                self._narrow_by_import_cache[cache_key] = matched[0].symbol_id
                return matched[0].symbol_id
        self._narrow_by_import_cache[cache_key] = None
        return None

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        enc = self._enclosing_symbol(node)
        if self._func_spans:
            enclosing_symbol = self._enclosing_symbol_defined(node)
            for dec in node.decorator_list:
                self._visit_decorator(dec, enclosing_symbol)
            for default in node.args.defaults or []:
                self._collect_read_ref_from_expr(default, enclosing_symbol)
            for kw_default in node.args.kw_defaults or []:
                if kw_default is not None:
                    self._collect_read_ref_from_expr(kw_default, enclosing_symbol)
            self.generic_visit(node)
            return

        func_id = f"{enc}.{node.name}"
        exact = self.definition_index.by_symbol_id.get(func_id)
        if not exact:
            for definition in self.definition_index.by_file_and_name.get((self.rel_path, node.name), []):
                if definition.location.line != node.lineno - 1:
                    continue
                if definition.symbol_id.count(".") >= func_id.count("."):
                    func_id = definition.symbol_id
                break
        start = node.lineno
        end = getattr(node, "end_lineno", node.lineno)
        self._func_spans.append((start, end, func_id))
        for dec in node.decorator_list:
            self._visit_decorator(dec, func_id)
        for default in node.args.defaults or []:
            self._collect_read_ref_from_expr(default, func_id)
        for kw_default in node.args.kw_defaults or []:
            if kw_default is not None:
                self._collect_read_ref_from_expr(kw_default, func_id)
        self.generic_visit(node)
        self._func_spans.pop()

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self.visit_FunctionDef(node)

    def _visit_decorator(self, node: ast.expr, decorated_symbol_id: str) -> None:
        target_sym = None
        if isinstance(node, ast.Name):
            target_sym = self._resolve_symbol_for_node_with_hint(node, kind_hint=SymbolKind.Function)
        elif isinstance(node, ast.Attribute):
            target_sym = self._resolve_symbol_for_node_with_hint(node, kind_hint=SymbolKind.Function)
        target_sym = self._prefer_import_fallback(target_sym, node, kind_hint=SymbolKind.Function)
        if not target_sym and isinstance(node, ast.Name) and node.id in _BUILTIN_BEHAVIORAL_DECORATORS:
            target_sym = self._builtin_external_symbol_id(node.id, kind=SymbolKind.Function, node=node)
        self._ensure_external_symbol(target_sym, node, kind_hint=SymbolKind.Function)
        if target_sym:
            self._append_reference(
                target_symbol=target_sym,
                location=self._loc(node),
                enclosing_symbol=decorated_symbol_id,
                role=ReferenceRole.Decorate,
            )

    def visit_ExceptHandler(self, node: ast.ExceptHandler) -> None:
        enc = self._enclosing_symbol(node)
        if enc and node.type:

            def _extract_type(t_node: ast.AST) -> None:
                if isinstance(t_node, (ast.Name, ast.Attribute)):
                    target_sym = self._resolve_symbol_for_node_with_hint(t_node, kind_hint=SymbolKind.Type)
                    if target_sym:
                        self._append_reference(
                            target_symbol=target_sym,
                            location=self._loc(t_node),
                            enclosing_symbol=self._enclosing_symbol_defined(node),
                            role=ReferenceRole.Read,
                        )
                elif isinstance(t_node, ast.Tuple):
                    for elt in t_node.elts:
                        _extract_type(elt)

            _extract_type(node.type)
        self.generic_visit(node)

    def _resolve_super_call_target(self, enc: str, method_name: str) -> Optional[str]:
        parts = enc.split(".")
        if len(parts) < 2:
            return None
        class_symbol_id = ".".join(parts[:-1])
        type_def = self.definition_index.type_by_symbol_id.get(class_symbol_id)
        if not type_def or not isinstance(type_def.details, TypeDetails):
            return None
        bases = type_def.details.inherits
        if not bases:
            return None
        base_ref = bases[0]
        base_name = base_ref.split(".")[-1].strip()
        class_module_prefix = class_symbol_id.rsplit(".", 1)[0] if "." in class_symbol_id else ""
        base_full = f"{class_module_prefix}.{base_name}" if class_module_prefix else base_name
        base_type = next(
            (
                definition
                for definition in self._type_candidates(base_name)
                if definition.symbol_id == base_full
                or (not class_module_prefix or definition.symbol_id.startswith(class_module_prefix + "."))
            ),
            None,
        )
        if not base_type:
            cross_module = self._type_candidates(base_name)
            if len(cross_module) == 1:
                base_type = cross_module[0]
        if not base_type and self.tree is not None and type_def.location.file_path == self.rel_path:
            class_line = type_def.location.line
            class_def_node = next(
                (
                    n for n in ast.walk(self.tree)
                    if isinstance(n, ast.ClassDef) and n.lineno - 1 == class_line
                ),
                None,
            )
            if class_def_node:
                for base_node in class_def_node.bases:
                    node_str = ast.unparse(base_node)
                    if node_str == base_ref or node_str.split(".")[-1] == base_name:
                        line, column = self._query_position(base_node)
                        defs = self._resolve_at(line, column, follow_imports=True)
                        if defs:
                            canon_id = definition_symbol_id_from_target(self.definition_index, defs[0])
                            if canon_id:
                                base_type = self.definition_index.type_by_symbol_id.get(canon_id)
                                if base_type:
                                    break
                            if not base_type and defs[0].name:
                                jedi_name = defs[0].name
                                base_type = next(
                                    (
                                        definition
                                        for definition in self._type_candidates(jedi_name)
                                        if definition.symbol_id != class_symbol_id
                                    ),
                                    None,
                                )
                                if base_type:
                                    break
        if not base_type:
            return None
        return f"{base_type.symbol_id}.{method_name}"

    def visit_Call(self, node: ast.Call) -> None:
        enc = self._enclosing_symbol(node)
        if not enc:
            self.generic_visit(node)
            return
        if self._is_inside_annotation(node):
            return
        if self._is_builtin_intrinsic_call(node):
            for arg in node.args:
                self.visit(arg)
            for keyword in node.keywords:
                self.visit(keyword.value)
            return
        target_sym = None
        receiver_sym = None
        method_name = None
        is_super_method_call = False

        if (
            isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Call)
            and isinstance(node.func.value.func, ast.Name)
            and node.func.value.func.id == "super"
        ):
            is_super_method_call = True
            method_name = node.func.attr
            target_sym = self._resolve_super_call_target(enc, method_name)

        if target_sym is None and isinstance(node.func, ast.Name):
            if node.func.id == "cls" and "." in enc:
                class_symbol_id = ".".join(enc.split(".")[:-1])
                target_sym = f"{class_symbol_id}.__init__"
            else:
                target_sym = self._resolve_symbol_for_node_with_hint(node.func, kind_hint=SymbolKind.Function)
                if not target_sym:
                    target_sym = self._resolve_symbol_for_node_with_hint(
                        node.func,
                        follow_imports=True,
                        kind_hint=SymbolKind.Function,
                    )
                if not target_sym:
                    target_sym = self._fallback_imported_symbol(node.func.id)
        elif target_sym is None and isinstance(node.func, ast.Attribute):
            method_name = node.func.attr
            target_sym = self._resolve_symbol_for_node_with_hint(node.func, kind_hint=SymbolKind.Function)
            if not target_sym:
                target_sym = self._resolve_symbol_for_node_with_hint(
                    node.func,
                    follow_imports=True,
                    kind_hint=SymbolKind.Function,
                )

            if not target_sym and isinstance(node.func.value, ast.Name):
                receiver_name = node.func.value.id
                enc_def = self.definition_index.by_symbol_id.get(enc)
                if enc_def and enc_def.kind == SymbolKind.Function and hasattr(enc_def.details, "parameters"):
                    param = next((p for p in enc_def.details.parameters if p.name == receiver_name), None)
                    if param and param.param_type:
                        ptype = self._normalize_type_ref(
                            param.param_type,
                            line=node.func.value.lineno - 1,
                            column=getattr(node.func.value, "col_offset", 0) or 0,
                        )
                        ptype_short = ptype.split(".")[-1] if ptype else ""
                        type_def = next(iter(self._type_candidates(ptype_short)), None)
                        if type_def:
                            target_sym = f"{type_def.symbol_id}.{method_name}"
                        elif ptype:
                            target_sym = f"{ptype}.{method_name}"
                    elif receiver_name == "self" and "." in enc:
                        class_prefix = self._find_type_for_enclosing_symbol(enc)
                        if class_prefix:
                            target_sym = f"{class_prefix}.{method_name}"
                        else:
                            parts = enc.split(".")
                            enclosing_class = ".".join(parts[:-1]) if len(parts) >= 2 else enc
                            target_sym = f"{enclosing_class}.{method_name}"
                    elif receiver_name == "cls" and "." in enc:
                        class_prefix = self._find_type_for_enclosing_symbol(enc)
                        if class_prefix:
                            target_sym = f"{class_prefix}.{method_name}"
                        else:
                            parts = enc.split(".")
                            enclosing_class = ".".join(parts[:-1]) if len(parts) >= 2 else enc
                            target_sym = f"{enclosing_class}.{method_name}"
                elif receiver_name == "self" and "." in enc:
                    class_prefix = self._find_type_for_enclosing_symbol(enc)
                    if class_prefix:
                        target_sym = f"{class_prefix}.{method_name}"
                    else:
                        parts = enc.split(".")
                        enclosing_class = ".".join(parts[:-1]) if len(parts) >= 2 else enc
                        target_sym = f"{enclosing_class}.{method_name}"
                elif receiver_name == "cls" and "." in enc:
                    class_prefix = self._find_type_for_enclosing_symbol(enc)
                    if class_prefix:
                        target_sym = f"{class_prefix}.{method_name}"
                    else:
                        parts = enc.split(".")
                        enclosing_class = ".".join(parts[:-1]) if len(parts) >= 2 else enc
                        target_sym = f"{enclosing_class}.{method_name}"

            if (
                target_sym
                and "." not in target_sym
                and isinstance(node.func.value, ast.Name)
                and node.func.value.id in ("self", "cls")
                and enc
            ):
                class_prefix = self._find_type_for_enclosing_symbol(enc)
                if class_prefix:
                    target_sym = f"{class_prefix}.{method_name}"
                else:
                    parts = enc.split(".")
                    if len(parts) >= 2:
                        enclosing_class = ".".join(parts[:-1])
                        target_sym = f"{enclosing_class}.{method_name}"

            receiver_type = self._symbol_id_to_receiver_type(target_sym)
            inferred_receiver_type = self._infer_receiver_type_symbol(node.func.value, enc)
            if inferred_receiver_type and (
                receiver_type is None
                or target_sym == receiver_type
                or not (target_sym or "").endswith(f".{method_name}")
            ):
                receiver_type = inferred_receiver_type
            if receiver_type:
                target_sym = f"{receiver_type}.{method_name}"

            if not target_sym:
                target_sym = self._fallback_method_target_from_receiver_type(node.func, enc)

            if isinstance(node.func.value, ast.Name):
                receiver_sym = self._resolve_internal_variable_at(
                    node.func.value.lineno,
                    node.func.value.col_offset,
                )

        target_sym = self._prefer_import_fallback(target_sym, node.func, kind_hint=SymbolKind.Function)
        self._ensure_external_symbol(target_sym, node.func, kind_hint=SymbolKind.Function)

        assigned_to = None
        parent = getattr(node, "parent", None)
        if isinstance(parent, ast.Assign) and parent.value == node:
            for t in parent.targets:
                if isinstance(t, ast.Name):
                    definition = self.definition_index.by_file_line_name.get((self.rel_path, parent.lineno - 1, t.id))
                    if definition and definition.kind == SymbolKind.Variable:
                        assigned_to = definition.symbol_id
                break
        self._append_reference(
            target_symbol=target_sym,
            location=self._loc(node),
            enclosing_symbol=self._enclosing_symbol_defined(node),
            role=ReferenceRole.Call,
            receiver=receiver_sym,
            method_name=method_name,
            assigned_to=assigned_to,
        )
        if is_super_method_call:
            for arg in node.args:
                self.visit(arg)
            for keyword in node.keywords:
                self.visit(keyword.value)
            return
        self.generic_visit(node)

    def visit_Name(self, node: ast.Name) -> None:
        enc = self._enclosing_symbol(node)
        if not enc:
            self.generic_visit(node)
            return
        if self._is_inside_default_value(node):
            self.generic_visit(node)
            return
        if self._is_inside_annotation(node):
            return
        if self._is_inside_type_alias_value(node):
            return
        if self._is_decorator_expr(node):
            return
        if self._is_call_callee(node):
            return
        if isinstance(node.ctx, ast.Load):
            target_sym = self._resolve_symbol_at_with_hint(
                node.lineno,
                node.col_offset,
                kind_hint=SymbolKind.Variable,
            )
            if not target_sym:
                target_sym = self._resolve_symbol_at_with_hint(
                    node.lineno,
                    node.col_offset,
                    follow_imports=True,
                    kind_hint=SymbolKind.Variable,
                )
            target_sym = self._prefer_import_fallback(target_sym, node, kind_hint=SymbolKind.Variable)
            self._ensure_external_symbol(target_sym, node, kind_hint=SymbolKind.Variable)
            if target_sym and self._is_read_target(target_sym):
                self._append_reference(
                    target_symbol=target_sym,
                    location=self._loc(node),
                    enclosing_symbol=self._enclosing_symbol_defined(node),
                    role=ReferenceRole.Read,
                )
        elif isinstance(node.ctx, ast.Store):
            target_sym = self._resolve_symbol_at_with_hint(
                node.lineno,
                node.col_offset,
                kind_hint=SymbolKind.Variable,
            )
            if not target_sym and not self._func_spans:
                target_sym = self._same_module_variable_symbol(node.id)
            target_sym = self._prefer_import_fallback(target_sym, node, kind_hint=SymbolKind.Variable)
            self._ensure_external_symbol(target_sym, node, kind_hint=SymbolKind.Variable)
            if target_sym and self._is_write_target(target_sym):
                self._append_reference(
                    target_symbol=target_sym,
                    location=self._loc(node),
                    enclosing_symbol=self._enclosing_symbol_defined(node),
                    role=ReferenceRole.Write,
                )
        self.generic_visit(node)

    def visit_AugAssign(self, node: ast.AugAssign) -> None:
        enc = self._enclosing_symbol(node)
        if not enc:
            self.generic_visit(node)
            return
        target_sym = None
        receiver_sym = None
        if isinstance(node.target, ast.Name):
            target_sym = self._resolve_symbol_at_with_hint(
                node.target.lineno,
                node.target.col_offset,
                kind_hint=SymbolKind.Variable,
            )
            if target_sym:
                target_sym = self._prefer_same_module_variable(target_sym, node.target.id)
            else:
                target_sym = self._same_module_variable_symbol(node.target.id)
        elif isinstance(node.target, ast.Attribute):
            target_sym = self._resolve_symbol_for_node_with_hint(node.target, kind_hint=SymbolKind.Variable)
            if not target_sym:
                target_sym = self._fallback_attribute_target_from_receiver_type(node.target, enc)
            if target_sym:
                target_sym = self._prefer_same_module_variable(target_sym, node.target.attr)
                self._ensure_external_symbol(target_sym, node.target, kind_hint=SymbolKind.Variable)
            if isinstance(node.target.value, ast.Name):
                receiver_sym = self._resolve_internal_variable_at(
                    node.target.value.lineno,
                    node.target.value.col_offset,
                )
        if target_sym and self._is_write_target(target_sym):
            loc = self._loc(node.target)
            enc_def = self._enclosing_symbol_defined(node)
            self._append_reference(
                target_symbol=target_sym,
                location=loc,
                enclosing_symbol=enc_def,
                role=ReferenceRole.Read,
                receiver=receiver_sym,
            )
            self._append_reference(
                target_symbol=target_sym,
                location=loc,
                enclosing_symbol=enc_def,
                role=ReferenceRole.Write,
                receiver=receiver_sym,
            )
        self.visit(node.value)

    def visit_Attribute(self, node: ast.Attribute) -> None:
        enc = self._enclosing_symbol(node)
        if not enc:
            self.generic_visit(node)
            return
        if self._is_inside_default_value(node):
            self.generic_visit(node)
            return
        if self._is_inside_annotation(node):
            return
        if self._is_inside_type_alias_value(node):
            return
        if self._is_call_callee(node):
            self.visit(node.value)
            return
        if isinstance(node.ctx, ast.Load):
            target_sym = self._resolve_symbol_for_node_with_hint(node, kind_hint=SymbolKind.Variable)
            if not target_sym:
                target_sym = self._resolve_symbol_for_node_with_hint(
                    node,
                    follow_imports=True,
                    kind_hint=SymbolKind.Variable,
                )
            target_sym = self._prefer_import_fallback(target_sym, node, kind_hint=SymbolKind.Variable)
            if not target_sym:
                target_sym = self._fallback_attribute_target_from_receiver_type(node, enc)
            self._ensure_external_symbol(target_sym, node, kind_hint=SymbolKind.Variable)
            if target_sym and self._is_read_target(target_sym):
                receiver_sym = None
                if isinstance(node.value, ast.Name):
                    receiver_sym = self._resolve_internal_variable_at(
                        node.value.lineno,
                        node.value.col_offset,
                    )
                self._append_reference(
                    target_symbol=target_sym,
                    location=self._loc(node),
                    enclosing_symbol=self._enclosing_symbol_defined(node),
                    role=ReferenceRole.Read,
                    receiver=receiver_sym,
                )
        elif isinstance(node.ctx, ast.Store):
            target_sym = self._resolve_symbol_for_node_with_hint(node, kind_hint=SymbolKind.Variable)
            if not target_sym:
                target_sym = self._fallback_attribute_target_from_receiver_type(node, enc)
            target_sym = self._prefer_import_fallback(target_sym, node, kind_hint=SymbolKind.Variable)
            self._ensure_external_symbol(target_sym, node, kind_hint=SymbolKind.Variable)
            if target_sym and self._is_write_target(target_sym):
                receiver_sym = None
                if isinstance(node.value, ast.Name):
                    receiver_sym = self._resolve_internal_variable_at(
                        node.value.lineno,
                        node.value.col_offset,
                    )
                self._append_reference(
                    target_symbol=target_sym,
                    location=self._loc(node),
                    enclosing_symbol=self._enclosing_symbol_defined(node),
                    role=ReferenceRole.Write,
                    receiver=receiver_sym,
                )
        self.generic_visit(node)


def collect_references(
    doc: DocumentSemantics,
    file_path: str,
    source: str,
    project_root: str,
    all_definitions: list[SymbolDefinition],
    module_symbol_id: str,
    resolver: DocumentResolver,
    definition_index: DefinitionIndex | None = None,
) -> tuple[list[SymbolReference], list[SymbolDefinition]]:
    """Run reference collection with the provided resolver on a single document."""
    abs_path = os.path.join(project_root, doc.relative_path)
    collector = ReferenceCollector(
        file_path=abs_path,
        source=source,
        project_root=project_root,
        all_definitions=all_definitions,
        definition_index=definition_index or DefinitionIndex.build(all_definitions),
        module_symbol_id=module_symbol_id,
        resolver=resolver,
    )
    tree = ast.parse(source, filename=abs_path)
    add_parents(tree)
    collector.tree = tree
    collector.visit(tree)
    return collector.references, list(collector.external_symbols.values())
