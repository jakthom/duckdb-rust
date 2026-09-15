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
from compiled_registry import enumerate_registry, source_matrix_definitions, source_parameterizations
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


def native_registry(configuration, output_dir, runner=None):
    return enumerate_registry(configuration.source,
                              configuration.build / "test/unittest" if runner is None else runner, output_dir)


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
    parser.add_argument("--development-runner", type=Path,
                        help="actual development unittest binary; its adjacent CMakeCache must source-match")
    parser.add_argument("--release-runner", type=Path,
                        help="actual release unittest binary; its adjacent CMakeCache must source-match")
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(),
              "host": platform.platform(), "machine": platform.machine(),
              "script_sha256": digest(Path(__file__)), "rust": rust_inventory(), "targets": {},
              "performance_gate": {
                  "status": "open",
                  "reason": "This Python inventory invokes the C++ registry and has no equivalent Rust registry candidate; timing it against either reference would be circular and cannot establish at-parity performance.",
                  "reference_workloads": [
                      {"id": "compiled-registry-development", "target": "development", "command": ["unittest", "*", "--list-tests"]},
                      {"id": "compiled-registry-release", "target": "release", "command": ["unittest", "*", "--list-tests"]},
                  ],
                  "required_candidate_contract": "Complete source-matched Catch listing with identical unique-ID and SQL accounting.",
                  "required_measurement": "quiet host; three warmups; 21 samples; wall time, process CPU, peak RSS, and rusage inblock+oublock; candidate median <= faster reference and candidate throughput >= larger reference throughput.",
              },
              "unmeasured": ["runtime generated/parameterized invocations (Catch list protocol does not expose them)", "full configuration/platform matrix",
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
            entry["source"]["parameterized_generated"] = source_parameterizations(configuration.source)
            entry["source"]["matrix_definitions"] = source_matrix_definitions(configuration.source)
            entry["build"] = build_identity(target)
            runner = getattr(args, f"{target}_runner")
            entry["compiled_registry"] = native_registry(configuration, directory, runner)
            if entry["compiled_registry"]["status"] != "enumerated_not_executed":
                raise ValueError(entry["compiled_registry"].get("reason", "compiled registry unavailable"))
            query = ("SELECT function_type, count(*) AS overloads, "
                     "count(DISTINCT function_name) AS names FROM duckdb_functions() "
                     "GROUP BY function_type ORDER BY function_type")
            entry["function_catalog"] = json.loads(subprocess.check_output(
                [str(configuration.binary), ":memory:", "-json", "-c", query], text=True))
            entry["status"] = "inventoried_not_validated"
            registry = entry["compiled_registry"]
            print(target, entry["source"]["counts"], {
                "status": registry["status"], "names": registry["names"],
                "hidden_cases": registry["hidden_cases"], "sql_file_cases": registry["sql_file_cases"],
            }, flush=True)
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
