"""Gate G01.4a's selected-suite feedback boundary against both pinned runners.

Each timed Rust invocation validates the extracted suite cache, launches the
prebuilt Rust worker, and writes a complete selected-suite report.  The matching
C++ invocation is the pinned source tree's ``unittest`` SQLLogicTest runner for
the same unchanged path.  Compilation is deliberately outside the timed public
operation; callers must build both runners before declaring the host quiet.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import subprocess
import tempfile

import measure_sqllogic_performance as measure
from reference_version import ROOT, TARGETS, require_checkout, require_reference
from upstream_suite import digest


def validate_manifest(path):
    data = json.loads(path.read_text())
    workloads = data.get("workloads")
    if not isinstance(workloads, list) or not workloads:
        raise ValueError("workload manifest must contain workloads")
    result, identifiers = [], set()
    for item in workloads:
        if set(item) != {"id", "path"} or not all(isinstance(item[key], str) and item[key] for key in item):
            raise ValueError("workloads require only nonempty id and source path")
        if item["id"] in identifiers or Path(item["path"]).is_absolute() or ".." in Path(item["path"]).parts:
            raise ValueError("duplicate or unsafe workload")
        identifiers.add(item["id"])
        sources = {}
        for target, config in TARGETS.items():
            candidate = config.source / item["path"]
            if not candidate.is_file():
                raise ValueError(f"{target} source lacks workload {item['path']}")
            sources[target] = digest(candidate)
        result.append({**item, "source_sha256": sources})
    return result


def upstream_verdict(path, target, expected):
    report = json.loads(path.read_text())
    if report.get("stale_source") or report.get("worker_profile") != "release":
        raise ValueError("stale or non-release Rust feedback run")
    population = report.get("populations", {}).get(target)
    if not population or population.get("sql_files_selected") != 1:
        raise ValueError("Rust feedback report has an incomplete selection")
    selected = population.get("selected")
    results = population.get("results")
    if selected != [{"id": expected, "kind": "sqllogictest", "path": expected, "line": 1}] or len(results) != 1:
        raise ValueError("Rust feedback report selected a different source case")
    if results[0].get("status") != "passed" or not results[0].get("passed_records"):
        raise ValueError("Rust feedback result did not validate every assertion")
    return results[0]["passed_records"]


def rust_command(args, target, workload, scratch, sample_id):
    selected = scratch / f"{sample_id}-{target}-{workload['id']}.paths"
    selected.write_text(workload["path"] + "\n")
    report = scratch / f"{sample_id}-{target}-{workload['id']}.json"
    return ["python3", ROOT / "scripts/run_upstream.py", "--worker", args.rust,
            "--target", target, "--path-list", selected, "--jobs", "1", "--timeout", str(args.timeout),
            "--suite-cache", scratch / "cold-suite-cache" / sample_id / target,
            "--report", report], report


def timed_rust(command, report, target, workload):
    sample = measure.run_timed(command, "feedback", execute=subprocess.run)
    sample["records"] = upstream_verdict(report, target, workload["path"])
    return sample


def gate(raw, workloads):
    release = {"workloads": [{**workload, "cpp": item["observations"]["release_cpp"],
                               "rust": item["observations"]["release_rust"]}
                            for workload, item in zip(workloads, raw)]}
    development = {"workloads": [{**workload, "cpp": item["observations"]["development_cpp"],
                                   "rust": item["observations"]["development_rust"]}
                                for workload, item in zip(workloads, raw)]}
    return measure.gate(release, development, workloads)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workloads", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--release-cpp", type=Path, default=TARGETS["release"].build / "test/unittest")
    parser.add_argument("--development-cpp", type=Path, default=TARGETS["development"].build / "test/unittest")
    parser.add_argument("--suite-cache", type=Path, default=ROOT / "target/upstream-suite-cache")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=21)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=10)
    args = parser.parse_args()
    if args.report.exists(): raise FileExistsError("preserve prior evidence: choose a new report path")
    if args.samples < 9 or args.samples % 2 != 1 or args.warmups != 3 or args.timeout <= 0:
        raise ValueError("use three warmups, odd >=9 samples, and positive timeout")
    if measure.active_peers(): raise RuntimeError("quiet host required for feedback acceptance")
    raw, references = [], {}
    workloads = validate_manifest(args.workloads)
    rust = args.rust.resolve(strict=True)
    for target, binary in (("release", args.release_cpp), ("development", args.development_cpp)):
        source, build = TARGETS[target].source, TARGETS[target].build
        revision = require_checkout(source, target)
        _, cli = require_reference(TARGETS[target].binary, target=target)
        cache = measure.release_cache(build)
        binary = binary.resolve(strict=True)
        expected_binary = (build / "test/unittest").resolve(strict=True)
        if binary != expected_binary:
            raise ValueError(f"{target} unittest must be the binary from its pinned release build")
        references[target] = {"revision": revision, "cli": cli, "unittest": str(binary),
                              "unittest_sha256": digest(binary), "cmake_cache_sha256": digest(cache)}
    try:
        with tempfile.TemporaryDirectory(prefix="ddb-feedback-performance-") as directory:
            scratch = Path(directory)
            for workload in workloads:
                observations = {name: [] for name in ("release_cpp", "development_cpp", "release_rust", "development_rust")}
                commands = {}
                for target, binary in (("release", args.release_cpp), ("development", args.development_cpp)):
                    commands[f"{target}_cpp"] = [binary, "--test-dir", TARGETS[target].source,
                                                   workload["path"], "--use-colour", "no", "--durations", "no"]
                for iteration in range(args.warmups + args.samples):
                    names = ["release_cpp", "development_cpp", "release_rust", "development_rust"]
                    names = names[iteration % len(names):] + names[:iteration % len(names)]
                    for name in names:
                        if name.endswith("_cpp"):
                            sample = measure.run_timed(commands[name], "cpp")
                        else:
                            target = name.removesuffix("_rust")
                            command, result_report = rust_command(args, target, workload, scratch, f"{workload['id']}-round-{iteration}")
                            sample = timed_rust(command, result_report, target, workload)
                        if iteration >= args.warmups: observations[name].append(sample)
                raw.append({**workload, "observations": observations})
        result = gate(raw, workloads)
        report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "workloads": raw, "gate": result,
                  "passed": result["passed"], "references": references, "rust_binary": str(rust),
                  "rust_binary_sha256": digest(rust), "workloads_sha256": digest(args.workloads),
                  "samples": args.samples, "warmups": args.warmups,
                  "scope": "Selected upstream SQLLogic source paths. Every timed Rust sample has a fresh worktree-local cache, so timing includes cache validation and suite materialization, plus worker launch, execution, unchanged assertions, report emission and caller-visible process startup; C++ timing includes its matching pinned unittest runner startup, execution and assertions. Compilation is excluded."}
    except Exception as error:
        report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "workloads": raw, "references": references,
                  "passed": False, "error": str(error)}
    args.report.parent.mkdir(parents=True, exist_ok=True); args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "report": str(args.report), "error": report.get("error")}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__": main()
