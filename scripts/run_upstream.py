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
from pathlib import PurePosixPath

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
        members = extract_source_archive(contents, temporary / "tree")
        for member in members:
            if member.isfile():
                data = contents.extractfile(member).read(); files.append({"path": member.name, "sha256": hashlib.sha256(data).hexdigest()}); tests.extend(declarations(member.name, data))
    manifest = {"revision": revision, "files": files, "tests": tests, "counts": dict(Counter(t["kind"] for t in tests))}
    return temporary / "tree", manifest, {"kind": "git_archive", "revision": revision, "archive_sha256": digest(archive), "source_path": str(source), "source_tree_sha256": hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()}


def safe_archive_path(path):
    """Normalize a POSIX tar name, rejecting any escape from its root."""
    candidate = PurePosixPath(path)
    if candidate.is_absolute():
        raise ValueError("archive member path is absolute: " + path)
    result = []
    for part in candidate.parts:
        if part in ("", "."):
            continue
        if part == "..":
            if not result:
                raise ValueError("archive member path escapes root: " + path)
            result.pop()
        else:
            result.append(part)
    if not result:
        raise ValueError("archive member path is empty: " + path)
    return PurePosixPath(*result)


def extract_source_archive(contents, destination):
    """Extract Git's source archive without allowing link/path traversal."""
    destination.mkdir(parents=True, exist_ok=True)
    members = list(contents)
    seen, links = set(), []
    for member in members:
        path = safe_archive_path(member.name)
        if member.isdir():
            continue
        if str(path) in seen:
            raise ValueError("duplicate archive member: " + member.name)
        seen.add(str(path))
        target = destination / path
        if member.isfile():
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open("xb") as output:
                shutil.copyfileobj(contents.extractfile(member), output)
            target.chmod(member.mode & 0o777)
        elif member.issym() or member.islnk():
            link = member.linkname
            if not link or PurePosixPath(link).is_absolute():
                raise ValueError("archive link is absolute or empty: " + member.name)
            # Symlink names are interpreted relative to their parent; tar hard
            # link names are archive-root names, not paths relative to member.
            linked = safe_archive_path(str(path.parent / link) if member.issym() else link)
            links.append((target, linked, member.issym(), member.mode))
        else:
            raise ValueError("unsupported archive member: " + member.name)
    for target, linked, symbolic, mode in links:
        target.parent.mkdir(parents=True, exist_ok=True)
        resolved = destination / linked
        if symbolic:
            target.symlink_to(os.path.relpath(resolved, target.parent))
        else:
            if not resolved.is_file():
                raise ValueError("archive hard link target is absent: " + str(linked))
            os.link(resolved, target)
            target.chmod(mode & 0o777)
    return members


def cached_files(source):
    """Return a complete, content-addressed inventory of a cached suite tree."""
    if source.is_symlink() or not source.is_dir():
        raise ValueError("cached suite source is not an owned directory")
    root = source.resolve()
    files = []
    for path in sorted(source.rglob("*")):
        relative = str(path.relative_to(source))
        if path.is_symlink():
            link = os.readlink(path)
            # Cached suite links are part of the source identity, but must not
            # turn a selected SQL file into an arbitrary host-file read.
            if Path(link).is_absolute() or not (path.parent / link).resolve().is_relative_to(root):
                raise ValueError("cached suite symlink escapes source root: " + relative)
            data = link.encode()
        elif path.is_dir():
            continue
        else:
            data = path.read_bytes()
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
            if (saved.get("identity") != identity
                    or saved.get("files") != cached_files(root / "source")
                    or not cached_manifest_matches_source(root / "source", saved.get("manifest"))):
                raise ValueError("cached suite content differs")
            return root / "source", saved["manifest"], {**saved["population_identity"], "cache": "validated"}
        except (OSError, ValueError, KeyError, json.JSONDecodeError):
            # This directory is created and owned solely by this cache key.
            shutil.rmtree(root, ignore_errors=True)
    root.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ddb-upstream-cache-", dir=root.parent) as temporary:
        temporary = Path(temporary)
        input_directory = temporary / "input"
        input_directory.mkdir()
        source, manifest, population_identity = archive_population(target, input_directory)
        staging = temporary / "ready"
        shutil.copytree(source, staging / "source", symlinks=True)
        saved = {"identity": identity, "manifest": manifest,
                 "population_identity": population_identity,
                 "files": cached_files(staging / "source")}
        (staging / "suite.json").write_text(json.dumps(saved, sort_keys=True) + "\n")
        os.replace(staging, root)
    return root / "source", manifest, {**population_identity, "cache": "created"}


def cached_manifest_matches_source(source, manifest):
    """A changed suite.json cannot rewrite selection/count metadata by itself."""
    if not isinstance(manifest, dict) or not isinstance(manifest.get("files"), list):
        return False
    actual = cached_files(source)
    declared = {entry.get("path"): entry for entry in manifest["files"]}
    if len(declared) != len(manifest["files"]) or len(declared) != len(actual):
        return False
    for entry in actual:
        expected = declared.get(entry["path"])
        if not isinstance(expected, dict) or expected.get("sha256") != entry["sha256"]:
            return False
        if expected.get("kind", entry["kind"]) != entry["kind"]:
            return False
    tests = []
    for entry in actual:
        if entry["kind"] == "file":
            tests.extend(declarations(entry["path"], (source / entry["path"]).read_bytes()))
    return manifest.get("tests") == tests and manifest.get("counts") == dict(Counter(test["kind"] for test in tests))


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
    # Preserve the public watch intent in its one-shot children. A plain
    # release --path-list remains a full-population campaign for accounting.
    original.append("--feedback-watch-child")
    index = 0
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
        rewrite_report_argument(command, report)
        subprocess.run([sys.executable, str(Path(__file__)), *command], check=False)
        index += 1
        # A child that became stale must not leave the newest edit waiting for
        # another edit event: debounce and validate that state immediately.
        if validation_fingerprint() != candidate:
            continue
        while validation_fingerprint() == candidate:
            time.sleep(0.1)


def rewrite_report_argument(command, report):
    """Replace either argparse spelling without accidentally adding a second report."""
    for index, value in enumerate(command):
        if value == "--report":
            if index + 1 == len(command): raise ValueError("--report requires a path")
            command[index + 1] = str(report)
            return
        if value.startswith("--report="):
            command[index] = "--report=" + str(report)
            return
    raise ValueError("watch requires --report")


def selected_entries(sql, prefixes, path_list, retry_report=None, target=None):
    allowed = None if not path_list else set(selected_path_list(path_list))
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


def selected_path_list(path_list):
    """Exact, safe feedback paths; duplicates cannot disguise a reduced run."""
    paths, seen = [], set()
    for raw in path_list.read_text().splitlines():
        path = raw.strip()
        if not path or path.startswith("#"):
            continue
        parsed = PurePosixPath(path)
        if (parsed.is_absolute() or ".." in parsed.parts or parsed.as_posix() != path
                or "\x00" in path or path in seen):
            raise ValueError("path list contains unsafe or duplicate upstream path: " + path)
        seen.add(path); paths.append(path)
    if not paths:
        raise ValueError("selection contains no upstream SQL files")
    return paths


def selected_source_file(target, path):
    """Read one trusted pinned source file without extracting the full suite."""
    if target == "release":
        revision = require_checkout(TARGETS[target].source, target)
        try:
            data = subprocess.check_output(["git", "show", f"{revision}:{path}"], cwd=TARGETS[target].source)
        except subprocess.CalledProcessError as error:
            raise ValueError("selected upstream path is absent from pinned release: " + path) from error
        return revision, data
    manifest = json.loads((DESTINATION / "manifest.json").read_text())
    if manifest.get("revision") != REVISION:
        raise ValueError("unexpected retained development revision")
    expected = {entry["path"]: entry for entry in manifest.get("files", [])}.get(path)
    if not expected or expected.get("kind") != "file":
        raise ValueError("selected upstream path is absent from retained development source: " + path)
    archive_name = manifest.get("archive")
    if not isinstance(archive_name, str) or PurePosixPath(archive_name).name != archive_name:
        raise ValueError("retained development archive name is unsafe")
    with tarfile.open(DESTINATION / archive_name) as archive:
        try:
            member = archive.getmember(path)
        except KeyError as error:
            raise ValueError("retained development archive omits selected path: " + path) from error
        if not member.isfile(): raise ValueError("selected development path is not a regular file: " + path)
        data = archive.extractfile(member).read()
    if hashlib.sha256(data).hexdigest() != expected.get("sha256"):
        raise ValueError("selected development source hash differs: " + path)
    return REVISION, data


def selected_feedback_population(target, paths, cache_root):
    """Small, hash-validated source root for explicit debug/prebuilt feedback."""
    fetched = [(*selected_source_file(target, path), path) for path in paths]
    revisions = {revision for revision, _, _ in fetched}
    if len(revisions) != 1: raise ValueError("selected source revision changed during feedback setup")
    revision = revisions.pop()
    files = [{"path": path, "kind": "file", "sha256": hashlib.sha256(data).hexdigest()}
             for _, data, path in fetched]
    identity = {"target": target, "revision": revision, "paths": paths, "files": files}
    key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    root = cache_root / "selected-feedback" / target / key
    metadata = root / "suite.json"
    if metadata.exists():
        try:
            saved = json.loads(metadata.read_text())
            if (saved.get("identity") != identity or not cached_manifest_matches_source(root / "source", saved.get("manifest"))):
                raise ValueError("selected feedback cache differs")
            return root / "source", saved["manifest"], {"kind": "selected_feedback", **identity, "cache": "validated"}
        except (OSError, ValueError, KeyError, json.JSONDecodeError):
            shutil.rmtree(root, ignore_errors=True)
    root.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ddb-upstream-selected-", dir=root.parent) as temporary:
        staging = Path(temporary) / "ready"; source = staging / "source"
        tests = []
        for _, data, path in fetched:
            destination = source / path; destination.parent.mkdir(parents=True, exist_ok=True); destination.write_bytes(data)
            entries = declarations(path, data)
            if len(entries) != 1 or entries[0].get("kind") != "sqllogictest":
                raise ValueError("selected feedback path is not one SQLLogic file: " + path)
            tests.extend(entries)
        manifest = {"revision": revision, "files": files, "tests": tests,
                    "counts": dict(Counter(test["kind"] for test in tests))}
        (staging / "suite.json").write_text(json.dumps({"identity": identity, "manifest": manifest}, sort_keys=True) + "\n")
        os.replace(staging, root)
    return root / "source", manifest, {"kind": "selected_feedback", **identity, "cache": "created"}


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
    parser.add_argument("--feedback-watch-child", action="store_true", help=argparse.SUPPRESS)
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
    # This deliberately narrow path is an edit-feedback operation, not a way to
    # make a full-suite campaign appear complete.  It keeps worker/cache costs in
    # the caller-visible path while avoiding 14k unrelated source files.
    feedback_paths = (selected_path_list(args.path_list)
                      if args.path_list and (args.debug_worker or args.worker or args.feedback_watch_child) else None)
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
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "engine_git_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "rust_build_command": rust_build, "worker_profile": "debug" if args.debug_worker else "release", "rust_source_sha256": source_hash.hexdigest(), "rust_binary_sha256": digest(binary), "harness_sha256": {n: digest(ROOT / "scripts" / n) for n in ("sqllogic.py", "run_upstream.py", "upstream_suite.py", "reference_version.py")}, "timeout_seconds": args.timeout, "jobs": args.jobs, "path_prefixes": args.path_prefix, "path_list": str(args.path_list) if args.path_list else None, "suite_cache": str(args.suite_cache), "campaign_kind": "selected-feedback" if feedback_paths else "suite-campaign", "populations": {}, "scope": "Exact SQLLogicTest inputs run against Rust with unchanged assertions. First blocker only. Passed/skipped are observed executions; unreached is source-record based, so loop-expanded totals are never invented. Native/client/benchmark declarations, compiled parameterizations, generated tests, configurations, platforms and external suites remain outside this SQL campaign." + (" Selected-feedback is deliberately not full-suite acceptance." if feedback_paths else "")}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    with journal.open("x") as progress:
        progress.write(json.dumps({"event": "started", "metadata": report}) + "\n"); progress.flush()
        for target in targets:
            if feedback_paths:
                source, manifest, identity = selected_feedback_population(target, feedback_paths, args.suite_cache)
            else:
                source, manifest, identity = cached_population(target, args.suite_cache)
            sql = [e for e in manifest["tests"] if e["kind"] == "sqllogictest"]
            selected = selected_entries(sql, args.path_prefix, args.path_list, args.retry_timeouts_from, target)
            population = {"identity": identity, "inventory": manifest["counts"], "sql_files_total": len(sql), "selected": selected, "sql_files_selected": len(selected), "results": [], "unported": [e for e in manifest["tests"] if e["kind"] != "sqllogictest"], "obligations": {scope: "unverified" for scope in REQUIRED_SCOPES}, "selection_kind": "selected-feedback" if feedback_paths else "suite"}
            with ThreadPoolExecutor(max_workers=args.jobs) as pool:
                for outcome in pool.map(lambda e: run_case(binary, source, e, args.timeout), selected):
                    population["results"].append(outcome); progress.write(json.dumps({"event": "result", "target": target, **outcome}) + "\n"); progress.flush()
                    if len(population["results"]) % 250 == 0: print(f"{target}: {len(population['results'])}/{len(selected)} files recorded", flush=True)
            population.update(summarize(sql, selected, population["results"], population["unported"], population["obligations"]))
            if feedback_paths:
                population["sql_suite_passed"] = False
                population["full_suite_passed"] = False
            report["populations"][target] = population
    if args.compare_release:
        release_build, release_binary = worker_build(False)
        compared = {"rust_build_command": release_build, "rust_binary_sha256": digest(release_binary), "targets": {}}
        for target, population in report["populations"].items():
            source, _, _ = (selected_feedback_population(target, feedback_paths, args.suite_cache)
                            if feedback_paths else cached_population(target, args.suite_cache))
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
