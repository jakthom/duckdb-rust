"""Source-match and enumerate the Catch registry emitted by a DuckDB test runner."""
from collections import Counter
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
    return {"runner": str(binary.resolve()), "runner_sha256": digest(binary), "cmake_cache": str(cache),
            "cmake_cache_sha256": digest(cache), "cmake_source": str(source.resolve()),
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
    ids = [case["id"] for case in cases]
    if not cases or len(ids) != len(set(ids)):
        raise ValueError("empty or duplicate Catch registry IDs")
    return cases


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
        listed_sql = {case["name"] for case in cases if case["sql"]}
        unknown = sorted(listed_sql - source_paths)
        missing = sorted(source_paths - listed_sql)
        if unknown or missing:
            raise ValueError("compiled SQL registry does not account for pinned source IDs: "
                             f"unknown={len(unknown)} missing={len(missing)}")
        return {"status": "enumerated_not_executed", "command": command, "exit_code": result.returncode,
                "provenance": provenance, "cases": cases, "names": len(cases), "unique_ids": len(set(c["id"] for c in cases)),
                "hidden_cases": sum(c["hidden"] for c in cases), "sql_file_cases": len(listed_sql),
                "native_cases": len(cases) - len(listed_sql), "source_sql_not_registered": missing,
                "registry_sql_not_in_source": unknown, "output": str(output), "output_sha256": digest(output)}
    except Exception as error:
        return {"status": "setup_failure", "command": command, "reason": str(error),
                "output": str(output) if output.exists() else None}
