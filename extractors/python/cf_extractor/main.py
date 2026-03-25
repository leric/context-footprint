"""
CLI entrypoint: scan project directory, extract SemanticData, print JSON to stdout.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import resource
import sys
import time
import traceback
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from .extractor import extract_definitions_from_file
from .reference_collector import collect_references
from .reference_index import DefinitionIndex
from .resolver_backend import DEFAULT_RESOLVER_BACKEND, RESOLVER_BACKENDS, build_project_resolver_backend
from .schema import DocumentSemantics, SemanticData, SymbolDefinition


_TEST_DIR_NAMES = ("test", "tests", "testing", "__tests__", "spec")
_SKIP_DIR_NAMES = frozenset({".venv", "venv", ".virtualenv", "virtualenv", "env"})


@dataclass(slots=True)
class ExtractionMetrics:
    resolver_backend: str
    file_count: int = 0
    definition_phase_seconds: float = 0.0
    reference_phase_seconds: float = 0.0
    total_seconds: float = 0.0
    definition_count: int = 0
    reference_count: int = 0
    resolved_reference_count: int = 0
    unresolved_reference_count: int = 0
    external_symbol_count: int = 0
    peak_rss_kb: int | None = None

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def _peak_rss_kb() -> int:
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if sys.platform == "darwin":
        return int(peak / 1024)
    return int(peak)


def _print_verbose(enabled: bool, message: str) -> None:
    if enabled:
        print(message, file=sys.stderr)


def _print_warning(message: str, *, verbose: bool) -> None:
    print(message, file=sys.stderr)
    if verbose:
        traceback.print_exc(file=sys.stderr)


def _is_skipped_path(rel_path: str) -> bool:
    parts = rel_path.replace("\\", "/").split("/")
    return any(part in _SKIP_DIR_NAMES for part in parts)


def _is_test_path(rel_path: str) -> bool:
    parts = rel_path.replace("\\", "/").split("/")
    for part in parts[:-1]:
        if part.lower() in _TEST_DIR_NAMES:
            return True
    name = parts[-1] if parts else ""
    return name.startswith("test_") and name.endswith(".py") or name.endswith("_test.py")


def find_python_files(
    project_root: str,
    *,
    include_tests: bool = False,
    include: list[str] | None = None,
    exclude: list[str] | None = None,
) -> list[str]:
    root = Path(project_root)
    out = []
    for p in root.rglob("*.py"):
        try:
            rel = p.relative_to(root)
            path_str = rel.as_posix()
            if _is_skipped_path(path_str):
                continue
            if include_tests or not _is_test_path(path_str):
                if include and not any(fnmatch.fnmatch(path_str, pat) for pat in include):
                    continue
                if exclude and any(fnmatch.fnmatch(path_str, pat) for pat in exclude):
                    continue
                out.append(path_str)
        except ValueError:
            continue
    return sorted(out)


def run_extract_with_metrics(
    project_root: str,
    venv_path: str | None = None,
    *,
    include_tests: bool = False,
    include: list[str] | None = None,
    exclude: list[str] | None = None,
    resolver_backend: str = DEFAULT_RESOLVER_BACKEND,
    ty_path: str | None = None,
    pyrefly_path: str | None = None,
    verbose: bool = False,
) -> tuple[SemanticData, ExtractionMetrics]:
    project_root = os.path.abspath(project_root)
    metrics = ExtractionMetrics(resolver_backend=resolver_backend)
    total_start = time.perf_counter()

    docs: list[DocumentSemantics] = []
    all_definitions: list[SymbolDefinition] = []
    external_symbols: dict[str, SymbolDefinition] = {}
    sources: dict[str, str] = {}

    py_files = find_python_files(
        project_root,
        include_tests=include_tests,
        include=include,
        exclude=exclude,
    )
    metrics.file_count = len(py_files)

    def_start = time.perf_counter()
    for idx, rel_path in enumerate(py_files, 1):
        print(f"[1/2] Definitions ({idx}/{metrics.file_count}): {rel_path}", file=sys.stderr)
        abs_path = os.path.join(project_root, rel_path)
        try:
            source = Path(abs_path).read_text(encoding="utf-8", errors="replace")
        except Exception as exc:
            _print_warning(f"Warning: skip {rel_path}: {exc}", verbose=verbose)
            continue
        sources[rel_path] = source
        try:
            doc = extract_definitions_from_file(abs_path, source, project_root)
            docs.append(doc)
            all_definitions.extend(doc.definitions)
        except Exception as exc:
            _print_warning(f"Warning: extract defs {rel_path}: {exc}", verbose=verbose)
            docs.append(DocumentSemantics(relative_path=rel_path, language="python", definitions=[], references=[]))
    metrics.definition_phase_seconds = time.perf_counter() - def_start
    metrics.definition_count = len(all_definitions)
    definition_index = DefinitionIndex.build(all_definitions)

    ref_start = time.perf_counter()
    backend = build_project_resolver_backend(
        resolver_backend,
        project_root=project_root,
        venv_path=venv_path,
        ty_path=ty_path,
        pyrefly_path=pyrefly_path,
    )
    try:
        total_ref = len(docs)
        for i, doc in enumerate(docs):
            rel_path = doc.relative_path
            print(f"[2/2] References ({i + 1}/{total_ref}): {rel_path}", file=sys.stderr)
            abs_path = os.path.join(project_root, rel_path)
            source = sources.get(rel_path)
            if source is None:
                try:
                    source = Path(abs_path).read_text(encoding="utf-8", errors="replace")
                except Exception:
                    continue
            open_seconds = 0.0
            collect_start: float | None = None
            try:
                open_start = time.perf_counter()
                resolver = backend.open_document(abs_path, source)
                open_seconds = time.perf_counter() - open_start
                collect_start = time.perf_counter()
                refs, ext_syms = collect_references(
                    doc,
                    abs_path,
                    source,
                    project_root,
                    all_definitions,
                    module_symbol_id=Path(rel_path).with_suffix("").as_posix().replace("/", ".") or "__main__",
                    resolver=resolver,
                    definition_index=definition_index,
                )
                collect_seconds = time.perf_counter() - collect_start
                for ext in ext_syms:
                    external_symbols[ext.symbol_id] = ext
                docs[i] = DocumentSemantics(
                    relative_path=doc.relative_path,
                    language=doc.language,
                    definitions=doc.definitions,
                    references=refs,
                )
                _print_verbose(
                    verbose,
                    (
                        f"[2/2] References done ({i + 1}/{total_ref}): {rel_path} "
                        f"open_document={open_seconds:.3f}s "
                        f"collect_references={collect_seconds:.3f}s "
                        f"refs={len(refs)} ext={len(ext_syms)}"
                    ),
                )
            except Exception as exc:
                failure_collect_seconds = time.perf_counter() - collect_start if collect_start is not None else 0.0
                _print_verbose(
                    verbose,
                    (
                        f"[2/2] References failed ({i + 1}/{total_ref}): {rel_path} "
                        f"open_document={open_seconds:.3f}s "
                        f"collect_references={failure_collect_seconds:.3f}s"
                    ),
                )
                _print_warning(f"Warning: references {rel_path}: {exc}", verbose=verbose)
    finally:
        backend.close()
    metrics.reference_phase_seconds = time.perf_counter() - ref_start

    internal_symbol_ids = {definition.symbol_id for definition in all_definitions}
    filtered_external_symbols = [
        symbol for symbol in external_symbols.values() if symbol.symbol_id not in internal_symbol_ids
    ]

    data = SemanticData(
        project_root=project_root,
        documents=docs,
        external_symbols=filtered_external_symbols,
    )
    all_references = [reference for doc in docs for reference in doc.references]
    metrics.reference_count = len(all_references)
    metrics.resolved_reference_count = sum(1 for reference in all_references if reference.target_symbol)
    metrics.unresolved_reference_count = sum(1 for reference in all_references if not reference.target_symbol)
    metrics.external_symbol_count = len(filtered_external_symbols)
    metrics.total_seconds = time.perf_counter() - total_start
    metrics.peak_rss_kb = _peak_rss_kb()
    return data, metrics


def run_extract(
    project_root: str,
    venv_path: str | None = None,
    *,
    include_tests: bool = False,
    include: list[str] | None = None,
    exclude: list[str] | None = None,
    resolver_backend: str = DEFAULT_RESOLVER_BACKEND,
    ty_path: str | None = None,
    pyrefly_path: str | None = None,
    verbose: bool = False,
) -> SemanticData:
    data, _ = run_extract_with_metrics(
        project_root,
        venv_path=venv_path,
        include_tests=include_tests,
        include=include,
        exclude=exclude,
        resolver_backend=resolver_backend,
        ty_path=ty_path,
        pyrefly_path=pyrefly_path,
        verbose=verbose,
    )
    return data


def main() -> None:
    parser = argparse.ArgumentParser(description="Extract Context-Footprint SemanticData from a Python project.")
    parser.add_argument("project_root", nargs="?", help="Project root directory")
    parser.add_argument(
        "--venv",
        help="Path to virtual environment (auto-detected as .venv in project root if not specified)",
    )
    parser.add_argument(
        "--include-tests",
        action="store_true",
        help="Include test files (default: exclude paths under test/tests/testing/__tests__/spec and test_*.py, *_test.py)",
    )
    parser.add_argument(
        "--include",
        nargs="*",
        metavar="PATTERN",
        help="Glob patterns for paths to include (relative to project root). If empty, all paths pass.",
    )
    parser.add_argument(
        "--exclude",
        nargs="*",
        metavar="PATTERN",
        help="Glob patterns for paths to exclude (relative to project root). Excluded paths are skipped.",
    )
    parser.add_argument(
        "--resolver-backend",
        choices=RESOLVER_BACKENDS,
        default=DEFAULT_RESOLVER_BACKEND,
        help="Resolver backend used for cross-file symbol navigation.",
    )
    parser.add_argument(
        "--ty-path",
        help="Path to the ty executable when --resolver-backend=ty is used.",
    )
    parser.add_argument(
        "--pyrefly-path",
        help="Path to the pyrefly executable when --resolver-backend=pyrefly is used.",
    )
    parser.add_argument(
        "--metrics-out",
        help="Optional path to write extraction metrics as JSON.",
    )
    parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Print per-file reference timings and full tracebacks for extraction warnings.",
    )

    args = parser.parse_args()
    project_root = os.path.abspath(args.project_root or ".")

    venv_path = args.venv
    if not venv_path:
        default_venv = os.path.join(project_root, ".venv")
        if os.path.isdir(default_venv):
            venv_path = default_venv
            print(f"Auto-detected venv at: {venv_path}", file=sys.stderr)

    data, metrics = run_extract_with_metrics(
        project_root,
        venv_path=venv_path,
        include_tests=args.include_tests,
        include=args.include,
        exclude=args.exclude,
        resolver_backend=args.resolver_backend,
        ty_path=args.ty_path,
        pyrefly_path=args.pyrefly_path,
        verbose=args.verbose,
    )
    if args.metrics_out:
        Path(args.metrics_out).write_text(
            json.dumps(metrics.to_dict(), indent=2, sort_keys=True),
            encoding="utf-8",
        )
    print(data.model_dump_json(indent=2))


if __name__ == "__main__":
    main()
