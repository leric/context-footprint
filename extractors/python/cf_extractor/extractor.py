"""
Pass 1: AST-based extraction of definitions (Types, Functions, Variables).
Only module-level and class-level symbols; no local variables.
"""

import ast
import os
from pathlib import Path
from typing import Optional

from .schema import (
    DocumentSemantics,
    FunctionDetails,
    FunctionModifiers,
    Mutability,
    Parameter,
    SourceLocation,
    SourceSpan,
    SymbolDefinition,
    SymbolKind,
    TypeDetails,
    TypeField,
    TypeKind,
    VariableDetails,
    VariableScope,
    Visibility,
)


def _visibility_from_name(name: str) -> Visibility:
    if name.startswith("__") and not name.endswith("__"):
        return Visibility.Private
    if name.startswith("_"):
        return Visibility.Private
    return Visibility.Public


def _mutability_from_name(name: str) -> Mutability:
    if not name:
        return Mutability.Mutable
    if name.isupper():
        return Mutability.Const
    if name[0].isupper() and any(c.islower() for c in name):
        return Mutability.Immutable
    return Mutability.Mutable


def _span_from_node(node: ast.AST, source_lines: list[str]) -> SourceSpan:
    """Build 0-based inclusive start, exclusive end span. AST uses 1-based lines."""
    start_line = node.lineno - 1
    start_col = getattr(node, "col_offset", 0) or 0
    end_lineno = getattr(node, "end_lineno", node.lineno)
    end_col = getattr(node, "end_col_offset", start_col) or start_col
    return SourceSpan(
        start_line=start_line,
        start_column=start_col,
        end_line=end_lineno - 1,
        end_column=end_col,
    )


def _location_from_node(node: ast.AST, file_path: str) -> SourceLocation:
    return SourceLocation(
        file_path=file_path,
        line=node.lineno - 1,
        column=getattr(node, "col_offset", 0) or 0,
    )


def _function_fragment_spans(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    source_lines: list[str],
) -> tuple[list[SourceSpan], list[SourceSpan]]:
    """Split a function into contract surface and implementation source ranges."""
    first_body = node.body[0] if node.body else None
    signature_end_line = (
        max(node.lineno - 1, first_body.lineno - 2)
        if first_body is not None
        else node.lineno - 1
    )
    signature_start_node: ast.AST = (
        node.decorator_list[0] if node.decorator_list else node
    )
    signature_start_line = signature_start_node.lineno - 1
    if first_body is not None and first_body.lineno == node.lineno:
        signature_end_column = getattr(first_body, "col_offset", 0) or 0
    else:
        signature_end_column = (
            len(source_lines[signature_end_line])
            if 0 <= signature_end_line < len(source_lines)
            else 0
        )
    surface_spans = [
        SourceSpan(
            start_line=signature_start_line,
            start_column=getattr(signature_start_node, "col_offset", 0) or 0,
            end_line=signature_end_line,
            end_column=signature_end_column,
        )
    ]

    body = list(node.body)
    if (
        body
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
        and isinstance(body[0].value.value, str)
    ):
        surface_spans.append(_span_from_node(body.pop(0), source_lines))

    implementation_spans = [_span_from_node(statement, source_lines) for statement in body]
    return surface_spans, implementation_spans


def _extract_doc_from_annotation(node: ast.expr) -> list[str]:
    docs = []
    for child in ast.walk(node):
        if isinstance(child, ast.Call):
            func_id = None
            if isinstance(child.func, ast.Name):
                func_id = child.func.id
            elif isinstance(child.func, ast.Attribute):
                func_id = child.func.attr
            if func_id == "Doc" and child.args:
                arg = child.args[0]
                if isinstance(arg, ast.Constant) and isinstance(arg.value, str):
                    docs.append(arg.value)
    return docs


def _has_doc_in_annotations(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    """True if any parameter or return annotation contains Doc() (PEP 727 / Annotated-style)."""
    for arg in node.args.args + getattr(node.args, "kwonlyargs", []) + getattr(node.args, "posonlyargs", []):
        if arg.annotation and _extract_doc_from_annotation(arg.annotation):
            return True
    if node.args.vararg and node.args.vararg.annotation and _extract_doc_from_annotation(node.args.vararg.annotation):
        return True
    if node.args.kwarg and node.args.kwarg.annotation and _extract_doc_from_annotation(node.args.kwarg.annotation):
        return True
    if node.returns and _extract_doc_from_annotation(node.returns):
        return True
    return False


def _is_trivial_body(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    """True if function body is only `pass` or a single return (Annotated-style factory)."""
    if len(node.body) != 1:
        return False
    stmt = node.body[0]
    if isinstance(stmt, ast.Pass):
        return True
    if isinstance(stmt, ast.Return):
        return True
    return False


def _get_docstring(node: ast.AsyncFunctionDef | ast.FunctionDef | ast.ClassDef | ast.Module) -> list[str]:
    docs = []
    doc = ast.get_docstring(node)
    if doc:
        docs.append(doc)
        
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        for arg in node.args.args + getattr(node.args, "kwonlyargs", []) + getattr(node.args, "posonlyargs", []):
            if arg.annotation:
                docs.extend(_extract_doc_from_annotation(arg.annotation))
        if node.args.vararg and node.args.vararg.annotation:
            docs.extend(_extract_doc_from_annotation(node.args.vararg.annotation))
        if node.args.kwarg and node.args.kwarg.annotation:
            docs.extend(_extract_doc_from_annotation(node.args.kwarg.annotation))
        if node.returns:
            docs.extend(_extract_doc_from_annotation(node.returns))
            
    return docs


def _annotation_to_typeref(annotation: Optional[ast.expr]) -> Optional[str]:
    if annotation is None:
        return None
    if isinstance(annotation, ast.Constant) and isinstance(annotation.value, str):
        return annotation.value
    try:
        return ast.unparse(annotation)
    except Exception:
        return None


def _is_high_freedom_type(param_type: Optional[str]) -> bool:
    """Check if a type is a high-freedom type (built-in primitives or collections)."""
    if param_type is None:
        return True  # Untyped is high freedom
        
    # Strip Optional/Union things for a basic check, or just check basic strings
    pt = param_type.strip()
    
    # Common high-freedom type names
    high_freedom_primitives = {"str", "int", "float", "bool", "bytes", "complex", "Any"}
    high_freedom_collections = {"dict", "list", "set", "tuple", "Dict", "List", "Set", "Tuple", "Mapping", "Sequence", "Iterable"}
    
    # If the type is exactly a primitive
    if pt in high_freedom_primitives:
        return True
        
    # Check if it starts with a collection type (e.g. "dict", "Dict[str, Any]")
    # We split by '[' to get the base type
    base_type = pt.split("[")[0].strip()
    if base_type in high_freedom_collections:
        return True
        
    return False


def _class_span_excluding_methods(node: ast.ClassDef, source_lines: list[str]) -> SourceSpan:
    """Calculate class span ending just before the first method to exclude method bodies."""
    start_line = node.lineno - 1
    start_col = getattr(node, "col_offset", 0) or 0
    
    first_method_line = None
    for child in node.body:
        if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            first_method_line = child.lineno - 1
            break
            
    if first_method_line is not None:
        end_lineno = first_method_line
        end_col = len(source_lines[end_lineno - 1]) if end_lineno > 0 else 0
    else:
        end_lineno = getattr(node, "end_lineno", node.lineno)
        end_col = getattr(node, "end_col_offset", start_col) or start_col
        
    return SourceSpan(
        start_line=start_line,
        start_column=start_col,
        end_line=end_lineno,
        end_column=end_col,
    )


class DefinitionCollector(ast.NodeVisitor):
    """Collects Type, Function, and Variable definitions from a single file."""

    def __init__(self, file_path: str, source: str, module_symbol_id: str):
        self.file_path = file_path
        self.source = source
        self.source_lines = source.splitlines()
        self.module_symbol_id = module_symbol_id
        self.definitions: list[SymbolDefinition] = []
        self._class_stack: list[tuple[str, str, TypeDetails]] = []
        self._func_stack: list[str] = []

    def _current_scope_prefix(self) -> str:
        if not self._class_stack:
            return self.module_symbol_id
        return self._class_stack[-1][1]

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        type_id = f"{self._current_scope_prefix()}.{node.name}"
        bases: list[str] = []
        for b in node.bases:
            ref = _annotation_to_typeref(b)
            if ref:
                bases.append(ref)

        is_abstract = any(
            ast.get_source_segment(self.source, n) == "abstractmethod"
            for dec in node.decorator_list
            for n in [dec] if isinstance(dec, ast.Name)
        ) or (ast.get_source_segment(self.source, node) or "").find("ABC") >= 0 or "Protocol" in bases

        type_kind = TypeKind.Class
        if "Enum" in bases or any(b.endswith(".Enum") for b in bases):
            type_kind = TypeKind.Enum
        elif "Protocol" in bases or "ABC" in bases or is_abstract:
            type_kind = TypeKind.Interface

        type_details = TypeDetails(
            kind=type_kind,
            is_abstract=is_abstract,
            is_final=False,
            visibility=_visibility_from_name(node.name),
            type_params=[],
            fields=[],
            inherits=bases,
            implements=[],
        )
        span = _class_span_excluding_methods(node, self.source_lines)
        loc = _location_from_node(node, self.file_path)
        self.definitions.append(
            SymbolDefinition(
                symbol_id=type_id,
                kind=SymbolKind.Type,
                name=node.name,
                display_name=node.name,
                location=loc,
                span=span,
                enclosing_symbol=None if not self._class_stack else self._class_stack[-1][1],
                is_external=False,
                documentation=_get_docstring(node),
                details=type_details,
            )
        )
        self._class_stack.append((node.name, type_id, type_details))
        self.generic_visit(node)
        self._class_stack.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._visit_function_def(node, is_async=False)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._visit_function_def(node, is_async=True)

    def _visit_function_def(
        self, node: ast.FunctionDef | ast.AsyncFunctionDef, is_async: bool
    ) -> None:
        if self._func_stack:
            # Nested functions are not extracted as standalone definitions.
            # Their references are attributed to the enclosing function.
            self.generic_visit(node)
            return

        prefix = self._current_scope_prefix()
        func_id = f"{prefix}.{node.name}"
        params: list[Parameter] = []
        num_args = len(node.args.args)
        num_defaults = len(node.args.defaults) if node.args.defaults else 0
        def_start = num_args - num_defaults
        vararg_arg = getattr(node.args.vararg, "arg", None) if node.args.vararg else None
        for i, arg in enumerate(node.args.args):
            if arg.arg in ("self", "cls"):
                continue
            param_type = _annotation_to_typeref(getattr(arg, "annotation", None))
            has_default = i >= def_start
            is_variadic = arg.arg == vararg_arg
            is_high_freedom = _is_high_freedom_type(param_type)
            params.append(
                Parameter(
                    name=arg.arg,
                    param_type=param_type,
                    is_high_freedom_type=is_high_freedom,
                    has_default=has_default,
                    is_variadic=is_variadic,
                )
            )
        return_ann = getattr(node, "returns", None)
        return_types = []
        if return_ann:
            rt = _annotation_to_typeref(return_ann)
            if rt:
                return_types.append(rt)
        is_abstract = any(
            (isinstance(d, ast.Name) and d.id == "abstractmethod")
            or (isinstance(d, ast.Attribute) and getattr(d, "attr", None) == "abstractmethod")
            for d in node.decorator_list
        )
        is_static = any(
            isinstance(d, ast.Name) and d.id == "staticmethod"
            for d in node.decorator_list
        )
        is_classmethod = any(
            isinstance(d, ast.Name) and d.id == "classmethod"
            for d in node.decorator_list
        )
        is_constructor = node.name == "__init__"
        if is_constructor and not return_types:
            return_types = ["None"]

        # Annotated-style documented factory (e.g. Body(), Query()): Doc() in params/return + trivial body.
        # Use signature-only for context_size so CF does not inflate on doc-heavy signatures.
        use_signature_only_for_size = (
            _has_doc_in_annotations(node) and _is_trivial_body(node)
        )

        modifiers = FunctionModifiers(
            is_async=is_async,
            is_generator=any(isinstance(n, ast.Yield) for n in ast.walk(node)),
            is_static=is_static or is_classmethod,
            is_abstract=is_abstract,
            is_constructor=is_constructor,
            is_di_wired=False,
            use_signature_only_for_size=use_signature_only_for_size,
            visibility=_visibility_from_name(node.name),
        )
        span = _span_from_node(node, self.source_lines)
        surface_spans, implementation_spans = _function_fragment_spans(
            node, self.source_lines
        )
        loc = _location_from_node(node, self.file_path)
        enclosing = self._class_stack[-1][1] if self._class_stack else None
        self.definitions.append(
            SymbolDefinition(
                symbol_id=func_id,
                kind=SymbolKind.Function,
                name=node.name,
                display_name=node.name,
                location=loc,
                span=span,
                enclosing_symbol=enclosing,
                is_external=False,
                documentation=_get_docstring(node),
                details=FunctionDetails(
                    parameters=params,
                    return_types=return_types,
                    type_params=[],
                    surface_spans=surface_spans,
                    implementation_spans=implementation_spans,
                    modifiers=modifiers,
                ),
            )
        )
        self._func_stack.append(func_id)
        self.generic_visit(node)
        self._func_stack.pop()

    def visit_Assign(self, node: ast.Assign) -> None:
        for target in node.targets:
            if isinstance(target, ast.Name):
                if self._func_stack:
                    continue  # Local variable, ignore
                if self._class_stack:
                    scope = VariableScope.Field
                    prefix = self._class_stack[-1][1]
                    sym_id = f"{prefix}.{target.id}"
                    enclosing = prefix
                    self._class_stack[-1][2].fields.append(
                        TypeField(
                            name=target.id,
                            field_type=None,
                            mutability=_mutability_from_name(target.id),
                            visibility=_visibility_from_name(target.id),
                            symbol_id=sym_id,
                        )
                    )
                else:
                    scope = VariableScope.Global
                    sym_id = f"{self.module_symbol_id}.{target.id}"
                    enclosing = None
                span = _span_from_node(node, self.source_lines)
                loc = _location_from_node(node, self.file_path)
                self.definitions.append(
                    SymbolDefinition(
                        symbol_id=sym_id,
                        kind=SymbolKind.Variable,
                        name=target.id,
                        display_name=target.id,
                        location=loc,
                        span=span,
                        enclosing_symbol=enclosing,
                        is_external=False,
                        documentation=[],
                        details=VariableDetails(
                            var_type=None,
                            mutability=_mutability_from_name(target.id),
                            scope=scope,
                            visibility=_visibility_from_name(target.id),
                        ),
                    )
                )
            elif isinstance(target, ast.Attribute) and self._func_stack and self._class_stack:
                if isinstance(target.value, ast.Name) and target.value.id in ("self", "cls"):
                    scope = VariableScope.Field
                    prefix = self._class_stack[-1][1]
                    sym_id = f"{prefix}.{target.attr}"
                    enclosing = prefix
                    
                    if not any(f.name == target.attr for f in self._class_stack[-1][2].fields):
                        self._class_stack[-1][2].fields.append(
                            TypeField(
                                name=target.attr,
                                field_type=None,
                                mutability=_mutability_from_name(target.attr),
                                visibility=_visibility_from_name(target.attr),
                                symbol_id=sym_id,
                            )
                        )
                        span = _span_from_node(node, self.source_lines)
                        loc = _location_from_node(node, self.file_path)
                        self.definitions.append(
                            SymbolDefinition(
                                symbol_id=sym_id,
                                kind=SymbolKind.Variable,
                                name=target.attr,
                                display_name=target.attr,
                                location=loc,
                                span=span,
                                enclosing_symbol=enclosing,
                                is_external=False,
                                documentation=[],
                                details=VariableDetails(
                                    var_type=None,
                                    mutability=_mutability_from_name(target.attr),
                                    scope=scope,
                                    visibility=_visibility_from_name(target.attr),
                                ),
                            )
                        )
        self.generic_visit(node)

    def visit_AnnAssign(self, node: ast.AnnAssign) -> None:
        if isinstance(node.target, ast.Name):
            if self._func_stack:
                self.generic_visit(node)
                return  # Local variable, ignore
            var_type = _annotation_to_typeref(node.annotation)
            if self._class_stack:
                scope = VariableScope.Field
                prefix = self._class_stack[-1][1]
                sym_id = f"{prefix}.{node.target.id}"
                enclosing = prefix
                self._class_stack[-1][2].fields.append(
                    TypeField(
                        name=node.target.id,
                        field_type=var_type,
                        mutability=_mutability_from_name(node.target.id),
                        visibility=_visibility_from_name(node.target.id),
                        symbol_id=sym_id,
                    )
                )
            else:
                scope = VariableScope.Global
                sym_id = f"{self.module_symbol_id}.{node.target.id}"
                enclosing = None
            span = _span_from_node(node, self.source_lines)
            loc = _location_from_node(node, self.file_path)
            self.definitions.append(
                SymbolDefinition(
                    symbol_id=sym_id,
                    kind=SymbolKind.Variable,
                    name=node.target.id,
                    display_name=node.target.id,
                    location=loc,
                    span=span,
                    enclosing_symbol=enclosing,
                    is_external=False,
                    documentation=[],
                    details=VariableDetails(
                        var_type=var_type,
                        mutability=_mutability_from_name(node.target.id),
                        scope=scope,
                        visibility=_visibility_from_name(node.target.id),
                    ),
                )
            )
        elif isinstance(node.target, ast.Attribute) and self._func_stack and self._class_stack:
            if isinstance(node.target.value, ast.Name) and node.target.value.id in ("self", "cls"):
                var_type = _annotation_to_typeref(node.annotation)
                scope = VariableScope.Field
                prefix = self._class_stack[-1][1]
                sym_id = f"{prefix}.{node.target.attr}"
                enclosing = prefix
                
                if not any(f.name == node.target.attr for f in self._class_stack[-1][2].fields):
                    self._class_stack[-1][2].fields.append(
                        TypeField(
                            name=node.target.attr,
                            field_type=var_type,
                            mutability=_mutability_from_name(node.target.attr),
                            visibility=_visibility_from_name(node.target.attr),
                            symbol_id=sym_id,
                        )
                    )
                    span = _span_from_node(node, self.source_lines)
                    loc = _location_from_node(node, self.file_path)
                    self.definitions.append(
                        SymbolDefinition(
                            symbol_id=sym_id,
                            kind=SymbolKind.Variable,
                            name=node.target.attr,
                            display_name=node.target.attr,
                            location=loc,
                            span=span,
                            enclosing_symbol=enclosing,
                            is_external=False,
                            documentation=[],
                            details=VariableDetails(
                                var_type=var_type,
                                mutability=_mutability_from_name(node.target.attr),
                                scope=scope,
                                visibility=_visibility_from_name(node.target.attr),
                            ),
                        )
                    )
        self.generic_visit(node)


def extract_definitions_from_file(
    file_path: str, source: str, project_root: str
) -> DocumentSemantics:
    """Parse one Python file and return its definitions (no references yet)."""
    rel_path = os.path.relpath(file_path, project_root).replace("\\", "/")
    module_name = Path(rel_path).with_suffix("").as_posix().replace("/", ".")
    if module_name == ".":
        module_name = "__main__"
    module_symbol_id = module_name
    tree = ast.parse(source, filename=file_path)
    collector = DefinitionCollector(file_path=rel_path, source=source, module_symbol_id=module_symbol_id)
    collector.visit(tree)
    return DocumentSemantics(
        relative_path=rel_path,
        language="python",
        definitions=collector.definitions,
        references=[],
    )
