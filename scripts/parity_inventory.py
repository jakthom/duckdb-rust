"""Inventory parity obligations and installed reference capabilities, without claiming passes."""
import argparse
from collections import Counter
from datetime import datetime, timezone
import json
from pathlib import Path
import platform
import re
import subprocess

from check_references import build_identity
from reference_version import ROOT, TARGETS, require_checkout
from upstream_suite import declarations, digest


def source_inventory(source):
    paths = subprocess.check_output(
        ["git", "ls-files", "-z"], cwd=source).decode().split("\0")[:-1]
    obligations = []
    for name in paths:
        path = source / name
        if path.is_file() and not path.is_symlink() and path.suffix in (
                ".cpp", ".hpp", ".cc", ".py", ".swift", ".benchmark",
                ".test", ".test_slow", ".test_coverage"):
            obligations.extend(declarations(name, path.read_bytes()))
    sql_paths = [e["path"] for e in obligations if e["kind"] == "sqllogictest"]
    # Both pinned RegisterSqllogictests implementations scan these core roots,
    # then configured loaded-extension test roots, not arbitrary repository files.
    core_roots = ("test/", "third_party/sqllogictest/test/")
    core_sql = [p for p in sql_paths if p.startswith(core_roots)]
    core_set = set(core_sql)
    return {
        "tracked_assets": len(paths),
        "counts": dict(sorted(Counter(e["kind"] for e in obligations).items())),
        "obligations": obligations,
        "sql_discovery": {
            "core_roots": core_roots,
            "core_candidates": core_sql,
            "core_candidate_count": len(core_sql),
            "outside_core_roots": [p for p in sql_paths if p not in core_set],
            "extension_roots": "conditional on configured/loaded extension test registrations",
        },
        "configuration_files": [p for p in paths if p.startswith("test/configs/")],
        "ci_workflows": [p for p in paths if p.startswith(".github/workflows/")],
        "external_references": [p for p in paths if p.endswith(".gitmodules")
                                or p.startswith(".github/config/extensions/")],
    }


def native_registry(configuration, output_dir):
    binary = configuration.build / "test/unittest"
    if not binary.is_file():
        return {"status": "unavailable", "reason": "native test runner not built"}
    # An explicit wildcard includes Catch hidden tests; this is listing only.
    command = [str(binary), "*", "--list-test-names-only"]
    result = subprocess.run(command, cwd=configuration.source, text=True,
                            capture_output=True, timeout=60)
    output = output_dir / "native-registry.txt"
    output.write_text(result.stdout)
    (output_dir / "native-registry.stderr").write_text(result.stderr)
    names = [line for line in result.stdout.splitlines() if line.strip()]
    # Catch's listing can return the number of names modulo the process exit range.
    if result.returncode not in (0, len(names) % 256) or not names:
        return {"status": "setup_failure", "command": command,
                "exit_code": result.returncode, "output": str(output)}
    sql = [name for name in names if name.endswith((".test", ".test_slow", ".test_coverage"))]
    return {"status": "enumerated_not_executed", "command": command,
            "exit_code": result.returncode, "names": len(names),
            "unique_names": len(set(names)), "sql_file_names": len(sql),
            "other_names": len(names) - len(sql), "binary_sha256": digest(binary),
            "output": str(output), "output_sha256": digest(output)}


def rust_inventory():
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version=1"], cwd=ROOT))
    packages = [p for p in metadata["packages"] if p["id"] in metadata["workspace_members"]]
    targets = [{"package": p["name"], "name": t["name"], "kind": t["kind"],
                "source": t["src_path"]} for p in packages for t in p["targets"]]
    paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")[:-1]
    tests = []
    for name in paths:
        if name.endswith(".rs") and not name.startswith("third_party/"):
            content = (ROOT / name).read_text()
            for match in re.finditer(r"#\[test\]", content):
                tests.append({"path": name, "line": content.count("\n", 0, match.start()) + 1})
    return {"revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
            "status": subprocess.check_output(["git", "status", "--short"], cwd=ROOT, text=True),
            "targets": targets, "test_attribute_declarations": len(tests), "declarations": tests,
            "target_kinds": dict(Counter(kind for t in targets for kind in t["kind"])),
            "scope": "Source test attributes and Cargo targets, not generated tests or executed assertions."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(),
              "host": platform.platform(), "machine": platform.machine(),
              "script_sha256": digest(Path(__file__)), "rust": rust_inventory(), "targets": {},
              "unmeasured": ["generated/parameterized instances", "full configuration/platform matrix",
                             "external repositories and client suites", "native-to-Rust assertion mappings",
                             "full performance/CPU/memory/I/O population"],
              "complete_parity": False}
    for target, configuration in TARGETS.items():
        directory = output / target
        directory.mkdir()
        entry = {}
        report["targets"][target] = entry
        try:
            require_checkout(configuration.source, target)
            entry["source"] = source_inventory(configuration.source)
            entry["build"] = build_identity(target)
            entry["compiled_registry"] = native_registry(configuration, directory)
            if entry["compiled_registry"]["status"] == "enumerated_not_executed":
                names = set((directory / "native-registry.txt").read_text().splitlines())
                core = set(entry["source"]["sql_discovery"]["core_candidates"])
                entry["compiled_registry"]["missing_core_sql"] = sorted(core - names)
                entry["compiled_registry"]["core_sql_registered"] = len(core & names)
            query = ("SELECT function_type, count(*) AS overloads, "
                     "count(DISTINCT function_name) AS names FROM duckdb_functions() "
                     "GROUP BY function_type ORDER BY function_type")
            entry["function_catalog"] = json.loads(subprocess.check_output(
                [str(configuration.binary), ":memory:", "-json", "-c", query], text=True))
            entry["status"] = "inventoried_not_validated"
            print(target, entry["source"]["counts"], entry["compiled_registry"], flush=True)
        except Exception as error:
            # A missing executable must not erase a successfully enumerated source population.
            entry.update(status="setup_failure", error=str(error))
        (output / "inventory.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"report": str(output / "inventory.json"),
                      "rust_targets": len(report["rust"]["targets"]),
                      "rust_test_attributes": report["rust"]["test_attribute_declarations"]}))
    return int(any(e["status"] == "setup_failure" for e in report["targets"].values()))


if __name__ == "__main__":
    raise SystemExit(main())
