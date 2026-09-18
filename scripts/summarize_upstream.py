"""Reconcile immutable G01 first-pass and timeout-retry SQL campaign reports."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path

from reference_version import TARGETS


FIXTURE = "data/parquet-testing/orders_small_parquet.test"
IDENTITY_KEYS = ("rust_source_sha256", "rust_binary_sha256")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def counts(results):
    return {"status": dict(Counter(r["status"] for r in results)),
            "first_blocker": dict(Counter(r.get("failure_class", "passed") for r in results))}


def validate_identity(initial, retry, target):
    for key in IDENTITY_KEYS:
        if initial[key] != retry[key]: raise ValueError(f"{target}: {key} differs")
    left, right = initial["populations"][target]["identity"], retry["populations"][target]["identity"]
    for key in ("revision", "archive_sha256"):
        if left.get(key) != right.get(key): raise ValueError(f"{target}: pinned {key} differs")
    if left.get("revision") != TARGETS[target].revision:
        raise ValueError(f"{target}: report is not pinned to {TARGETS[target].revision}")


def merge_population(initial, retry, target):
    validate_identity(initial, retry, target)
    base, rerun = initial["populations"][target], retry["populations"][target]
    selected = {r["path"] for r in base["selected"]}
    original = {r["path"]: r for r in base["results"]}
    if len(selected) != len(base["selected"]) or len(original) != len(base["results"]) or set(original) != selected:
        raise ValueError(f"{target}: missing or duplicate initial ids")
    retry_paths, retry_results = {r["path"] for r in rerun["selected"]}, {r["path"]: r for r in rerun["results"]}
    timeouts = {p for p, r in original.items() if r.get("failure_class") == "timeout"}
    if (len(retry_paths) != len(rerun["selected"]) or retry_paths != timeouts
            or set(retry_results) != retry_paths or len(retry_results) != len(rerun["results"])):
        raise ValueError(f"{target}: retry ids are not exactly initial timeout ids")
    effective = [dict(retry_results.get(r["path"], r)) for r in base["results"]]
    fixture = next((r for r in effective if r["path"] == FIXTURE), None)
    if fixture is None: raise ValueError(f"{target}: known binary fixture is missing")
    executable = [r for r in effective if r["path"] != FIXTURE]
    for r in executable:
        if r.get("source_sql_records") == 0 and r.get("status") == "incomplete":
            r["status"] = "incomplete"
            controls = r.get("worker_requests", r.get("attempted_records", 0))
            r["failure_class"] = "controls_only" if controls else "no_sql_records"
    observed = {"passed_records": sum(r.get("passed_records", 0) for r in executable),
                "skipped_records": sum(r.get("skipped_records", 0) for r in executable),
                "source_sql_records": sum(r.get("source_sql_records", 0) for r in executable),
                "unreached_source_records": "unknown: historical reports combine source and expanded execution accounting",
                "attempted_sql_requests": "unknown: historical attempted_records counted worker transport requests"}
    return {"identity": base["identity"], "provenance": {"initial": {key: initial[key] for key in ("engine_git_revision", "rust_source_sha256", "rust_binary_sha256", "harness_sha256")}, "retry": {key: retry[key] for key in ("engine_git_revision", "rust_source_sha256", "rust_binary_sha256", "harness_sha256")}}, "candidate_file_count": len(base["selected"]), "executable_file_count": len(executable),
            "excluded": {"path": FIXTURE, "reason": "binary fixture misidentified by suffix discovery", "source_outcome": fixture},
            "initial": counts(base["results"]), "retry": counts(rerun["results"]), "effective": counts(executable),
            "observed_record_totals": observed, "results": executable}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--development", type=Path, required=True); parser.add_argument("--development-retry", type=Path, required=True)
    parser.add_argument("--release", type=Path, required=True); parser.add_argument("--release-retry", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True); args = parser.parse_args()
    inputs = {"development_initial": args.development, "development_retry": args.development_retry, "release_initial": args.release, "release_retry": args.release_retry}
    if args.output.exists(): raise FileExistsError("refusing to overwrite accounting artifact")
    reports = {name: json.loads(path.read_text()) for name, path in inputs.items()}
    result = {"full_parity": False, "scope": "Normalized G01 SQL campaign accounting. It does not claim full parity: upstream native/client/benchmark declarations, generated cases, configurations, platforms and external suites remain unexecuted.",
              "summary_script_sha256": digest(Path(__file__)),
              "raw_reports": {name: {"path": str(path), "sha256": digest(path)} for name, path in inputs.items()},
              "populations": {"development": merge_population(reports["development_initial"], reports["development_retry"], "development"), "release": merge_population(reports["release_initial"], reports["release_retry"], "release")}}
    args.output.parent.mkdir(parents=True, exist_ok=True); args.output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__": main()
