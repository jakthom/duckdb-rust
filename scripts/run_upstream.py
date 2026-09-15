"""Measure exact upstream SQLLogicTest populations against the Rust worker.

This keeps upstream assertions unchanged. A non-passing file gets its first
blocking outcome and source location, allowing retries without overwriting it.
"""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import selectors
import subprocess
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
        self.deadline = deadline; self.engine_unsupported_seen = False
    def request(self, request):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0: raise TimeoutError("file deadline exceeded")
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


def failure_class(error, engine_unsupported_seen=False):
    reason = str(error)
    if isinstance(error, TimeoutError): return "timeout"
    if isinstance(error, RuntimeError) and reason.startswith("worker exited:"): return "crash"
    if isinstance(error, sqllogic.Unsupported):
        if engine_unsupported_seen: return "engine_unsupported"
        return "harness_directive_or_oracle"
    if isinstance(error, (AssertionError, ValueError)): return "assertion_or_error_mismatch"
    return "setup"


def evidence(records, line):
    record = next((r for r in records if r.line == line and r.words[0] in ("query", "statement")), None)
    return {"line": line, "sql": record.sql[:2000] if record else None}


def run_case(binary, source, entry, timeout):
    start = time.monotonic(); runner = engine = None; records = []
    result = {"id": entry["id"], "path": entry["path"], "passed_records": 0, "skipped_records": 0,
              "attempted_records": 0, "unreached_source_records": None}
    with tempfile.TemporaryDirectory(prefix="ddb-upstream-case-") as scratch:
        try:
            records = sqllogic.parse((source / entry["path"]).read_text())
            result["source_sql_records"] = sum(r.words[0] in ("query", "statement") for r in records)
            engine = RustEngine(binary, scratch, start + timeout)
            runner = sqllogic.Runner(engine, {"{TEST_DIR}": scratch, "__TEST_DIR__": scratch, "{WORKING_DIRECTORY}": scratch,
                "__WORKING_DIRECTORY__": scratch, "{TEST_NAME}": entry["path"], "{BASE_TEST_NAME}": entry["path"].replace("/", "_"), "__SOURCE_DIR__": str(source)})
            runner.run(records)
            result["status"] = "passed" if runner.passed and not runner.skipped else "incomplete"
            if result["status"] != "passed": result["failure_class"] = "conditional_skip"
        except Exception as error:
            result.update(status="failed", failure_class=failure_class(error, engine and engine.engine_unsupported_seen), reason=str(error)[:2000])
        finally:
            if runner:
                result.update(evidence(records, runner.line), passed_records=runner.passed, skipped_records=runner.skipped, attempted_records=runner.passed)
            if engine: engine.close()
    if "source_sql_records" in result:
        result["unreached_source_records"] = max(0, result["source_sql_records"] - result["passed_records"] - result["skipped_records"])
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


def selected_entries(sql, prefixes, path_list):
    allowed = None if not path_list else {x.strip() for x in path_list.read_text().splitlines() if x.strip() and not x.startswith("#")}
    selected = [e for e in sql if (not prefixes or any(e["path"].startswith(p) for p in prefixes)) and (allowed is None or e["path"] in allowed)]
    if not selected: raise ValueError("selection contains no upstream SQL files")
    return selected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True); parser.add_argument("--target", choices=("development", "release", "both"), default="both")
    parser.add_argument("--timeout", type=float, default=10); parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--path-prefix", action="append", default=[]); parser.add_argument("--path-list", type=Path)
    args = parser.parse_args(); journal = args.report.with_suffix(args.report.suffix + "l")
    if args.report.exists() or journal.exists(): raise FileExistsError("choose a new report path; retain earlier failures")
    if args.timeout <= 0 or not 1 <= args.jobs <= 8: raise ValueError("positive timeout and 1..8 workers required")
    rust_build = ["cargo", "build", "--offline", "--release", "--no-default-features", "--bin", "duckdb-rust-test-worker"]
    subprocess.run(rust_build, cwd=ROOT, check=True); binary = ROOT / "target/release/duckdb-rust-test-worker"; source_hash = hashlib.sha256()
    from source_identity import vendored_sources
    for path in sorted([*vendored_sources(ROOT), ROOT / "Cargo.toml", ROOT / "Cargo.lock", *(ROOT / "src").rglob("*.rs"), ROOT / "test/runner/worker.rs"]):
        source_hash.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    targets = ("development", "release") if args.target == "both" else (args.target,)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "engine_git_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "rust_build_command": rust_build, "rust_source_sha256": source_hash.hexdigest(), "rust_binary_sha256": digest(binary), "harness_sha256": {n: digest(ROOT / "scripts" / n) for n in ("sqllogic.py", "run_upstream.py", "upstream_suite.py", "reference_version.py")}, "timeout_seconds": args.timeout, "jobs": args.jobs, "path_prefixes": args.path_prefix, "path_list": str(args.path_list) if args.path_list else None, "populations": {}, "scope": "Exact SQLLogicTest inputs run against Rust with unchanged assertions. First blocker only. Passed/skipped are observed executions; unreached is source-record based, so loop-expanded totals are never invented. Native/client/benchmark declarations, compiled parameterizations, generated tests, configurations, platforms and external suites remain outside this SQL campaign."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    with journal.open("x") as progress:
        progress.write(json.dumps({"event": "started", "metadata": report}) + "\n"); progress.flush()
        for target in targets:
            with tempfile.TemporaryDirectory(prefix=f"ddb-upstream-{target}-") as directory:
                source, manifest, identity = archive_population(target, Path(directory)); sql = [e for e in manifest["tests"] if e["kind"] == "sqllogictest"]; selected = selected_entries(sql, args.path_prefix, args.path_list)
                population = {"identity": identity, "inventory": manifest["counts"], "sql_files_total": len(sql), "selected": selected, "sql_files_selected": len(selected), "results": [], "unported": [e for e in manifest["tests"] if e["kind"] != "sqllogictest"], "obligations": {scope: "unverified" for scope in REQUIRED_SCOPES}}
                with ThreadPoolExecutor(max_workers=args.jobs) as pool:
                    for outcome in pool.map(lambda e: run_case(binary, source, e, args.timeout), selected):
                        population["results"].append(outcome); progress.write(json.dumps({"event": "result", "target": target, **outcome}) + "\n"); progress.flush()
                        if len(population["results"]) % 250 == 0: print(f"{target}: {len(population['results'])}/{len(selected)} files recorded", flush=True)
                population.update(summarize(sql, selected, population["results"], population["unported"], population["obligations"])); report["populations"][target] = population
    report["journal_sha256"] = digest(journal); report["outcomes"] = {t: p["outcomes"] for t, p in report["populations"].items()}; args.report.write_text(json.dumps(report, indent=2) + "\n"); print(json.dumps({"outcomes": report["outcomes"], "report": str(args.report)}))


if __name__ == "__main__": main()
