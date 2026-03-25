"""Basic tests for the Python extractor prototype."""

import ast
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

# Add package to path when running tests without install
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import cf_extractor.main as extractor_main
import cf_extractor.resolvers.lsp_common as lsp_common
from cf_extractor.main import find_python_files, run_extract, run_extract_with_metrics
from cf_extractor.reference_collector import ReferenceCollector
from cf_extractor.reference_index import DefinitionIndex, add_parents
from cf_extractor.resolvers.lsp_common import (
    LspProjectResolverBackend,
    guess_symbol_name,
    module_name_from_path,
)
from cf_extractor.schema import (
    FunctionDetails,
    ReferenceRole,
    SemanticData,
    SourceLocation,
    SourceSpan,
    SymbolDefinition,
    SymbolKind,
    )
from cf_extractor.schema import TypeDetails


FIXTURES_DIR = Path(__file__).resolve().parent / "fixtures"


def test_find_python_files_include_pattern():
    """Include filter restricts to matching paths only."""
    files = find_python_files(str(FIXTURES_DIR), include_tests=True, include=["simple.py"])
    assert files == ["simple.py"]


def test_guess_symbol_name_requires_cursor_inside_identifier(tmp_path: Path):
    file_path = tmp_path / "sample.pyi"
    file_path.write_text("# This is a comment\nvalue = 1\n", encoding="utf-8")
    assert guess_symbol_name(str(file_path), 0, 0) is None


def test_guess_symbol_name_reuses_cached_file_contents(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    lsp_common._cached_file_lines.cache_clear()
    lsp_common._cached_symbol_name.cache_clear()

    file_path = tmp_path / "sample.pyi"
    file_path.write_text("value = 1\n", encoding="utf-8")

    original_read_text = Path.read_text
    read_calls = 0

    def counting_read_text(self: Path, *args, **kwargs):
        nonlocal read_calls
        if self == file_path:
            read_calls += 1
        return original_read_text(self, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", counting_read_text)

    assert guess_symbol_name(str(file_path), 0, 1) == "value"
    assert guess_symbol_name(str(file_path), 0, 1) == "value"
    assert read_calls == 1


def test_location_to_target_reuses_cached_resolved_target(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    lsp_common._cached_file_lines.cache_clear()
    lsp_common._cached_symbol_name.cache_clear()

    file_path = tmp_path / "sample.py"
    file_path.write_text("value = 1\n", encoding="utf-8")

    backend = object.__new__(LspProjectResolverBackend)
    backend._resolved_target_cache = {}

    original_guess_symbol_name = lsp_common.guess_symbol_name
    guess_calls = 0

    def counting_guess_symbol_name(path: str | None, line: int, column: int):
        nonlocal guess_calls
        guess_calls += 1
        return original_guess_symbol_name(path, line, column)

    monkeypatch.setattr(lsp_common, "guess_symbol_name", counting_guess_symbol_name)

    location = {
        "uri": lsp_common.path_to_uri(str(file_path)),
        "range": {"start": {"line": 0, "character": 1}},
    }
    first = backend.location_to_target(location)
    second = backend.location_to_target(location)

    assert first is second
    assert first.name == "value"
    assert guess_calls == 1


def test_module_name_from_typeshed_path_uses_module_not_first_word():
    path = "/tmp/typeshed/stdlib/os/__init__.pyi"
    assert module_name_from_path(path) == "os"


def test_find_python_files_exclude_pattern():
    """Exclude filter skips matching paths."""
    all_files = find_python_files(str(FIXTURES_DIR), include_tests=True)
    files = find_python_files(str(FIXTURES_DIR), include_tests=True, exclude=["test_*"])
    assert len(files) < len(all_files)
    assert not any("test_" in f for f in files)
    assert "simple.py" in files


def test_find_python_files_include_and_exclude():
    """Include and exclude can be combined."""
    files = find_python_files(
        str(FIXTURES_DIR),
        include_tests=True,
        include=["*_call.py"],
        exclude=["test_nested*"],
    )
    assert "test_self_call.py" in files
    assert "test_nested_call.py" not in files


def test_run_extract_produces_valid_json():
    data = run_extract(str(FIXTURES_DIR))
    out = data.model_dump_json()
    parsed = json.loads(out)
    assert "project_root" in parsed
    assert "documents" in parsed
    assert isinstance(parsed["documents"], list)


def test_run_extract_with_metrics_reports_counts():
    data, metrics = run_extract_with_metrics(str(FIXTURES_DIR))
    assert data.documents
    assert metrics.resolver_backend == "ty"
    assert metrics.file_count >= 1
    assert metrics.definition_count >= 1
    assert metrics.reference_count >= 1
    assert metrics.resolved_reference_count >= 1
    assert metrics.total_seconds >= 0


def test_run_extract_with_metrics_builds_definition_index_once(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    (tmp_path / "a.py").write_text(
        """
def a():
    return 1
""".strip()
        + "\n",
        encoding="utf-8",
    )
    (tmp_path / "b.py").write_text(
        """
from a import a


def b():
    return a()
""".strip()
        + "\n",
        encoding="utf-8",
    )

    class DummyBackend:
        def open_document(self, file_path: str, source: str) -> object:
            return object()

        def close(self) -> None:
            return None

    build_calls = 0
    original_build = extractor_main.DefinitionIndex.build.__func__

    def counting_build(cls, definitions):
        nonlocal build_calls
        build_calls += 1
        return original_build(cls, definitions)

    monkeypatch.setattr(extractor_main.DefinitionIndex, "build", classmethod(counting_build))
    monkeypatch.setattr(extractor_main, "build_project_resolver_backend", lambda *args, **kwargs: DummyBackend())
    monkeypatch.setattr(extractor_main, "collect_references", lambda *args, **kwargs: ([], []))

    run_extract_with_metrics(str(tmp_path))

    assert build_calls == 1


def test_verbose_reference_warning_prints_traceback_and_timing(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
):
    (tmp_path / "sample.py").write_text(
        """
def sample():
    return 1
""".strip()
        + "\n",
        encoding="utf-8",
    )

    class DummyBackend:
        def open_document(self, file_path: str, source: str) -> object:
            return object()

        def close(self) -> None:
            return None

    def boom(*args, **kwargs):
        raise RecursionError("maximum recursion depth exceeded")

    monkeypatch.setattr(extractor_main, "build_project_resolver_backend", lambda *args, **kwargs: DummyBackend())
    monkeypatch.setattr(extractor_main, "collect_references", boom)

    run_extract_with_metrics(str(tmp_path), verbose=True)
    captured = capsys.readouterr()

    assert "Warning: references sample.py: maximum recursion depth exceeded" in captured.err
    assert "Traceback (most recent call last):" in captured.err
    assert "[2/2] References failed (1/1): sample.py" in captured.err
    assert "open_document=" in captured.err
    assert "collect_references=" in captured.err


def test_narrow_by_import_reuses_cached_import_bindings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    file_path = tmp_path / "sample.py"
    source = "from pkg.ops import CreateModel\nCreateModel()\nCreateModel()\n"
    file_path.write_text(source, encoding="utf-8")

    class DummyResolver:
        def goto(self, line: int, column: int, *, follow_imports: bool = False):
            return []

        def references(self, line: int, column: int):
            return []

        def document_symbols(self):
            return []

        def workspace_symbols(self, query: str):
            return []

        def hover(self, line: int, column: int):
            return []

    candidates = [
        SymbolDefinition(
            symbol_id="pkg.ops.models.CreateModel",
            kind=SymbolKind.Function,
            name="CreateModel",
            display_name="CreateModel",
            location=SourceLocation(file_path="pkg/ops/models.py", line=0, column=0),
            span=SourceSpan(start_line=0, start_column=0, end_line=1, end_column=0),
            details=FunctionDetails(),
        ),
        SymbolDefinition(
            symbol_id="other.models.CreateModel",
            kind=SymbolKind.Function,
            name="CreateModel",
            display_name="CreateModel",
            location=SourceLocation(file_path="other/models.py", line=0, column=0),
            span=SourceSpan(start_line=0, start_column=0, end_line=1, end_column=0),
            details=FunctionDetails(),
        ),
    ]
    collector = ReferenceCollector(
        file_path=str(file_path),
        source=source,
        project_root=str(tmp_path),
        all_definitions=candidates,
        definition_index=DefinitionIndex.build(candidates),
        module_symbol_id="sample",
        resolver=DummyResolver(),
    )
    collector.tree = ast.parse(source, filename=str(file_path))

    original_sorted_import_nodes = collector._sorted_import_nodes
    sorted_import_nodes_calls = 0

    def counting_sorted_import_nodes():
        nonlocal sorted_import_nodes_calls
        sorted_import_nodes_calls += 1
        return original_sorted_import_nodes()

    monkeypatch.setattr(collector, "_sorted_import_nodes", counting_sorted_import_nodes)

    assert collector._narrow_by_import("CreateModel", candidates) == "pkg.ops.models.CreateModel"
    assert collector._narrow_by_import("CreateModel", candidates) == "pkg.ops.models.CreateModel"
    assert sorted_import_nodes_calls == 1


def test_infer_local_name_type_reuses_function_assignment_index(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    file_path = tmp_path / "sample.py"
    source = """
class Builder:
    pass


def sample():
    value: Builder = builder()
    first = value
    second = value
""".strip() + "\n"
    file_path.write_text(source, encoding="utf-8")

    class DummyResolver:
        def goto(self, line: int, column: int, *, follow_imports: bool = False):
            return []

        def references(self, line: int, column: int):
            return []

        def document_symbols(self):
            return []

        def workspace_symbols(self, query: str):
            return []

        def hover(self, line: int, column: int):
            return []

    builder_definition = SymbolDefinition(
        symbol_id="sample.builder",
        kind=SymbolKind.Function,
        name="builder",
        display_name="builder",
        location=SourceLocation(file_path="sample.py", line=3, column=0),
        span=SourceSpan(start_line=3, start_column=0, end_line=4, end_column=0),
        details=FunctionDetails(return_types=["sample.Builder"]),
    )
    builder_type_definition = SymbolDefinition(
        symbol_id="sample.Builder",
        kind=SymbolKind.Type,
        name="Builder",
        display_name="Builder",
        location=SourceLocation(file_path="sample.py", line=0, column=0),
        span=SourceSpan(start_line=0, start_column=0, end_line=2, end_column=0),
        details=TypeDetails(),
    )
    definitions = [builder_definition, builder_type_definition]
    collector = ReferenceCollector(
        file_path=str(file_path),
        source=source,
        project_root=str(tmp_path),
        all_definitions=definitions,
        definition_index=DefinitionIndex.build(definitions),
        module_symbol_id="sample",
        resolver=DummyResolver(),
    )
    collector.tree = ast.parse(source, filename=str(file_path))
    add_parents(collector.tree)
    func_node = next(node for node in ast.walk(collector.tree) if isinstance(node, ast.FunctionDef))
    name_nodes = [node for node in ast.walk(func_node) if isinstance(node, ast.Name) and node.id == "value" and isinstance(node.ctx, ast.Load)]

    original_ast_walk = ast.walk
    ast_walk_calls = 0

    def counting_ast_walk(node):
        nonlocal ast_walk_calls
        if node is func_node:
            ast_walk_calls += 1
        return original_ast_walk(node)

    monkeypatch.setattr(ast, "walk", counting_ast_walk)
    monkeypatch.setattr(
        collector,
        "_normalize_type_ref",
        lambda type_ref, *, line, column: "sample.Builder" if type_ref == "Builder" else None,
    )

    assert collector._infer_local_name_type("value", name_nodes[0], "sample.sample") == "sample.Builder"
    assert collector._infer_local_name_type("value", name_nodes[1], "sample.sample") == "sample.Builder"
    assert ast_walk_calls == 1


def test_fixtures_have_definitions():
    data = run_extract(str(FIXTURES_DIR))
    assert len(data.documents) >= 1
    defs = [d for doc in data.documents for d in doc.definitions]
    assert len(defs) >= 2
    kinds = {d.kind for d in defs}
    assert SymbolKind.Function in kinds


def test_fixtures_have_references():
    data = run_extract(str(FIXTURES_DIR))
    refs = [r for doc in data.documents for r in doc.references]
    assert len(refs) >= 1
    roles = {r.role for r in refs}
    assert ReferenceRole.Call in roles


def test_direct_call_does_not_emit_callee_read():
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "simple.foo"
    ]
    assert any(r.role == ReferenceRole.Call and r.target_symbol == "simple.bar" for r in refs)
    assert not any(r.role == ReferenceRole.Read and r.target_symbol == "simple.bar" for r in refs)


def test_imported_function_call_resolves_across_modules():
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "with_class.Helper.run"
    ]
    assert any(r.role == ReferenceRole.Call and r.target_symbol == "simple.bar" for r in refs)


def test_super_call_does_not_emit_builtin_super_call():
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "test_super_call.Child.__init__"
    ]
    assert any(r.role == ReferenceRole.Call and r.target_symbol == "test_super_call.Base.__init__" for r in refs)
    assert not any(r.target_symbol == "builtins.super" for r in refs)


def test_builtin_intrinsic_object_call_is_not_emitted():
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol in {"sentinel_def", "test_default_arg_ref"}
    ]
    assert not any(r.target_symbol in {"object", "builtins.object"} for r in refs)


def test_builtin_decorator_emits_decorate_without_read():
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol in {"test_cls_call.MyClass.create", "test_cls_call"}
    ]
    assert any(r.role == ReferenceRole.Decorate and r.target_symbol == "builtins.classmethod" for r in refs)
    assert not any(r.role == ReferenceRole.Read and r.target_symbol == "builtins.classmethod" for r in refs)


def test_import_alias_calls_use_stable_external_ids(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
import json as r
from itertools import chain


def normalize(items):
    r.dumps(items)
    return list(chain.from_iterable(items))
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.normalize" and reference.role == ReferenceRole.Call
    ]

    assert any(reference.target_symbol == "json.dumps" for reference in refs)
    assert any(reference.target_symbol == "itertools.chain.from_iterable" for reference in refs)
    assert not any(reference.target_symbol in {"r", "from", "None"} for reference in refs)


def test_import_driven_fallback_resolves_reexported_attribute_calls(tmp_path: Path):
    pkg_dir = tmp_path / "pkg"
    ops_dir = pkg_dir / "ops"
    ops_dir.mkdir(parents=True)

    (pkg_dir / "__init__.py").write_text("from . import ops\n", encoding="utf-8")
    (ops_dir / "__init__.py").write_text("from .models import CreateModel\n", encoding="utf-8")
    (ops_dir / "models.py").write_text(
        """
class CreateModel:
    def __init__(self):
        self.value = 1
""".strip()
        + "\n",
        encoding="utf-8",
    )
    (tmp_path / "main.py").write_text(
        """
from pkg import ops


def build():
    return ops.CreateModel()
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "main.build" and reference.role == ReferenceRole.Call
    ]

    assert any(reference.target_symbol == "pkg.ops.models.CreateModel" for reference in refs)


def test_import_driven_fallback_prefers_package_reexport_definition(tmp_path: Path):
    pkg_dir = tmp_path / "django" / "utils" / "translation"
    pkg_dir.mkdir(parents=True)

    (tmp_path / "django" / "__init__.py").write_text("", encoding="utf-8")
    (tmp_path / "django" / "utils" / "__init__.py").write_text("", encoding="utf-8")
    (pkg_dir / "__init__.py").write_text(
        """
def gettext(value):
    return value


gettext_lazy = gettext
""".strip()
        + "\n",
        encoding="utf-8",
    )
    (pkg_dir / "trans_null.py").write_text(
        """
def _(value):
    return value
""".strip()
        + "\n",
        encoding="utf-8",
    )
    (tmp_path / "sample.py").write_text(
        """
from django.utils.translation import gettext_lazy as _


def render():
    return _("hello")
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.render" and reference.role == ReferenceRole.Call
    ]

    assert any(reference.target_symbol == "django.utils.translation.__init__.gettext_lazy" for reference in refs)


def test_type_alias_assignments_do_not_emit_typing_reads(tmp_path: Path):
    (tmp_path / "typing_aliases.py").write_text(
        """
from __future__ import annotations
import typing as t

ResponseValue = t.Union[str, bytes]
HeadersValue = tuple[str, ...] | list[str]
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.role == ReferenceRole.Read
    ]

    assert not any(
        reference.target_symbol in {"typing.Union", "typing_aliases.ResponseValue", "typing_aliases.HeadersValue"}
        for reference in refs
    )


def test_chained_path_method_calls_resolve_to_path_methods(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
from pathlib import Path


def read_template(name: str) -> str:
    return Path(__file__).parent.joinpath(name).read_text(encoding="utf-8")
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.read_template" and reference.role == ReferenceRole.Call
    ]

    assert any(reference.target_symbol == "pathlib.Path.joinpath" for reference in refs)
    assert any(reference.target_symbol == "pathlib.Path.read_text" for reference in refs)


def test_local_path_variable_method_call_uses_constructor_type(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
from pathlib import Path


def normalize(path: str) -> str:
    current = Path(path)
    return str(current.resolve())
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.normalize" and reference.role == ReferenceRole.Call
    ]

    assert any(reference.target_symbol == "pathlib.Path.resolve" for reference in refs)


def test_string_literal_methods_do_not_emit_unresolved_builtin_calls(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
def render(name: str) -> str:
    return "{!r}".format(name)
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.render" and reference.role == ReferenceRole.Call
    ]

    assert not any(reference.method_name == "format" and reference.target_symbol is None for reference in refs)


def test_self_referential_attribute_update_does_not_recurse(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
class Box:
    value: str

    def normalize(self):
        self.value = self.value.strip()
        return self.value
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.Box.normalize"
    ]

    assert any(
        reference.role == ReferenceRole.Write
        and reference.target_symbol == "sample.Box.value"
        for reference in refs
    )


def test_path_method_on_binary_expression_uses_return_type_and_operator(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
from pathlib import Path


class Store:
    def root(self) -> Path:
        return Path(__file__).parent

    def read(self, name: str) -> str:
        return (self.root() / name).resolve().read_text()
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.Store.read" and reference.role == ReferenceRole.Call
    ]

    assert any(reference.target_symbol == "pathlib.Path.resolve" for reference in refs)
    assert any(reference.target_symbol == "pathlib.Path.read_text" for reference in refs)


@pytest.mark.skipif(shutil.which("pyrefly") is None, reason="pyrefly not installed")
def test_pyrefly_suppresses_builtin_noise_but_keeps_behavioral_decorator(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
class Example:
    @classmethod
    def build(cls, values):
        if isinstance(values, list):
            return cls(str(len(values)))
        try:
            raise ValueError("bad")
        except Exception:
            return cls(str(max(values)))
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path), resolver_backend="pyrefly")
    refs = [reference for doc in data.documents for reference in doc.references]

    assert any(
        reference.role == ReferenceRole.Decorate and reference.target_symbol == "builtins.classmethod"
        for reference in refs
    )
    assert not any(
        reference.target_symbol
        and reference.target_symbol.startswith("builtins.")
        and not (
            reference.role == ReferenceRole.Decorate
            and reference.target_symbol == "builtins.classmethod"
        )
        for reference in refs
    )


def test_schema_details_tagged_for_rust():
    data = run_extract(str(FIXTURES_DIR))
    raw = json.loads(data.model_dump_json())
    for doc in raw["documents"]:
        for d in doc["definitions"]:
            details = d["details"]
            assert isinstance(details, dict)
            assert len(details) == 1
            assert list(details.keys())[0] in ("Function", "Variable", "Type")

def test_method_call_on_type_hinted_parameter():
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [r for doc in data.documents for r in doc.references if r.enclosing_symbol == "test_method_resolve.create_image_edit"]
    
    # We expect a Call reference to RelayImageUseCase.execute
    execute_calls = [r for r in refs if r.role == ReferenceRole.Call and r.target_symbol == "test_method_resolve.RelayImageUseCase.execute"]
    if not execute_calls:
        print("REFS for test_method_resolve.create_image_edit:", refs)
    assert len(execute_calls) == 1, "Should resolve use_case.execute() to RelayImageUseCase.execute"

def test_except_handler_type_reference():
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [r for doc in data.documents for r in doc.references if r.enclosing_symbol == "test_except_resolve.create_image_edit"]
    
    # We expect a Read reference to QuotaError
    except_refs = [r for r in refs if r.role == ReferenceRole.Read and r.target_symbol == "test_except_resolve.QuotaError"]
    if not except_refs:
        print("REFS for test_except_resolve.create_image_edit:", refs)
    assert len(except_refs) >= 1, "Should resolve 'except QuotaError' as a Read reference to QuotaError"


def test_self_call_resolution():
    """put() calls self.api_route() -> should resolve to APIRouter.api_route for CF traversal."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "test_self_call.APIRouter.put"
    ]
    api_route_calls = [
        r
        for r in refs
        if r.role == ReferenceRole.Call and r.target_symbol == "test_self_call.APIRouter.api_route"
    ]
    if not api_route_calls:
        print("REFS for test_self_call.APIRouter.put:", refs)
    assert len(api_route_calls) == 1, "Should resolve self.api_route() to APIRouter.api_route"


def test_nested_function_call_attributed_to_outer():
    """Call inside nested function (decorator) should have enclosing_symbol = api_route, not api_route.decorator."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.role == ReferenceRole.Call and r.target_symbol == "test_nested_call.APIRouter.add_api_route"
    ]
    from_api_route = [r for r in refs if r.enclosing_symbol == "test_nested_call.APIRouter.api_route"]
    if not from_api_route:
        print("Call refs to add_api_route:", [(r.enclosing_symbol, r.target_symbol) for r in refs])
    assert len(from_api_route) >= 1, "Call from nested decorator should be attributed to api_route"


def test_nested_function_decorator_attributed_to_outer_and_not_defined(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
def deco(fn):
    return fn


def outer():
    @deco
    def inner():
        return 1

    return inner
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    defs = [definition.symbol_id for doc in data.documents for definition in doc.definitions]
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.role == ReferenceRole.Decorate and reference.target_symbol == "sample.deco"
    ]

    assert "sample.inner" not in defs
    assert any(reference.enclosing_symbol == "sample.outer" for reference in refs)
    assert not any(reference.enclosing_symbol == "sample.outer.inner" for reference in refs)


def test_annotated_doc_extraction():
    """Ensure PEP 727 Doc() strings inside Annotated are extracted as documentation."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    body_def = next(
        (d for doc in data.documents for d in doc.definitions if d.name == "Body" and "test_annotated_doc" in doc.relative_path),
        None,
    )
    assert body_def is not None, "Body function definition not found"
    assert len(body_def.documentation) == 2, "Should extract exactly 2 Doc() strings"
    assert "Default value if the parameter field is not set." in body_def.documentation[0]
    assert "The media type." in body_def.documentation[1]


def test_annotated_style_factory_use_signature_only_for_size():
    """Annotated-style factory (Doc() in params + trivial body) gets use_signature_only_for_size=True."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    body_def = next(
        (d for doc in data.documents for d in doc.definitions if d.name == "Body" and "test_annotated_doc" in doc.relative_path),
        None,
    )
    assert body_def is not None, "Body function definition not found"
    assert body_def.details.modifiers.use_signature_only_for_size is True, (
        "Body() has Doc() in Annotated params and trivial body (pass); "
        "should set use_signature_only_for_size so CF uses signature-only size"
    )


def test_default_arg_reference_emitted_as_read():
    """References in default argument values (e.g. SENTINEL) must be emitted as Read so CF includes them."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "test_default_arg_ref.foo"
    ]
    sentinel_reads = [
        r
        for r in refs
        if r.role == ReferenceRole.Read and r.target_symbol == "test_default_arg_ref.SENTINEL"
    ]
    if not sentinel_reads:
        print("REFS for test_default_arg_ref.foo:", refs)
    assert len(sentinel_reads) >= 1, (
        "Default arg 'flag: bool = SENTINEL' should produce a Read reference from foo to SENTINEL"
    )


def test_func_as_value_emitted_as_read():
    """Function used as value (e.g. register(handler), callback = handler) must produce Read ref."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "test_func_as_value.setup"
    ]
    handler_reads = [
        r
        for r in refs
        if r.role == ReferenceRole.Read and r.target_symbol == "test_func_as_value.handler"
    ]
    if not handler_reads:
        print("REFS for test_func_as_value.setup:", refs)
    assert len(handler_reads) >= 1, (
        "setup() uses handler as value (register(handler), callback=handler); "
        "should produce at least one Read reference from setup to handler"
    )


def test_super_call_resolves_to_parent_method():
    """super().__init__(...) must produce Call ref from Child.__init__ to Base.__init__."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "test_super_call.Child.__init__"
    ]
    super_calls = [
        r
        for r in refs
        if r.role == ReferenceRole.Call and r.target_symbol == "test_super_call.Base.__init__"
    ]
    if not super_calls:
        print("REFS for test_super_call.Child.__init__:", refs)
    assert len(super_calls) >= 1, (
        "super().__init__(name) should produce Call reference from Child.__init__ to Base.__init__"
    )


def test_augassign_emits_read_and_write():
    """counter += 1 must produce both Read and Write refs from increment to counter."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "test_augassign.increment"
    ]
    counter_reads = [
        r for r in refs if r.role == ReferenceRole.Read and r.target_symbol == "test_augassign.counter"
    ]
    counter_writes = [
        r for r in refs if r.role == ReferenceRole.Write and r.target_symbol == "test_augassign.counter"
    ]
    if not counter_reads or not counter_writes:
        print("REFS for test_augassign.increment:", refs)
    assert len(counter_reads) >= 1, "counter += 1 should produce Read from increment to counter"
    assert len(counter_writes) >= 1, "counter += 1 should produce Write from increment to counter"


def test_chained_attribute_call_uses_field_value_type(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
class Config:
    def from_mapping(self):
        return 1


class App:
    config: Config


def build(app: App):
    return app.config.from_mapping()
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.build"
    ]

    assert any(
        reference.role == ReferenceRole.Call
        and reference.target_symbol == "sample.Config.from_mapping"
        for reference in refs
    )


def test_chained_attribute_write_uses_field_value_type(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
class Inner:
    value: int


class Outer:
    inner: Inner

    def touch(self):
        self.inner.value = 1
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.Outer.touch"
    ]

    assert any(
        reference.role == ReferenceRole.Write
        and reference.target_symbol == "sample.Inner.value"
        for reference in refs
    )


def test_inferred_external_method_target_is_materialized(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
from pathlib import Path


def read(name: str):
    path = Path(name)
    return path.read_text()
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.read"
    ]
    external_symbols = {definition.symbol_id for definition in data.external_symbols}

    assert any(
        reference.role == ReferenceRole.Call
        and reference.target_symbol == "pathlib.Path.read_text"
        for reference in refs
    )
    assert "pathlib.Path.read_text" in external_symbols


def test_field_value_type_inference_uses_constructor_context(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
class Map:
    def add(self, rule):
        return rule


class App:
    url_map_class = Map

    def __init__(self):
        self.url_map = self.url_map_class()

    def add_url_rule(self, rule):
        self.url_map.add(rule)
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.App.add_url_rule"
    ]

    assert any(
        reference.role == ReferenceRole.Call
        and reference.target_symbol == "sample.Map.add"
        for reference in refs
    )


def test_unknown_function_hover_does_not_create_pseudo_receiver_type(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
def get_db():
    return object()


def init_db():
    db = get_db()
    db.executescript("schema")
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.init_db"
    ]
    external_symbols = {definition.symbol_id for definition in data.external_symbols}

    assert not any(reference.target_symbol == "def get_db.executescript" for reference in refs)
    assert "def get_db.executescript" not in external_symbols


def test_any_receiver_type_does_not_materialize_pseudo_method_target(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
from typing import Any


def normalize(item: Any):
    return item.upper()
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.normalize"
    ]
    external_symbols = {definition.symbol_id for definition in data.external_symbols}

    assert not any(reference.target_symbol == "Any.upper" for reference in refs)
    assert "Any.upper" not in external_symbols


def test_literal_receiver_type_does_not_materialize_pseudo_method_target(tmp_path: Path):
    (tmp_path / "sample.py").write_text(
        """
from typing import Literal


def normalize(item: Literal["GET"]):
    return item.upper()
""".strip()
        + "\n",
        encoding="utf-8",
    )

    data = run_extract(str(tmp_path))
    refs = [
        reference
        for doc in data.documents
        for reference in doc.references
        if reference.enclosing_symbol == "sample.normalize"
    ]
    external_symbols = {definition.symbol_id for definition in data.external_symbols}

    assert not any(reference.target_symbol == "Literal.upper" for reference in refs)
    assert "Literal.upper" not in external_symbols


def test_cls_call_resolves_to_init():
    """cls(name) in classmethod create() must produce Call ref from MyClass.create to MyClass.__init__."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "test_cls_call.MyClass.create"
    ]
    init_calls = [
        r
        for r in refs
        if r.role == ReferenceRole.Call and r.target_symbol == "test_cls_call.MyClass.__init__"
    ]
    if not init_calls:
        print("REFS for test_cls_call.MyClass.create:", refs)
    assert len(init_calls) >= 1, (
        "return cls(name) in classmethod create() should produce Call from MyClass.create to MyClass.__init__"
    )


def test_super_call_cross_module_resolves_to_parent_method():
    """super().__init__() in child_super.Child (base in base_super) must produce Call to base_super.Base.__init__."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "child_super.Child.__init__"
    ]
    super_calls = [
        r
        for r in refs
        if r.role == ReferenceRole.Call and r.target_symbol == "base_super.Base.__init__"
    ]
    if not super_calls:
        print("REFS for child_super.Child.__init__:", refs)
    assert len(super_calls) >= 1, (
        "super().__init__() in child_super.Child (Base from base_super) should produce Call to base_super.Base.__init__"
    )


def test_default_arg_cross_module_emitted_as_read():
    """Default arg SENTINEL imported from sentinel_def must produce Read from foo to sentinel_def.SENTINEL."""
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "default_arg_cross.foo"
    ]
    sentinel_reads = [
        r for r in refs if r.role == ReferenceRole.Read and r.target_symbol == "sentinel_def.SENTINEL"
    ]
    if not sentinel_reads:
        print("REFS for default_arg_cross.foo:", refs)
    assert len(sentinel_reads) >= 1, (
        "def foo(..., flag=SENTINEL) with SENTINEL from sentinel_def should produce Read from foo to sentinel_def.SENTINEL"
    )


def test_super_call_alias_base_resolves_to_parent_method():
    """super().__init__() in child_alias.Child (base imported as AliasBase) must produce Call to base_alias.Base.__init__.

    This simulates Flask's 'from .sansio.blueprints import Blueprint as SansioBlueprint'
    pattern where the base class is referenced by an alias name that differs from the
    actual class name.
    """
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "child_alias.Child.__init__"
    ]
    super_calls = [
        r
        for r in refs
        if r.role == ReferenceRole.Call and r.target_symbol == "base_alias.Base.__init__"
    ]
    if not super_calls:
        print("REFS for child_alias.Child.__init__:", refs)
    assert len(super_calls) >= 1, (
        "super().__init__() in child_alias.Child (AliasBase = base_alias.Base) should produce Call to base_alias.Base.__init__"
    )


def test_default_arg_ambiguous_sentinel_uses_import_context():
    """Default arg _sentinel with two same-named defs must pick the imported one (sentinel_a._sentinel).

    This simulates Flask's situation where both sansio/scaffold.py and ctx.py define
    _sentinel, and blueprints.py imports specifically from sansio.scaffold.
    """
    data = run_extract(str(FIXTURES_DIR), include_tests=True)
    refs = [
        r
        for doc in data.documents
        for r in doc.references
        if r.enclosing_symbol == "default_arg_ambig.bar"
    ]
    sentinel_reads = [
        r for r in refs if r.role == ReferenceRole.Read and r.target_symbol == "sentinel_a._sentinel"
    ]
    if not sentinel_reads:
        print("REFS for default_arg_ambig.bar:", refs)
    assert len(sentinel_reads) >= 1, (
        "def bar(..., flag=_sentinel) with _sentinel from sentinel_a (and sentinel_b also existing) "
        "should use import context to produce Read from bar to sentinel_a._sentinel"
    )
