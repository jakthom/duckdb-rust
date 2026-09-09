"""Run every retained upstream SQL file; report all other test obligations as gaps."""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import selectors
import subprocess
import tempfile
import time

import sqllogic
from upstream_suite import DESTINATION, REVISION, ROOT, digest, verify


REQUIRED_SCOPES = ("compiled_registry", "generated_cases", "configurations", "platforms", "external_suites")


def summarize(sql, selected, results, unported, obligations):
    """Acceptance needs a one-to-one result for every obligation, not a pass count."""
    def identities(entries):
        return {entry["id"] for entry in entries}

    selected_ids, result_ids = identities(selected), identities(results)
    selection_passed = (bool(selected) and len(selected_ids) == len(selected)
                        and len(result_ids) == len(results) and selected_ids == result_ids
                        and all(r["status"] == "passed" for r in results))
    sql_passed = (selection_passed and len(identities(sql)) == len(sql)
                  and identities(sql) == selected_ids)
    scopes_passed = (set(obligations) == set(REQUIRED_SCOPES)
                     and all(value == "passed" for value in obligations.values()))
    return {"outcomes": dict(Counter(r["status"] for r in results)),
            "sql_selection_passed": selection_passed, "sql_suite_passed": sql_passed,
            "full_suite_passed": sql_passed and not unported and scopes_passed}


class RustEngine:
    def __init__(self, binary, directory, deadline):
        self.errors = tempfile.TemporaryFile(mode="w+t")
        self.process = subprocess.Popen([str(binary)], cwd=directory, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.errors, text=True, bufsize=1)
        self.events = selectors.DefaultSelector()
        self.events.register(self.process.stdout, selectors.EVENT_READ)
        self.deadline = deadline

    def request(self, request):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("file deadline exceeded")
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        if not self.events.select(timeout=remaining):
            raise TimeoutError("file deadline exceeded")
        line = self.process.stdout.readline()
        if not line:
            self.errors.seek(0)
            raise RuntimeError("worker exited: " + self.errors.read()[:2000])
        return json.loads(line)

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()
        self.process.stdin.close()
        self.process.stdout.close()
        self.events.close()
        self.errors.close()


def run_case(binary, source, entry, timeout):
    start = time.monotonic()
    result = {"id": entry["id"], "path": entry["path"], "passed_records": 0, "skipped_records": 0}
    runner, engine = None, None
    with tempfile.TemporaryDirectory(prefix="ddb-upstream-case-") as scratch:
        try:
            records = sqllogic.parse((source / entry["path"]).read_text())
            result["source_sql_records"] = sum(r.words[0] in ("query", "statement") for r in records)
            engine = RustEngine(binary, scratch, start + timeout)
            runner = sqllogic.Runner(engine, {"{TEST_DIR}": scratch, "__TEST_DIR__": scratch,
                                              "{WORKING_DIRECTORY}": scratch, "__WORKING_DIRECTORY__": scratch,
                                              "{TEST_NAME}": entry["path"], "{BASE_TEST_NAME}": entry["path"].replace("/", "_"),
                                              "__SOURCE_DIR__": str(source)})
            runner.run(records)
            result["status"] = "passed" if runner.passed and not runner.skipped else "incomplete"
        except sqllogic.Unsupported as error:
            result.update(status="unsupported", reason=str(error))
        except TimeoutError as error:
            result.update(status="timeout", reason=str(error))
        except Exception as error:
            result.update(status="failed", reason=str(error))
        finally:
            if runner:
                result.update(line=runner.line, passed_records=runner.passed, skipped_records=runner.skipped)
            if engine:
                engine.close()
    result["elapsed_seconds"] = round(time.monotonic()-start, 6)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=3)
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--path-prefix", default="")
    args = parser.parse_args()
    journal = args.report.with_suffix(args.report.suffix + "l")
    if args.report.exists() or journal.exists():
        raise FileExistsError("choose a new report path; retain earlier failures")
    if args.timeout <= 0 or not 1 <= args.jobs <= 8:
        raise ValueError("positive timeout and 1..8 workers required")
    manifest = verify(DESTINATION)
    subprocess.run(["cargo", "build", "--offline", "--release", "--bin", "duckdb-rust-test-worker"], cwd=ROOT, check=True)
    binary = ROOT / "target/release/duckdb-rust-test-worker"
    source_hash = hashlib.sha256()
    for path in sorted([ROOT / "Cargo.toml", ROOT / "Cargo.lock", *(ROOT / "src").rglob("*.rs"), ROOT / "test/runner/worker.rs"]):
        source_hash.update(str(path.relative_to(ROOT)).encode()+b"\0"+path.read_bytes())
    sql = [entry for entry in manifest["tests"] if entry["kind"] == "sqllogictest"]
    selected = [entry for entry in sql if entry["path"].startswith(args.path_prefix)]
    if not selected:
        raise ValueError("selection contains no upstream SQL files")
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "upstream_revision": REVISION,
              "archive_sha256": manifest["archive_sha256"], "manifest_sha256": digest(DESTINATION / "manifest.json"),
              "rust_source_sha256": source_hash.hexdigest(), "rust_binary_sha256": digest(binary),
              "harness_sha256": {name: digest(ROOT / "scripts" / name) for name in ("sqllogic.py", "run_upstream.py", "upstream_suite.py")},
              "source_assets_retained": len(manifest["files"]), "upstream_inventory": manifest["counts"],
              "sql_files_total": len(sql), "sql_files_selected": len(selected), "timeout_seconds": args.timeout,
              "jobs": args.jobs, "results": [], "unported": [entry for entry in manifest["tests"] if entry["kind"] != "sqllogictest"],
              "full_suite_passed": False, "compiled_registry_parity": "unverified",
              "obligations": {scope: "unverified" for scope in REQUIRED_SCOPES},
              "journal": str(journal),
              "scope": "All source assets are retained. SQL runs use Rust exclusively and unchanged assertions. Conditional omissions, unsupported directives/features, timeouts, failures and unmapped native/client/benchmark declarations remain gaps. Compiled parameterizations, generated tests, all configurations/platforms and external repositories still require execution/mapping. No claim of complete test parity."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    with journal.open("x") as progress, tempfile.TemporaryDirectory(prefix="ddb-upstream-source-") as temporary:
        progress.write(json.dumps({"event": "started", "metadata": report}) + "\n")
        progress.flush()
        source = Path(temporary)
        verify(DESTINATION, source)
        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            for outcome in pool.map(lambda entry: run_case(binary, source, entry, args.timeout), selected):
                report["results"].append(outcome)
                progress.write(json.dumps({"event": "result", **outcome}) + "\n")
                progress.flush()
                if len(report["results"]) % 250 == 0:
                    print(f"{len(report['results'])}/{len(selected)} files recorded", flush=True)
    report.update(summarize(sql, selected, report["results"], report["unported"], report["obligations"]))
    report["journal_sha256"] = digest(journal)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"outcomes": report["outcomes"], "full_suite_passed": report["full_suite_passed"], "report": str(args.report)}))
    raise SystemExit(0 if report["full_suite_passed"] else 1)


if __name__ == "__main__":
    main()
