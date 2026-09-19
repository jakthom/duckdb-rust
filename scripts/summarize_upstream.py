"""Reconcile G01 reports and classify immutable baseline/current snapshots."""
import argparse
from collections import Counter
from dataclasses import dataclass
import hashlib
import json
import math
from pathlib import Path

from reference_version import TARGETS

FIXTURE = "data/parquet-testing/orders_small_parquet.test"
IDENTITY_KEYS = ("rust_source_sha256", "rust_binary_sha256")
CONFIG_KEYS = ("campaign_kind", "worker_profile", "timeout_seconds", "execution_mode")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def canonical_digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def counts(results):
    return {"status": dict(Counter(r["status"] for r in results)), "first_blocker": dict(Counter(r.get("failure_class", "passed") for r in results))}


def _unique(rows, name, target):
    got = {row.get("path"): row for row in rows}
    if None in got or len(got) != len(rows): raise ValueError(f"{target}: missing or duplicate {name} ids")
    return got


def validate_identity(initial, retry, target):
    for key in IDENTITY_KEYS:
        if initial[key] != retry[key]: raise ValueError(f"{target}: {key} differs")
    left, right = initial["populations"][target]["identity"], retry["populations"][target]["identity"]
    for key in ("revision", "archive_sha256"):
        if left.get(key) != right.get(key): raise ValueError(f"{target}: pinned {key} differs")
    if left.get("revision") != TARGETS[target].revision: raise ValueError(f"{target}: report is not pinned to {TARGETS[target].revision}")


def merge_population(initial, retry, target):
    """Merge a first pass with its exact timeout retry; retry may be absent only for no timeouts."""
    base = initial["populations"][target]
    if base["identity"].get("revision") != TARGETS[target].revision:
        raise ValueError(f"{target}: initial report is not pinned")
    selected, original = _unique(base["selected"], "initial selected", target), _unique(base["results"], "initial result", target)
    if set(selected) != set(original): raise ValueError(f"{target}: missing or duplicate initial ids")
    timeouts = {path for path, row in original.items() if row.get("failure_class") == "timeout"}
    if retry is None:
        if timeouts: raise ValueError(f"{target}: retry report required for initial timeout ids")
        rerun, retry_results = {"results": []}, {}
    else:
        validate_identity(initial, retry, target); rerun = retry["populations"][target]
        retry_selected, retry_results = _unique(rerun["selected"], "retry selected", target), _unique(rerun["results"], "retry result", target)
        if set(retry_selected) != timeouts or set(retry_results) != timeouts: raise ValueError(f"{target}: retry ids are not exactly initial timeout ids")
    effective = [dict(retry_results.get(row["path"], row)) for row in base["results"]]
    fixture = next((row for row in effective if row["path"] == FIXTURE), None)
    if fixture is None: raise ValueError(f"{target}: known binary fixture is missing")
    executable = [row for row in effective if row["path"] != FIXTURE]
    for row in executable:
        if row.get("source_sql_records") == 0 and row.get("status") == "incomplete":
            row["failure_class"] = "controls_only" if row.get("worker_requests", row.get("attempted_records", 0)) else "no_sql_records"
    observed = {"passed_records": sum(r.get("passed_records", 0) for r in executable), "skipped_records": sum(r.get("skipped_records", 0) for r in executable), "source_sql_records": sum(r.get("source_sql_records", 0) for r in executable), "unreached_source_records": "unknown: historical reports combine source and expanded execution accounting", "attempted_sql_requests": "unknown: historical attempted_records counted worker transport requests"}
    fields = ("engine_git_revision", "rust_source_sha256", "rust_binary_sha256", "harness_sha256")
    return {"identity": base["identity"], "provenance": {"initial": {k: initial[k] for k in fields}, "retry": None if retry is None else {k: retry[k] for k in fields}}, "candidate_file_count": len(base["selected"]), "executable_file_count": len(executable), "excluded": {"path": FIXTURE, "reason": "binary fixture misidentified by suffix discovery", "source_outcome": fixture}, "initial": counts(base["results"]), "retry": counts(rerun["results"]), "effective": counts(executable), "observed_record_totals": observed, "results": executable}


def _configuration(report, population):
    values = {key: population.get(key, report.get(key)) for key in CONFIG_KEYS}
    values["execution_mode"] = values["execution_mode"] or "ordinary"
    missing = [key for key, value in values.items() if value is None and key != "selection_kind"]
    if missing: raise ValueError("missing runtime configuration: " + ", ".join(missing))
    return values, canonical_digest(values)


def _selection(report, population):
    rows = population.get("selected")
    if not isinstance(rows, list): raise ValueError("selected population is missing")
    paths = [row.get("path") for row in rows]
    if any(not isinstance(path, str) or not path or Path(path).is_absolute() or ".." in Path(path).parts for path in paths): raise ValueError("unsafe selected path")
    if len(set(paths)) != len(paths): raise ValueError("duplicate selected path")
    list_digest = population.get("path_list_content_sha256", report.get("path_list_content_sha256"))
    if report.get("path_list") and not list_digest: raise ValueError("path-list content digest is required")
    return canonical_digest({"paths": paths, "prefixes": report.get("path_prefixes", []), "path_list_content_sha256": list_digest, "selection_kind": population.get("selection_kind", report.get("selection_kind"))})


@dataclass(frozen=True)
class Observation:
    path: str; pin: str; configuration: str; status: str; selection: str; population: str; report: str; stale: tuple; provenance: tuple
    elapsed_seconds: object = None
    @property
    def key(self): return (self.path, self.pin, self.configuration)


def observations(report, *, requested_identity=None):
    """Validate one report locally. Baseline/current Rust identities may intentionally differ."""
    if not isinstance(report, dict) or not isinstance(report.get("populations"), dict): raise ValueError("malformed report")
    if not report["populations"] or set(report["populations"]) - set(TARGETS):
        raise ValueError("missing or unknown reference populations")
    for key in (*IDENTITY_KEYS, "engine_git_revision"):
        if not isinstance(report.get(key), str) or not report[key]:
            raise ValueError(f"missing report identity: {key}")
    if not isinstance(report.get("harness_sha256"), dict) or not report["harness_sha256"]:
        raise ValueError("missing harness identity")
    output, report_digest = [], canonical_digest(report)
    for target in TARGETS:
        if target not in report["populations"]: continue
        population = report["populations"][target]
        config, config_digest = _configuration(report, population)
        selected, rows = _unique(population.get("selected", []), "selected", target), _unique(population.get("results", []), "result", target)
        if set(selected) != set(rows): raise ValueError(f"{target}: selected/result population differs")
        identity = population.get("identity")
        if not isinstance(identity, dict) or identity.get("revision") != TARGETS[target].revision:
            raise ValueError(f"{target}: malformed population identity")
        file_identities = None
        if identity.get("kind") == "selected_feedback":
            file_identities = _unique(identity.get("files", []), "selected file identity", target)
            if set(file_identities) != set(selected) or any(item.get("kind") != "file" or not item.get("sha256") for item in file_identities.values()):
                raise ValueError(f"{target}: missing selected file identity")
        elif not identity.get("archive_sha256"):
            raise ValueError(f"{target}: missing archive identity")
        stable_population = {key: identity[key] for key in ("revision", "archive_sha256", "manifest_sha256", "source_tree_sha256") if key in identity}
        stale = []
        if report.get("stale_source") or population.get("stale_source"): stale.append("stale_source")
        if report.get("source_fingerprint_before") is not None and report.get("source_fingerprint_before") != report.get("source_fingerprint_after"): stale.append("source_fingerprint")
        if requested_identity and any(report.get(k) != v for k, v in requested_identity.items()): stale.append("requested_current_identity")
        provenance = (canonical_digest(config), _selection(report, population), canonical_digest(identity), report_digest, report.get("engine_git_revision"), report.get("rust_source_sha256"), report.get("rust_binary_sha256"), canonical_digest(report.get("harness_sha256", {})))
        for path, row in rows.items():
            if row.get("status") not in {"passed", "failed", "incomplete"}:
                raise ValueError(f"{target}: invalid result status")
            elapsed = row.get("elapsed_seconds")
            if elapsed is not None and (isinstance(elapsed, bool) or not isinstance(elapsed, (int, float)) or not math.isfinite(elapsed) or elapsed < 0):
                raise ValueError(f"{target}: invalid elapsed seconds")
            population_digest = canonical_digest({"revision": identity["revision"], "file": file_identities[path]}) if file_identities is not None else canonical_digest(stable_population)
            if path != FIXTURE: output.append(Observation(path, target, config_digest, row["status"], provenance[1], population_digest, report_digest, tuple(stale), provenance, elapsed))
    if len({row.key for row in output}) != len(output): raise ValueError("duplicate (path, pin, configuration) observation")
    return tuple(sorted(output, key=lambda row: row.key))


def classify_incremental(baseline, current, *, requested_current_identity=None):
    """Return disjoint baseline/current classifications without summing refreshes."""
    old, new = {row.key: row for row in observations(baseline)}, {row.key: row for row in observations(current, requested_identity=requested_current_identity)}
    rows = []
    # A changed runtime configuration is evidence of a different operation,
    # not two outcome transitions. Pair the sole unmatched configuration for a
    # case/pin and retain it as one explicit uncomparable row.
    old_by_case, new_by_case = {}, {}
    for item in old.values(): old_by_case.setdefault((item.path, item.pin), []).append(item)
    for item in new.values(): new_by_case.setdefault((item.path, item.pin), []).append(item)
    config_pairs = {}
    for case in set(old_by_case) & set(new_by_case):
        left = [item for item in old_by_case[case] if item.key not in new]
        right = [item for item in new_by_case[case] if item.key not in old]
        if len(left) == len(right) == 1:
            config_pairs[left[0].key] = right[0]
    paired_current = {row.key for row in config_pairs.values()}
    for key in sorted(set(old) | set(new)):
        left, right = old.get(key), new.get(key)
        if left is None and key in paired_current: continue
        if left is not None and key in config_pairs:
            right, kind, fresh, lost = config_pairs[key], "uncomparable_configuration", False, False
        elif left is None: kind, fresh, lost = "new_current_only", False, False
        elif right is None: kind, fresh, lost = ("lost_pass_omitted" if left.status == "passed" else "baseline_only_omitted"), False, left.status == "passed"
        elif left.stale or right.stale: kind, fresh, lost = "stale", False, False
        elif left.population != right.population: kind, fresh, lost = "uncomparable_population", False, False
        elif left.status == "passed" and right.status != "passed": kind, fresh, lost = "fresh_failure", True, True
        elif left.status == right.status: kind, fresh, lost = "unchanged_" + right.status, False, False
        else: kind, fresh, lost = "changed_nonpass", False, False
        rows.append({"path": key[0], "pin": key[1], "runtime_configuration_sha256": key[2], "classification": kind, "fresh_failure": fresh, "lost_pass": lost, "baseline_status": None if left is None else left.status, "current_status": None if right is None else right.status, "stale_reasons": sorted(set((left.stale if left else ()) + (right.stale if right else ()))), "baseline_provenance": None if left is None else left.provenance, "current_provenance": None if right is None else right.provenance,
                     "baseline_elapsed_seconds": None if left is None else left.elapsed_seconds,
                     "current_elapsed_seconds": None if right is None else right.elapsed_seconds})
    return {"rows": rows, "counts": dict(Counter(row["classification"] for row in rows)), "fresh_failures": sum(row["fresh_failure"] for row in rows), "lost_passes": sum(row["lost_pass"] for row in rows),
            "elapsed_stages": {"baseline": elapsed_stages(baseline), "current": elapsed_stages(current)}}


def elapsed_stages(report):
    """Keep measured stage scopes separate; absent historical timings stay unknown."""
    result = {"report": report.get("elapsed_stages"),
              "populations": {pin: population.get("elapsed_stages") for pin, population in report["populations"].items()}}
    for stages in (result["report"], *result["populations"].values()):
        if stages is not None and (not isinstance(stages, dict) or any(isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0 for value in stages.values())):
            raise ValueError("invalid elapsed stage measurement")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--development", type=Path); parser.add_argument("--development-retry", type=Path); parser.add_argument("--release", type=Path); parser.add_argument("--release-retry", type=Path)
    parser.add_argument("--baseline", type=Path); parser.add_argument("--current", type=Path); parser.add_argument("--output", type=Path, required=True); args = parser.parse_args()
    if args.output.exists(): raise FileExistsError("refusing to overwrite accounting artifact")
    if bool(args.baseline) != bool(args.current): raise ValueError("--baseline and --current are paired")
    if args.baseline:
        output = {"full_parity": False, "scope": "Incremental G01 case/pin/runtime-configuration accounting.", "summary_script_sha256": digest(Path(__file__)), "baseline": {"path": str(args.baseline), "sha256": digest(args.baseline)}, "current": {"path": str(args.current), "sha256": digest(args.current)}, "incremental": classify_incremental(json.loads(args.baseline.read_text()), json.loads(args.current.read_text()))}
    else:
        paths = {"development_initial": args.development, "development_retry": args.development_retry, "release_initial": args.release, "release_retry": args.release_retry}
        if not paths["development_initial"] or not paths["release_initial"]: raise ValueError("initial reports are required")
        reports = {name: None if path is None else json.loads(path.read_text()) for name, path in paths.items()}
        output = {"full_parity": False, "scope": "Normalized G01 SQL campaign accounting.", "summary_script_sha256": digest(Path(__file__)), "raw_reports": {name: None if path is None else {"path": str(path), "sha256": digest(path)} for name, path in paths.items()}, "populations": {target: merge_population(reports[target + "_initial"], reports[target + "_retry"], target) for target in TARGETS}}
    args.output.parent.mkdir(parents=True, exist_ok=True); args.output.write_text(json.dumps(output, indent=2) + "\n")


if __name__ == "__main__": main()
