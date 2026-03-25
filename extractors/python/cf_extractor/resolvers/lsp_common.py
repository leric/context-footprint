from __future__ import annotations

import json
import os
import re
import subprocess
from functools import lru_cache
from pathlib import Path
from typing import Any, Optional
from urllib.parse import unquote, urlparse
from urllib.request import pathname2url, url2pathname

from .base import DocumentResolver, ProjectResolverBackend, ResolvedTarget


_IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def path_to_uri(path: str) -> str:
    return f"file://{pathname2url(os.path.abspath(path))}"


def uri_to_path(uri: str | None) -> Optional[str]:
    if not uri:
        return None
    parsed = urlparse(uri)
    if parsed.scheme != "file":
        return None
    netloc = f"//{parsed.netloc}" if parsed.netloc else ""
    return os.path.abspath(url2pathname(f"{netloc}{unquote(parsed.path)}"))


def extract_marked_up_text(contents: Any) -> list[str]:
    if contents is None:
        return []
    if isinstance(contents, str):
        return [contents]
    if isinstance(contents, list):
        out: list[str] = []
        for item in contents:
            out.extend(extract_marked_up_text(item))
        return out
    if isinstance(contents, dict):
        value = contents.get("value")
        if isinstance(value, str):
            return [value]
    return []


@lru_cache(maxsize=1024)
def _cached_file_lines(path: str, mtime_ns: int, size: int) -> tuple[str, ...]:
    del mtime_ns, size
    try:
        return tuple(Path(path).read_text(encoding="utf-8", errors="replace").splitlines())
    except Exception:
        return ()


@lru_cache(maxsize=4096)
def _cached_symbol_name(path: str, line: int, column: int, mtime_ns: int, size: int) -> Optional[str]:
    if line < 0 or column < 0:
        return None
    lines = _cached_file_lines(path, mtime_ns, size)
    if line >= len(lines):
        return None

    for match in _IDENT_RE.finditer(lines[line]):
        if match.start() <= column < match.end():
            return match.group(0)
    return None


def guess_symbol_name(path: str | None, line: int, column: int) -> Optional[str]:
    if not path:
        return None
    try:
        stat_result = os.stat(path)
    except OSError:
        return None
    return _cached_symbol_name(path, line, column, stat_result.st_mtime_ns, stat_result.st_size)


def module_name_from_path(path: str | None) -> str | None:
    if not path:
        return None
    normalized = path.replace("\\", "/")
    marker = None
    for candidate in ("/stdlib/", "/stubs/"):
        if candidate in normalized:
            marker = candidate
            break
    if marker is None:
        return None
    rel = normalized.split(marker, 1)[1]
    if rel.endswith("/__init__.pyi") or rel.endswith("/__init__.py"):
        rel = rel.rsplit("/__init__.", 1)[0]
    elif rel.endswith(".pyi") or rel.endswith(".py"):
        rel = rel.rsplit(".", 1)[0]
    parts = [part for part in rel.split("/") if part]
    return ".".join(parts) if parts else None


def builtin_full_name(path: str | None, name: str | None) -> str | None:
    if not path or not name:
        return None
    normalized = path.replace("\\", "/")
    if normalized.endswith("/builtins.pyi") or normalized.endswith("/builtins.py"):
        return f"builtins.{name}"
    return None


def build_lsp_environment(python_env: str | None) -> dict[str, str]:
    env = os.environ.copy()
    if not python_env:
        return env

    resolved = os.path.abspath(python_env)
    venv_root = resolved
    if os.path.isfile(resolved):
        venv_root = os.path.dirname(os.path.dirname(resolved))

    env["VIRTUAL_ENV"] = venv_root
    bin_dir = os.path.join(venv_root, "Scripts" if os.name == "nt" else "bin")
    if os.path.isdir(bin_dir):
        env["PATH"] = os.pathsep.join([bin_dir, env.get("PATH", "")]).strip(os.pathsep)
    return env


def python_executable_from_env(python_env: str | None) -> str | None:
    if not python_env:
        return None

    resolved = os.path.abspath(python_env)
    if os.path.isfile(resolved):
        return resolved

    candidates = [
        os.path.join(resolved, "bin", "python"),
        os.path.join(resolved, "Scripts", "python.exe"),
        os.path.join(resolved, "Scripts", "python"),
    ]
    for candidate in candidates:
        if os.path.isfile(candidate):
            return candidate
    return None


class JsonRpcClient:
    def __init__(
        self,
        command: list[str],
        root_uri: str,
        *,
        server_name: str,
        cwd: str | None = None,
        env: dict[str, str] | None = None,
        initialization_options: dict[str, Any] | None = None,
    ):
        self._server_name = server_name
        self._proc = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            cwd=cwd,
            env=env,
        )
        self._next_id = 1
        params: dict[str, Any] = {
            "processId": os.getpid(),
            "rootUri": root_uri,
            "capabilities": {},
            "clientInfo": {"name": "cf-extractor", "version": "0.2.0"},
        }
        if initialization_options:
            params["initializationOptions"] = initialization_options
        self.request("initialize", params)
        self.notify("initialized", {})

    def _write(self, payload: dict[str, Any]) -> None:
        body = json.dumps(payload).encode("utf-8")
        header = f"Content-Length: {len(body)}\r\n\r\n".encode("ascii")
        assert self._proc.stdin is not None
        self._proc.stdin.write(header)
        self._proc.stdin.write(body)
        self._proc.stdin.flush()

    def _read(self) -> dict[str, Any]:
        assert self._proc.stdout is not None
        headers: dict[str, str] = {}
        while True:
            line = self._proc.stdout.readline()
            if not line:
                raise RuntimeError(f"{self._server_name} server exited unexpectedly")
            if line == b"\r\n":
                break
            header = line.decode("ascii").strip()
            key, _, value = header.partition(":")
            headers[key.lower()] = value.strip()
        content_length = int(headers["content-length"])
        body = self._proc.stdout.read(content_length)
        return json.loads(body.decode("utf-8"))

    def request(self, method: str, params: dict[str, Any]) -> Any:
        request_id = self._next_id
        self._next_id += 1
        self._write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        while True:
            message = self._read()
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise RuntimeError(f"{self._server_name} server {method} failed: {message['error']}")
            return message.get("result")

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def close(self) -> None:
        if self._proc.poll() is None:
            try:
                self.request("shutdown", {})
            except Exception:
                pass
            try:
                self.notify("exit", {})
            except Exception:
                pass
            try:
                self._proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self._proc.terminate()
                self._proc.wait(timeout=2)


class LspDocumentResolver(DocumentResolver):
    def __init__(self, backend: "LspProjectResolverBackend", file_path: str):
        self._backend = backend
        self._file_path = os.path.abspath(file_path)

    def goto(self, line: int, column: int, *, follow_imports: bool = False) -> list[ResolvedTarget]:
        result = self._backend.request_definition(self._file_path, line, column)
        return self._backend.targets_from_locations(result)

    def references(self, line: int, column: int) -> list[ResolvedTarget]:
        result = self._backend.request_references(self._file_path, line, column)
        return self._backend.targets_from_locations(result)

    def document_symbols(self) -> list[dict[str, Any]]:
        return self._backend.request_document_symbols(self._file_path)

    def workspace_symbols(self, query: str) -> list[dict[str, Any]]:
        return self._backend.request_workspace_symbols(query)

    def hover(self, line: int, column: int) -> list[str]:
        return self._backend.request_hover(self._file_path, line, column)


class LspProjectResolverBackend(ProjectResolverBackend):
    def __init__(
        self,
        project_root: str,
        *,
        command: list[str],
        server_name: str,
        python_env: str | None = None,
        initialization_options: dict[str, Any] | None = None,
    ):
        self._project_root = os.path.abspath(project_root)
        self._client = JsonRpcClient(
            command,
            path_to_uri(self._project_root),
            server_name=server_name,
            cwd=self._project_root,
            env=build_lsp_environment(python_env),
            initialization_options=initialization_options,
        )
        self._opened_documents: set[str] = set()
        self._resolved_target_cache: dict[tuple[str, int, int], ResolvedTarget] = {}

    @staticmethod
    def to_lsp_line(line: int) -> int:
        return max(0, line - 1)

    def _ensure_open(self, file_path: str, source: str | None = None) -> None:
        abs_path = os.path.abspath(file_path)
        if abs_path in self._opened_documents:
            return
        if source is None:
            source = Path(abs_path).read_text(encoding="utf-8", errors="replace")
        self._client.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": path_to_uri(abs_path),
                    "languageId": "python",
                    "version": 1,
                    "text": source,
                }
            },
        )
        self._opened_documents.add(abs_path)

    def location_to_target(self, location: dict[str, Any]) -> ResolvedTarget:
        uri = location.get("uri") or location.get("targetUri")
        range_data = location.get("range") or location.get("targetSelectionRange") or location.get("targetRange") or {}
        start = range_data.get("start", {})
        line = int(start.get("line", 0))
        column = int(start.get("character", 0))
        path = uri_to_path(uri)
        cache_key = (path or uri or "", line, column)
        cached = self._resolved_target_cache.get(cache_key)
        if cached is not None:
            return cached
        name = guess_symbol_name(path, line, column)
        if not name and line == 0 and column == 0:
            name = module_name_from_path(path)
        elif line == 0 and column == 0:
            module_name = module_name_from_path(path)
            if module_name:
                name = module_name
        target = ResolvedTarget(
            path=path,
            line=line,
            column=column,
            name=name,
            full_name=builtin_full_name(path, name),
            kind=None,
            documentation=[],
        )
        self._resolved_target_cache[cache_key] = target
        return target

    def targets_from_locations(self, result: Any) -> list[ResolvedTarget]:
        if result is None:
            return []
        if isinstance(result, dict):
            return [self.location_to_target(result)]
        if isinstance(result, list):
            return [self.location_to_target(item) for item in result]
        return []

    def request_definition(self, file_path: str, line: int, column: int) -> Any:
        self._ensure_open(file_path)
        return self._client.request(
            "textDocument/definition",
            {
                "textDocument": {"uri": path_to_uri(file_path)},
                "position": {"line": self.to_lsp_line(line), "character": column},
            },
        )

    def request_references(self, file_path: str, line: int, column: int) -> Any:
        self._ensure_open(file_path)
        return self._client.request(
            "textDocument/references",
            {
                "textDocument": {"uri": path_to_uri(file_path)},
                "position": {"line": self.to_lsp_line(line), "character": column},
                "context": {"includeDeclaration": True},
            },
        )

    def request_document_symbols(self, file_path: str) -> list[dict[str, Any]]:
        self._ensure_open(file_path)
        result = self._client.request("textDocument/documentSymbol", {"textDocument": {"uri": path_to_uri(file_path)}})
        return result or []

    def request_workspace_symbols(self, query: str) -> list[dict[str, Any]]:
        result = self._client.request("workspace/symbol", {"query": query})
        return result or []

    def request_hover(self, file_path: str, line: int, column: int) -> list[str]:
        if not file_path:
            return []
        self._ensure_open(file_path)
        result = self._client.request(
            "textDocument/hover",
            {
                "textDocument": {"uri": path_to_uri(file_path)},
                "position": {"line": self.to_lsp_line(line), "character": column},
            },
        )
        if not isinstance(result, dict):
            return []
        return extract_marked_up_text(result.get("contents"))

    def open_document(self, file_path: str, source: str) -> DocumentResolver:
        self._ensure_open(file_path, source)
        return LspDocumentResolver(self, file_path)

    def close(self) -> None:
        self._client.close()
