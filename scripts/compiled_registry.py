"""Source-match and enumerate the Catch registry emitted by a DuckDB test runner."""
from collections import Counter
import json
from pathlib import Path
import platform
import re
import subprocess

from upstream_suite import digest

SQL_SUFFIXES = (".test", ".test_slow", ".test_coverage")
GENERATED_MARKERS = {
    "template": r"\bTEMPLATE_TEST_CASE(?:_METHOD)?\s*\(",
    "generator": r"\bGENERATE(?:_COPY|_REF)?\s*\(",
    "section": r"\b(?:SECTION|DYNAMIC_SECTION)\s*\(",
}


def source_parameterizations(source):
    """Report source sites which Catch may expand; do not pretend they are instances."""
    cases = []
    for path in sorted((source / "test").rglob("*")):
        if path.suffix not in (".cpp", ".cc", ".hpp") or not path.is_file():
            continue
        text = path.read_text(errors="replace")
        for kind, pattern in GENERATED_MARKERS.items():
            for match in re.finditer(pattern, text):
                cases.append({"id": f"{path.relative_to(source)}:{text.count(chr(10), 0, match.start()) + 1}:{kind}",
                              "path": str(path.relative_to(source)),
                              "line": text.count("\n", 0, match.start()) + 1, "kind": kind})
    ids = [case["id"] for case in cases]
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate source parameterization IDs")
    return {"declarations": cases, "counts": dict(sorted(Counter(c["kind"] for c in cases).items())),
            "runtime_instances": "not exposed by Catch --list-tests; source declarations are not instance counts"}


def source_matrix_definitions(source):
    """Enumerate source configuration, platform and extension-root definitions, not runs."""
    definitions = []
    config_root = source / "test/configs"
    for path in sorted(config_root.glob("*.json")):
        try:
            value = json.loads(path.read_text())
        except json.JSONDecodeError as error:
            raise ValueError(f"malformed test configuration {path.relative_to(source)}: {error}") from error
        if not isinstance(value, dict):
            raise ValueError(f"test configuration must be an object: {path.relative_to(source)}")
        definitions.append({"id": f"config:{path.relative_to(source)}", "kind": "test_config",
                            "path": str(path.relative_to(source)), "keys": sorted(value)})
    workflow_root = source / ".github/workflows"
    for path in sorted(workflow_root.glob("*.y*ml")):
        text = path.read_text()
        for line_number, line in enumerate(text.splitlines(), 1):
            config = re.search(r"--test-config\s+(test/configs/[A-Za-z0-9_.-]+\.json)", line)
            if config:
                config_path = config.group(1)
                if not (source / config_path).is_file():
                    raise ValueError(f"CI references missing test configuration {config_path}")
                definitions.append({"id": f"ci-config:{path.relative_to(source)}:{line_number}",
                                    "kind": "ci_test_config_invocation", "path": str(path.relative_to(source)),
                                    "line": line_number, "config": config_path})
            runner = re.search(r"^\s*runs-on:\s*(.+?)\s*$", line)
            if runner:
                definitions.append({"id": f"ci-platform:{path.relative_to(source)}:{line_number}",
                                    "kind": "ci_platform", "path": str(path.relative_to(source)),
                                    "line": line_number, "expression": runner.group(1)})
    for path in sorted((source / "extension").glob("*/test/sql")):
        if path.is_dir():
            definitions.append({"id": f"extension-root:{path.relative_to(source)}", "kind": "extension_test_root",
                                "path": str(path.relative_to(source))})
    ids = [entry["id"] for entry in definitions]
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate source matrix definition IDs")
    return {"definitions": definitions, "counts": dict(sorted(Counter(d["kind"] for d in definitions).items())),
            "scope": "Pinned source definitions and CI declarations; they are not compiled or executed matrix instances."}


def _cache_identity(source, binary):
    cache = binary.parents[1] / "CMakeCache.txt"
    if not cache.is_file():
        raise ValueError(f"compiled registry requires CMakeCache.txt beside runner: {cache}")
    text = cache.read_text()
    expected = f"CMAKE_HOME_DIRECTORY:INTERNAL={source.resolve()}"
    if expected not in text:
        raise ValueError("runner CMake source directory does not match the pinned source checkout")
    build_type = re.search(r"^CMAKE_BUILD_TYPE:[^=]*=(.*)$", text, re.MULTILINE)
    if build_type is None:
        raise ValueError("runner CMake configuration has no build type")
    settings = {line.split("=", 1)[0]: line.split("=", 1)[1] for line in text.splitlines()
                if "=" in line and line.split("=", 1)[0].split(":", 1)[0].startswith(
                    ("BUILD_", "ENABLE_", "DISABLE_", "DUCKDB_", "EXTENSION_", "CMAKE_OSX"))}
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
    generated_loader = binary.parents[1] / "codegen/src/generated_extension_loader.cpp"
    if not generated_loader.is_file():
        raise ValueError("compiled registry requires generated extension loader provenance")
    return {"runner": str(binary.resolve()), "runner_sha256": digest(binary), "cmake_cache": str(cache),
            "cmake_cache_sha256": digest(cache), "cmake_source": str(source.resolve()),
            "source_revision": revision, "source_discovery_sha256": digest(source / "test/sqlite/test_sqllogictest.cpp"),
            "generated_extension_loader": str(generated_loader), "generated_extension_loader_sha256": digest(generated_loader),
            "build_type": build_type.group(1), "settings": settings,
            "platform": platform.system(), "machine": platform.machine()}


def _parse_listing(stdout):
    lines = [line for line in stdout.splitlines() if line.strip()]
    if not lines or lines[0] != "name\tgroup":
        raise ValueError("Catch --list-tests did not produce the required name/group registry format")
    cases = []
    for line in lines[1:]:
        if "\t" not in line:
            raise ValueError(f"malformed Catch registry line: {line!r}")
        name, tags = line.split("\t", 1)
        # Catch emits an empty group field for untagged cases; retain it in the
        # identity rather than silently dropping that compiled case.
        if not name:
            raise ValueError(f"incomplete Catch registry line: {line!r}")
        cases.append({"id": f"{name}\t{tags}", "name": name, "tags": tags,
                      "hidden": "[.]" in tags, "sql": name.endswith(SQL_SUFFIXES)})
    ids, names = [case["id"] for case in cases], [case["name"] for case in cases]
    if not cases or len(ids) != len(set(ids)) or len(names) != len(set(names)):
        raise ValueError("empty, duplicate Catch registry IDs, or duplicate Catch test names")
    return cases


def _configured_extension_roots(source, provenance):
    text = Path(provenance["generated_extension_loader"]).read_text()
    match = re.search(r"LoadedExtensionTestPaths\s*\(\)\s*\{.*?vector<string>\s+VEC\s*=\s*\{(.*?)\};",
                      text, re.DOTALL)
    if match is None:
        raise ValueError("generated extension loader has no parseable LoadedExtensionTestPaths body")
    roots = re.findall(r'"([^"\\]*(?:\\.[^"\\]*)*)"', match.group(1))
    if len(roots) != len(set(roots)):
        raise ValueError("duplicate configured loaded-extension test roots")
    entries = []
    for root in roots:
        raw_root = Path(root)
        resolved = (raw_root if raw_root.is_absolute() else Path(source) / raw_root).resolve()
        entry = {"id": f"configured-extension-root:{root}", "path": root,
                 "resolved_path": str(resolved), "exists": resolved.is_dir(), "source_relative": None}
        if resolved.is_relative_to(Path(source).resolve()):
            entry["source_relative"] = str(resolved.relative_to(Path(source).resolve()))
        entries.append(entry)
    if any(not entry["exists"] for entry in entries):
        raise ValueError("configured loaded-extension test root is absent")
    return {"roots": entries, "count": len(entries),
            "scope": "Actual CMake-generated loader roots for this one compiled runner configuration."}


def _configured_extension_sql_ids(configured_extensions):
    """Mirror listFiles(root): retain an absolute root's absolute emitted paths."""
    ids = set()
    for root in configured_extensions["roots"]:
        raw_root, resolved = Path(root["path"]), Path(root["resolved_path"])
        for path in resolved.rglob("*"):
            if not path.is_file() or not str(path).endswith(SQL_SUFFIXES):
                continue
            # FileSystem::JoinPath retains the configured root spelling.  Do
            # not leak Python's resolved /private/... spelling for /var/... .
            emitted = str(raw_root / path.relative_to(resolved))
            ids.add(emitted.replace("\\", "/"))
    return ids


def enumerate_registry(source, binary, output_dir):
    """List the actual compiled registry, failing closed on provenance/accounting errors."""
    binary = Path(binary)
    output_dir = Path(output_dir)
    if not binary.is_file():
        return {"status": "unavailable", "reason": f"native test runner not built: {binary}"}
    command = [str(binary), "*", "--list-tests"]
    output = output_dir / "compiled-registry.txt"
    stderr = output_dir / "compiled-registry.stderr"
    try:
        provenance = _cache_identity(Path(source), binary)
        configured_extensions = _configured_extension_roots(source, provenance)
        result = subprocess.run(command, cwd=source, text=True, capture_output=True, timeout=60)
        output.write_text(result.stdout)
        stderr.write_text(result.stderr)
        cases = _parse_listing(result.stdout)
        # Catch reports the number of listed test cases as its process status.
        # On POSIX that is truncated to one byte, so a nonzero status is normal.
        if result.returncode not in (0, len(cases) % 256):
            raise ValueError(f"Catch registry listing failed with exit status {result.returncode}")
        source_paths = {str(path.relative_to(source)) for root in ("test", "third_party/sqllogictest/test")
                        for path in (Path(source) / root).rglob("*")
                        if path.is_file() and str(path.relative_to(source)).endswith(SQL_SUFFIXES)}
        extension_paths = _configured_extension_sql_ids(configured_extensions)
        listed_sql = {case["name"] for case in cases if case["sql"]}
        expected_sql = source_paths | extension_paths
        unknown = sorted(listed_sql - expected_sql)
        missing = sorted(expected_sql - listed_sql)
        if unknown or missing:
            raise ValueError("compiled SQL registry does not account for pinned source IDs: "
                             f"unknown={len(unknown)} missing={len(missing)}")
        return {"status": "enumerated_not_executed", "command": command, "exit_code": result.returncode,
                "provenance": provenance, "configured_extension_roots": configured_extensions,
                "cases": cases, "names": len(cases), "unique_ids": len(set(c["id"] for c in cases)),
                "hidden_cases": sum(c["hidden"] for c in cases), "sql_file_cases": len(listed_sql),
                "slow_sql_file_cases": sum(c["sql"] and c["name"].endswith(".test_slow") for c in cases),
                "coverage_sql_file_cases": sum(c["sql"] and c["name"].endswith(".test_coverage") for c in cases),
                "native_cases": len(cases) - len(listed_sql), "source_sql_not_registered": missing,
                "registry_sql_not_in_source": unknown, "configured_extension_sql_file_cases": len(extension_paths),
                "output": str(output), "output_sha256": digest(output)}
    except Exception as error:
        return {"status": "setup_failure", "command": command, "reason": str(error),
                "output": str(output) if output.exists() else None}
