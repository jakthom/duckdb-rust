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
import re
import subprocess
import tempfile
import time

import measure_sqllogic_performance as measure
from reference_version import ROOT, TARGETS, require_checkout, require_reference
from upstream_suite import digest
from run_upstream import (checked_worker_provenance, selected_feedback_population)


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


def upstream_verdict(path, target, revision, workload, sample_id, source_sha256, suite_sha256, token_sha256, runner_sha256):
    report = json.loads(path.read_text())
    required = {"kind", "version", "status", "target", "revision", "workload_id", "sample_id", "path", "declarations",
                "passed_records", "skipped_records", "generated_records", "source_sha256", "suite_sha256", "token_sha256", "runner_binary_sha256"}
    if (set(report) != required or report.get("kind") != "duckdb-rust-selected-feedback" or report.get("version") != 1
            or report.get("target") != target or report.get("revision") != revision or report.get("workload_id") != workload["id"]
            or report.get("sample_id") != sample_id or report.get("path") != workload["path"]
            or report.get("source_sha256") != source_sha256
            or report.get("suite_sha256") != suite_sha256
            or report.get("token_sha256") != token_sha256 or report.get("runner_binary_sha256") != runner_sha256
            or report.get("status") != "passed" or not isinstance(report.get("passed_records"), int)
            or report["passed_records"] <= 0 or not isinstance(report.get("declarations"), int)
            or report["declarations"] <= 0 or report.get("skipped_records") != 0 or report.get("generated_records") != 0):
        raise ValueError("Rust feedback result did not validate every assertion")
    return report["passed_records"]


def rust_command(args, target, revision, workload, scratch, sample_id, source, suite, source_sha256, suite_sha256, provenance):
    token = scratch / f"{sample_id}-{target}-{workload['id']}.token.json"
    token_body = {"kind": "duckdb-rust-selected-feedback", "version": 1, "target": target, "revision": revision,
                  "workload_id": workload["id"], "sample_id": sample_id, "source_root": str(source), "path": workload["path"],
                  "source_sha256": source_sha256, "source_bytes": (source / workload["path"]).stat().st_size,
                  "suite_path": str(suite), "suite_sha256": suite_sha256, "runner_path": str(args.rust.resolve()),
                  "runner_binary_sha256": provenance["binary_sha256"], "runner_source_sha256": provenance["source_sha256"],
                  "runner_profile": "release", "provenance_sha256": digest(args.rust_provenance),
                  "timeout_ms": int(args.timeout * 1000)}
    token.write_text(json.dumps(token_body, sort_keys=True, separators=(",", ":")))
    token_sha256 = digest(token)
    report = scratch / f"{sample_id}-{target}-{workload['id']}.json"
    return [args.rust, "--feedback-token", token, "--token-sha256", token_sha256, "--feedback-report", report], report, token, token_sha256


def timed_rust(command, report, token, token_sha256, target, revision, workload, sample_id, source_sha256, suite_sha256, runner_sha256, timeout):
    sample = measure.run_timed(command, "feedback", execute=lambda *args, **kwargs: subprocess.run(*args, timeout=timeout, **kwargs))
    sample["records"] = upstream_verdict(report, target, revision, workload, sample_id, source_sha256, suite_sha256, token_sha256, runner_sha256)
    sample["token_sha256"] = token_sha256; sample["token"] = json.loads(token.read_text()); sample["compiled_report"] = json.loads(report.read_text())
    return sample


def gate(raw, workloads):
    release = {"workloads": [{**workload, "cpp": item["observations"]["release_cpp"],
                               "rust": item["observations"]["release_rust"]}
                            for workload, item in zip(workloads, raw)]}
    development = {"workloads": [{**workload, "cpp": item["observations"]["development_cpp"],
                                   "rust": item["observations"]["development_rust"]}
                                for workload, item in zip(workloads, raw)]}
    return measure.gate(release, development, workloads)


def failure_diagnostic(error):
    """Preserve failed timed-process evidence rather than only its summary."""
    if isinstance(error, measure.SampleFailure):
        return error.details
    return None


def campaign_snapshot(args, rust, references, prepared):
    """All authority outside a timed child; equality makes drift fail closed."""
    runner_sidecar = args.rust_provenance.resolve(strict=True)
    _, provenance = checked_worker_provenance(rust, runner_sidecar)
    pins = {}
    for target, reference in references.items():
        source = TARGETS[target].source
        revision = require_checkout(source, target)
        binary = Path(reference["unittest"]).resolve(strict=True)
        build = binary.parent.parent
        cache = measure.release_cache(build)
        pins[target] = {"revision": revision, "unittest_sha256": digest(binary), "cmake_cache_sha256": digest(cache)}
    caches = {}
    for (workload_id, target), value in prepared.items():
        source, suite, expected_source, expected_suite, identity, path = value
        source_file = source / path
        # Re-read every selected byte at both boundaries. The expected digest
        # comes from the cache materialization; this is not a copied snapshot.
        actual_source, actual_suite = digest(source_file), digest(suite)
        if actual_source != expected_source or actual_suite != expected_suite:
            raise ValueError(f"selected feedback cache drifted: {workload_id}/{target}")
        caches[f"{workload_id}/{target}"] = {"path": path, "source_sha256": actual_source,
                                               "suite_sha256": actual_suite, "identity": identity}
    return {"workloads_sha256": digest(args.workloads), "harness_sha256": {name: digest(ROOT / "scripts" / name) for name in
            ("measure_upstream_feedback.py", "run_upstream.py", "upstream_suite.py")}, "runner_sha256": digest(rust),
            "sidecar_sha256": digest(runner_sidecar), "provenance": provenance, "pins": pins, "caches": caches}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workloads", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--rust-provenance", type=Path,
                        help="matching run_upstream prebuilt-worker provenance sidecar")
    parser.add_argument("--release-cpp", type=Path, default=TARGETS["release"].build / "test/unittest")
    parser.add_argument("--development-cpp", type=Path, default=TARGETS["development"].build / "test/unittest")
    parser.add_argument("--release-build", type=Path, default=TARGETS["release"].build)
    parser.add_argument("--development-build", type=Path, default=TARGETS["development"].build)
    parser.add_argument("--suite-cache", type=Path, default=ROOT / "target/upstream-suite-cache")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=21)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=10)
    args = parser.parse_args()
    if args.rust_provenance is None:
        args.rust_provenance = Path(str(args.rust) + ".provenance.json")
    if args.report.exists(): raise FileExistsError("preserve prior evidence: choose a new report path")
    cache_run_root = args.suite_cache / args.report.stem
    if cache_run_root.exists(): raise FileExistsError("choose a fresh suite-cache/report identity for cold setup evidence")
    if args.samples < 9 or args.samples % 2 != 1 or args.warmups != 3 or args.timeout <= 0:
        raise ValueError("use three warmups, odd >=9 samples, and positive timeout")
    if measure.active_peers(): raise RuntimeError("quiet host required for feedback acceptance")
    raw, references = [], {}
    workloads = validate_manifest(args.workloads)
    canonical_rust = (ROOT / "target/release/sqllogictest").resolve(strict=True)
    rust = args.rust.resolve(strict=True)
    if rust != canonical_rust or args.rust_provenance.resolve(strict=True) != Path(str(canonical_rust) + ".provenance.json").resolve(strict=True):
        raise ValueError("feedback acceptance requires canonical target/release/sqllogictest and sidecar")
    # Full source/binary provenance belongs to campaign setup, matching the
    # pinned C++ build/source checks below. Timed invocations still validate the
    # exact cache metadata and selected source bytes through their token.
    provenance_path, provenance_before = checked_worker_provenance(rust, args.rust_provenance)
    for target, binary, build in (("release", args.release_cpp, args.release_build),
                                  ("development", args.development_cpp, args.development_build)):
        source = TARGETS[target].source
        build = build.resolve(strict=True)
        revision = require_checkout(source, target)
        _, cli = require_reference(TARGETS[target].binary, target=target)
        cache = measure.release_cache(build)
        home = re.search(r"(?m)^CMAKE_HOME_DIRECTORY:INTERNAL=(.+)$", cache.read_text(errors="replace"))
        if not home or Path(home.group(1)).resolve() != source.resolve():
            raise ValueError(f"{target} CMake build is not source-mapped to its pinned checkout")
        binary = binary.resolve(strict=True)
        expected_binary = (build / "test/unittest").resolve(strict=True)
        if binary != expected_binary:
            raise ValueError(f"{target} unittest must be the binary from its pinned release build")
        references[target] = {"revision": revision, "cli": cli, "unittest": str(binary),
                              "unittest_sha256": digest(binary), "cmake_cache_sha256": digest(cache)}
    snapshot_before = None
    try:
        with tempfile.TemporaryDirectory(prefix="ddb-feedback-performance-") as directory:
            scratch = Path(directory)
            prepared_all = {}
            cold_materialization = {}
            # Materialize all selected singleton caches before the timed phase,
            # then snapshot every one together. This one-time setup is retained
            # separately rather than charged to a warm invocation.
            for workload in workloads:
                for target in ("release", "development"):
                    cache = cache_run_root / workload["id"] / target
                    started = time.perf_counter_ns()
                    source, manifest, identity = selected_feedback_population(target, [workload["path"]], cache)
                    wall_ns = time.perf_counter_ns() - started
                    suite = source.parent / "suite.json"; source_file = source / workload["path"]
                    prepared_all[(workload["id"], target)] = (source, suite, digest(source_file), digest(suite), identity, workload["path"])
                    cold_materialization[f"{workload['id']}/{target}"] = {"wall_ns": wall_ns, "cache": identity}
            snapshot_before = campaign_snapshot(args, rust, references, prepared_all)
            for workload in workloads:
                observations = {name: [] for name in ("release_cpp", "development_cpp", "release_rust", "development_rust")}
                cold_setup = {}
                commands = {}
                prepared = {}
                for target, binary in (("release", args.release_cpp), ("development", args.development_cpp)):
                    commands[f"{target}_cpp"] = [binary, "--test-dir", TARGETS[target].source,
                                                   workload["path"], "--use-colour", "no", "--durations", "no"]
                for target in ("release", "development"):
                    source, suite, source_sha256, suite_sha256, identity, _ = prepared_all[(workload["id"], target)]
                    prepared[target] = (source, suite, source_sha256, suite_sha256, identity)
                    cold_setup[target] = cold_materialization[f"{workload['id']}/{target}"]
                for iteration in range(args.warmups + args.samples):
                    names = ["release_cpp", "development_cpp", "release_rust", "development_rust"]
                    names = names[iteration % len(names):] + names[:iteration % len(names)]
                    for name in names:
                        if name.endswith("_cpp"):
                            sample = measure.run_timed(commands[name], "cpp")
                        else:
                            target = name.removesuffix("_rust")
                            source, suite, source_sha256, suite_sha256, identity = prepared[target]
                            sample_id = f"{workload['id']}-round-{iteration}"
                            command, result_report, token, token_sha256 = rust_command(args, target, identity["revision"], workload, scratch, sample_id, source, suite, source_sha256, suite_sha256, provenance_before)
                            sample = timed_rust(command, result_report, token, token_sha256, target, identity["revision"], workload, sample_id, source_sha256, suite_sha256, provenance_before["binary_sha256"], args.timeout)
                        if iteration >= args.warmups: observations[name].append(sample)
                raw.append({**workload, "cold_setup": cold_setup, "observations": observations})
        provenance_path_after, provenance_after = checked_worker_provenance(rust, args.rust_provenance)
        snapshot_after = campaign_snapshot(args, rust, references, prepared_all)
        if provenance_path_after != provenance_path or provenance_after != provenance_before or snapshot_after != snapshot_before:
            raise ValueError("feedback campaign authority changed during campaign")
        result = gate(raw, workloads)
        report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "workloads": raw, "gate": result,
                  "passed": result["passed"], "references": references, "rust_binary": str(rust),
                  "rust_binary_sha256": digest(rust), "workloads_sha256": digest(args.workloads),
                  "rust_provenance_before": provenance_before, "rust_provenance_after": provenance_after,
                  "rust_provenance_path": str(provenance_path), "stale_source": False,
                  "preflight": snapshot_before, "postflight": snapshot_after,
                  "harness_sha256": {name: digest(ROOT / "scripts" / name) for name in
                                      ("measure_upstream_feedback.py", "run_upstream.py", "upstream_suite.py")},
                  "samples": args.samples, "warmups": args.warmups,
                  "scope": "Selected upstream SQLLogic source paths. Gate P times repeated warm feedback: cache validation, worker launch, execution, unchanged assertions, report emission and caller-visible process startup, matched to pinned C++ unittest startup/execution/assertions over its materialized source. `cold_setup` retains one-time cache materialization wall time and identity; its CPU, RSS and I/O dimensions are not collected and it is not gated. Compilation is excluded."}
    except Exception as error:
        report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "workloads": raw, "references": references,
                  "passed": False, "error": str(error)}
        diagnostic = failure_diagnostic(error)
        if diagnostic is not None:
            report["failed_run"] = diagnostic
    args.report.parent.mkdir(parents=True, exist_ok=True); args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "report": str(args.report), "error": report.get("error")}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__": main()
