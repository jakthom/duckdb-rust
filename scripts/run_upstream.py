"""Measure exact upstream SQLLogicTest populations against the Rust worker.

This keeps upstream assertions unchanged. A non-passing file gets its first
blocking outcome and source location, allowing retries without overwriting it.
"""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

import sqllogic
from reference_version import ROOT, TARGETS, require_checkout
from upstream_suite import DESTINATION, REVISION, declarations, digest, verify

REQUIRED_SCOPES = ("compiled_registry", "generated_cases", "configurations", "platforms", "external_suites")
HARNESS_WORDS = ("condition ", "test directive", "loop ", "external expected", "reserved harness", "load options", " options", "record limit", "halt leaves", "regular-expression", "require ", "mode ", "concurrent")


def summarize(sql, selected, results, unported, obligations):
    """A result for every selected source id is required; a pass count is insufficient."""
    identities = lambda xs: {x["id"] for x in xs}
    selected_ids, result_ids = identities(selected), identities(results)
    selection_passed = (bool(selected) and len(selected_ids) == len(selected) and len(result_ids) == len(results)
                        and selected_ids == result_ids and all(r["status"] == "passed" for r in results))
    sql_passed = selection_passed and len(identities(sql)) == len(sql) and identities(sql) == selected_ids
    scopes_passed = set(obligations) == set(REQUIRED_SCOPES) and all(v == "passed" for v in obligations.values())
    return {"outcomes": dict(Counter(r["status"] for r in results)),
            "failure_classes": dict(Counter(r.get("failure_class") for r in results if r.get("failure_class"))),
            "sql_selection_passed": selection_passed, "sql_suite_passed": sql_passed,
            "full_suite_passed": sql_passed and not unported and scopes_passed}


class RustEngine:
    def __init__(self, binary, directory, deadline):
        self.errors = tempfile.TemporaryFile(mode="w+t")
        self.process = subprocess.Popen([str(binary)], cwd=directory, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.errors, text=True, bufsize=1)
        self.events = selectors.DefaultSelector(); self.events.register(self.process.stdout, selectors.EVENT_READ)
        self.deadline = deadline; self.engine_unsupported_seen = False; self.worker_requests = 0; self.sql_requests = 0
    def request(self, request):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0: raise TimeoutError("file deadline exceeded")
        # Count only requests that are actually handed to the worker. SQL attempts
        # exclude runner transport controls such as restart and reconnect.
        nested = [item for stream in request.get("streams", []) for item in stream]
        self.worker_requests += 1 + len(nested)
        self.sql_requests += request.get("operation") in ("query", "statement")
        self.sql_requests += sum(item.get("operation") in ("query", "statement") for item in nested)
        self.process.stdin.write(json.dumps(request) + "\n"); self.process.stdin.flush()
        if not self.events.select(timeout=remaining): raise TimeoutError("file deadline exceeded")
        line = self.process.stdout.readline()
        if not line:
            self.errors.seek(0); raise RuntimeError("worker exited: " + self.errors.read()[:2000])
        response = json.loads(line)
        self.engine_unsupported_seen = bool(response.get("unsupported"))
        return response
    def close(self):
        if self.process.poll() is None: self.process.kill()
        self.process.wait(); self.process.stdin.close(); self.process.stdout.close(); self.events.close(); self.errors.close()


def failure_class(error, engine_unsupported_seen=False, phase="execution"):
    reason = str(error)
    if phase == "parse": return "harness_parse"
    if isinstance(error, TimeoutError): return "timeout"
    if isinstance(error, (RuntimeError, BrokenPipeError)) and (isinstance(error, BrokenPipeError) or reason.startswith("worker exited:")): return "crash"
    if isinstance(error, sqllogic.Unsupported):
        if engine_unsupported_seen: return "engine_unsupported"
        return "harness_directive_or_oracle"
    if isinstance(error, (AssertionError, ValueError)): return "assertion_or_error_mismatch"
    return "setup"


def evidence(records, line):
    record = next((r for r in records if r.line == line and r.words[0] in ("query", "statement")), None)
    return {"line": line, "sql": record.sql[:2000] if record else None}


def run_case(binary, source, entry, timeout):
    start = time.monotonic(); runner = engine = None; records = []; phase = "parse"
    result = {"id": entry["id"], "path": entry["path"], "passed_records": 0, "skipped_records": 0,
              "attempted_records": 0, "unreached_source_records": None}
    with tempfile.TemporaryDirectory(prefix="ddb-upstream-case-") as scratch:
        try:
            records = sqllogic.parse((source / entry["path"]).read_text())
            result["source_sql_records"] = sum(r.words[0] in ("query", "statement") for r in records)
            phase = "execution"; engine = RustEngine(binary, scratch, start + timeout)
            runner = sqllogic.Runner(engine, {"{TEST_DIR}": scratch, "__TEST_DIR__": scratch, "{WORKING_DIRECTORY}": scratch,
                "__WORKING_DIRECTORY__": scratch, "{TEST_NAME}": entry["path"], "{BASE_TEST_NAME}": entry["path"].replace("/", "_"), "__SOURCE_DIR__": str(source)})
            runner.run(records)
            if result["source_sql_records"] == 0:
                result.update(status="incomplete", failure_class="controls_only" if records else "no_sql_records")
            elif runner.passed and not runner.skipped:
                result["status"] = "passed"
            else:
                result.update(status="incomplete", failure_class="conditional_skip")
        except Exception as error:
            result.update(status="failed", failure_class=failure_class(error, engine and engine.engine_unsupported_seen, phase), reason=str(error)[:2000])
        finally:
            if runner:
                result.update(evidence(records, runner.line), passed_records=runner.passed, skipped_records=runner.skipped,
                              attempted_records=engine.sql_requests, worker_requests=engine.worker_requests)
            if engine: engine.close()
    if "source_sql_records" in result:
        repeated = any(r.words[0] in ("loop", "foreach", "concurrentloop", "concurrentforeach") for r in records)
        if repeated:
            result["unreached_source_records"] = None
            result["source_record_accounting"] = "unknown: loop control can revisit source records"
        else:
            result["unreached_source_records"] = sum(r.words[0] in ("query", "statement") and r.line > (runner.line if runner else 0) for r in records)
            result["source_record_accounting"] = "source line ordering"
    result["elapsed_seconds"] = round(time.monotonic() - start, 6)
    return result


def archive_population(target, temporary):
    if target == "development":
        manifest = verify(DESTINATION, temporary)
        return temporary, manifest, {"kind": "retained_archive", "revision": REVISION, "archive_sha256": manifest["archive_sha256"], "manifest_sha256": digest(DESTINATION / "manifest.json")}
    source = TARGETS[target].source; revision = require_checkout(source, target); archive = temporary / "source.tar"
    with archive.open("xb") as output: subprocess.run(["git", "archive", "--format=tar", revision], cwd=source, stdout=output, check=True)
    files, tests = [], []
    with tarfile.open(archive) as contents:
        contents.extractall(temporary / "tree", filter="data")
        for member in contents:
            if member.isfile():
                data = contents.extractfile(member).read(); files.append({"path": member.name, "sha256": hashlib.sha256(data).hexdigest()}); tests.extend(declarations(member.name, data))
    manifest = {"revision": revision, "files": files, "tests": tests, "counts": dict(Counter(t["kind"] for t in tests))}
    return temporary / "tree", manifest, {"kind": "git_archive", "revision": revision, "archive_sha256": digest(archive), "source_path": str(source), "source_tree_sha256": hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()}


def cached_files(source):
    """Return a complete, content-addressed inventory of a cached suite tree."""
    files = []
    for path in sorted(source.rglob("*")):
        if path.is_dir():
            continue
        relative = str(path.relative_to(source))
        data = os.readlink(path).encode() if path.is_symlink() else path.read_bytes()
        files.append({"path": relative, "kind": "symlink" if path.is_symlink() else "file",
                      "sha256": hashlib.sha256(data).hexdigest()})
    return files


def cache_identity(target):
    """Identity available without trusting the cache itself."""
    if target == "development":
        manifest = verify(DESTINATION)
        return {"target": target, "revision": REVISION,
                "archive_sha256": manifest["archive_sha256"],
                "manifest_sha256": digest(DESTINATION / "manifest.json")}
    revision = require_checkout(TARGETS[target].source, target)
    return {"target": target, "revision": revision}


def cached_population(target, cache_root):
    """Materialize a suite once, but validate every cached byte on every use."""
    identity = cache_identity(target)
    key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    root = cache_root / target / key
    metadata = root / "suite.json"
    if metadata.exists():
        try:
            saved = json.loads(metadata.read_text())
            if saved.get("identity") != identity or saved.get("files") != cached_files(root / "source"):
                raise ValueError("cached suite content differs")
            return root / "source", saved["manifest"], {**saved["population_identity"], "cache": "validated"}
        except (OSError, ValueError, KeyError, json.JSONDecodeError):
            # This directory is created and owned solely by this cache key.
            shutil.rmtree(root, ignore_errors=True)
    root.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ddb-upstream-cache-", dir=root.parent) as temporary:
        temporary = Path(temporary)
        source, manifest, population_identity = archive_population(target, temporary / "input")
        staging = temporary / "ready"
        shutil.copytree(source, staging / "source", symlinks=True)
        saved = {"identity": identity, "manifest": manifest,
                 "population_identity": population_identity,
                 "files": cached_files(staging / "source")}
        (staging / "suite.json").write_text(json.dumps(saved, sort_keys=True) + "\n")
        os.replace(staging, root)
    return root / "source", manifest, {**population_identity, "cache": "created"}


def worker_build(debug_worker):
    command = ["cargo", "build", "--offline"]
    if not debug_worker:
        command.append("--release")
    command.extend(["--no-default-features", "--bin", "duckdb-rust-test-worker"])
    subprocess.run(command, cwd=ROOT, check=True)
    profile = "debug" if debug_worker else "release"
    return command, ROOT / "target" / profile / "duckdb-rust-test-worker"


def comparable_outcome(result):
    """Only assertion-visible result fields participate in debug/release equivalence."""
    return {key: result.get(key) for key in ("id", "path", "status", "failure_class", "reason",
                                               "passed_records", "skipped_records", "attempted_records",
                                               "unreached_source_records", "source_sql_records")}


def validation_fingerprint():
    """Inputs that can change a worker result; a changed run is never green."""
    digestor = hashlib.sha256()
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "test/runner/worker.rs",
             *(ROOT / "src").rglob("*.rs"), *(ROOT / "scripts").glob("*.py")]
    for path in sorted(paths):
        digestor.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    return digestor.hexdigest()


def acquire_validation_lock(cache_root):
    """One campaign per worktree/cache; do not overlap builds or result journals."""
    cache_root.mkdir(parents=True, exist_ok=True)
    handle = (cache_root / ".validation.lock").open("a+")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        handle.close()
        raise RuntimeError("another upstream validation is active for this worktree") from error
    return handle


def watch_feedback(args):
    """Continuously rerun settled edits; each child retains a distinct report."""
    original = list(sys.argv[1:])
    original.remove("--watch")
    index, observed = 0, None
    while True:
        # A change resets the debounce interval rather than starting an
        # overlapping build or accepting a source state that is still moving.
        candidate = validation_fingerprint()
        while True:
            time.sleep(args.debounce_seconds)
            current = validation_fingerprint()
            if current == candidate:
                break
            candidate = current
        report = args.report if index == 0 else args.report.with_name(
            f"{args.report.stem}.watch-{index}{args.report.suffix}")
        command = original.copy()
        report_position = command.index("--report") + 1
        command[report_position] = str(report)
        subprocess.run([sys.executable, str(Path(__file__)), *command], check=False)
        index += 1
        observed = validation_fingerprint()
        while validation_fingerprint() == observed:
            time.sleep(0.1)


def selected_entries(sql, prefixes, path_list, retry_report=None, target=None):
    allowed = None if not path_list else {x.strip() for x in path_list.read_text().splitlines() if x.strip() and not x.startswith("#")}
    if allowed is not None:
        unknown = allowed - {e["path"] for e in sql}
        if unknown: raise ValueError(f"path list contains unknown upstream paths: {sorted(unknown)[:5]}")
    if retry_report:
        prior = json.loads(retry_report.read_text())["populations"][target]
        retries = {r["path"] for r in prior["results"] if r.get("failure_class") == "timeout"}
        allowed = retries if allowed is None else allowed & retries
    selected = [e for e in sql if (not prefixes or any(e["path"].startswith(p) for p in prefixes)) and (allowed is None or e["path"] in allowed)]
    if not selected: raise ValueError("selection contains no upstream SQL files")
    return selected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True); parser.add_argument("--target", choices=("development", "release", "both"), default="both")
    parser.add_argument("--timeout", type=float, default=10); parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--path-prefix", action="append", default=[]); parser.add_argument("--path-list", type=Path)
    parser.add_argument("--retry-timeouts-from", type=Path, help="select only timeouts from an earlier campaign report")
    parser.add_argument("--debug-worker", action="store_true", help="build and run the debug worker for edit feedback")
    parser.add_argument("--worker", type=Path, help="prebuilt worker; preserves caller-visible launch/cache costs but excludes compilation")
    parser.add_argument("--compare-release", action="store_true", help="require debug-worker outcomes to equal a fresh release-worker run")
    parser.add_argument("--suite-cache", type=Path, default=ROOT / "target/upstream-suite-cache",
                        help="worktree-local, hash-validated extracted-suite cache")
    parser.add_argument("--watch", action="store_true", help="continuously debounce edits and run one locked validation per settled source state")
    parser.add_argument("--debounce-seconds", type=float, default=0.25)
    args = parser.parse_args()
    if args.watch:
        if args.debounce_seconds <= 0: raise ValueError("watch debounce must be positive")
        watch_feedback(args)
        return
    journal = args.report.with_suffix(args.report.suffix + "l")
    if args.report.exists() or journal.exists(): raise FileExistsError("choose a new report path; retain earlier failures")
    if args.timeout <= 0 or not 1 <= args.jobs <= 8 or args.debounce_seconds < 0 or (args.watch and args.debounce_seconds <= 0): raise ValueError("positive timeout, 1..8 workers, and positive watch debounce required")
    if args.compare_release and (not args.debug_worker or args.worker): raise ValueError("--compare-release requires a built debug worker")
    lock_handle = acquire_validation_lock(args.suite_cache)
    source_before = validation_fingerprint()
    if args.worker:
        binary = args.worker.resolve(strict=True)
        if not binary.is_file(): raise ValueError("--worker must name a regular executable")
        rust_build = ["prebuilt-worker", str(binary)]
    else:
        rust_build, binary = worker_build(args.debug_worker)
    source_hash = hashlib.sha256()
    from source_identity import vendored_sources
    for path in sorted([*vendored_sources(ROOT), ROOT / "Cargo.toml", ROOT / "Cargo.lock", *(ROOT / "src").rglob("*.rs"), ROOT / "test/runner/worker.rs"]):
        source_hash.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    targets = ("development", "release") if args.target == "both" else (args.target,)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "engine_git_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "rust_build_command": rust_build, "worker_profile": "debug" if args.debug_worker else "release", "rust_source_sha256": source_hash.hexdigest(), "rust_binary_sha256": digest(binary), "harness_sha256": {n: digest(ROOT / "scripts" / n) for n in ("sqllogic.py", "run_upstream.py", "upstream_suite.py", "reference_version.py")}, "timeout_seconds": args.timeout, "jobs": args.jobs, "path_prefixes": args.path_prefix, "path_list": str(args.path_list) if args.path_list else None, "suite_cache": str(args.suite_cache), "populations": {}, "scope": "Exact SQLLogicTest inputs run against Rust with unchanged assertions. First blocker only. Passed/skipped are observed executions; unreached is source-record based, so loop-expanded totals are never invented. Native/client/benchmark declarations, compiled parameterizations, generated tests, configurations, platforms and external suites remain outside this SQL campaign."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    with journal.open("x") as progress:
        progress.write(json.dumps({"event": "started", "metadata": report}) + "\n"); progress.flush()
        for target in targets:
            source, manifest, identity = cached_population(target, args.suite_cache); sql = [e for e in manifest["tests"] if e["kind"] == "sqllogictest"]; selected = selected_entries(sql, args.path_prefix, args.path_list, args.retry_timeouts_from, target)
            population = {"identity": identity, "inventory": manifest["counts"], "sql_files_total": len(sql), "selected": selected, "sql_files_selected": len(selected), "results": [], "unported": [e for e in manifest["tests"] if e["kind"] != "sqllogictest"], "obligations": {scope: "unverified" for scope in REQUIRED_SCOPES}}
            with ThreadPoolExecutor(max_workers=args.jobs) as pool:
                for outcome in pool.map(lambda e: run_case(binary, source, e, args.timeout), selected):
                    population["results"].append(outcome); progress.write(json.dumps({"event": "result", "target": target, **outcome}) + "\n"); progress.flush()
                    if len(population["results"]) % 250 == 0: print(f"{target}: {len(population['results'])}/{len(selected)} files recorded", flush=True)
            population.update(summarize(sql, selected, population["results"], population["unported"], population["obligations"])); report["populations"][target] = population
    if args.compare_release:
        release_build, release_binary = worker_build(False)
        compared = {"rust_build_command": release_build, "rust_binary_sha256": digest(release_binary), "targets": {}}
        for target, population in report["populations"].items():
            source, _, _ = cached_population(target, args.suite_cache)
            with ThreadPoolExecutor(max_workers=args.jobs) as pool:
                release_results = list(pool.map(lambda e: run_case(release_binary, source, e, args.timeout), population["selected"]))
            differences = [(comparable_outcome(debug), comparable_outcome(release)) for debug, release in zip(population["results"], release_results) if comparable_outcome(debug) != comparable_outcome(release)]
            if len(release_results) != len(population["results"]): differences.append(("result count", [len(population["results"]), len(release_results)]))
            compared["targets"][target] = {"passed": not differences, "differences": differences[:10]}
        compared["passed"] = all(item["passed"] for item in compared["targets"].values())
        report["release_comparison"] = compared
    report["source_fingerprint_before"] = source_before
    report["source_fingerprint_after"] = validation_fingerprint()
    report["stale_source"] = report["source_fingerprint_before"] != report["source_fingerprint_after"]
    report["journal_sha256"] = digest(journal); report["outcomes"] = {t: p["outcomes"] for t, p in report["populations"].items()}; args.report.write_text(json.dumps(report, indent=2) + "\n"); lock_handle.close()
    if report["stale_source"]: raise RuntimeError("source changed during validation; report is stale and not green")
    if args.compare_release and not report["release_comparison"]["passed"]:
        raise RuntimeError("debug-worker outcomes differ from the release worker")
    print(json.dumps({"outcomes": report["outcomes"], "report": str(args.report)}))


if __name__ == "__main__": main()
