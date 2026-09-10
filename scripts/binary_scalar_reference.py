"""Typed BLOB/UUID SQL and native checkpoint/WAL interoperability, both pinned C++ references."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import platform
import subprocess
import tempfile
import time

from numeric_reference import equivalent
from reference_version import TARGETS, require_checkout, require_reference
from run_upstream import RustEngine
from session_reference import CppEngine, source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


SQL = [
    r"SELECT '\x00\xFF'::BLOB, 'abc'::BLOB, ''::BLOB, NULL::BLOB",
    "SELECT '{00112233445566778899AABBCCDDEEFF}'::UUID,'ffffffffffffffffffffffffffffffff'::UUID,'00000000000000000000000000000000'::UUID,NULL::UUID",
    "SELECT typeof('a'::BINARY),typeof('a'::VARBINARY),typeof('a'::BYTEA),typeof('00000000000000000000000000000000'::GUID)",
    r"SELECT hex('é'),hex('\x00\xFF'::BLOB),unhex('F'),encode('é'),decode('hello'::BLOB),octet_length('a\x00'::BLOB)",
    "SELECT hex(-1::HUGEINT),hex(-1::BIGINT),hex(18446744073709551615::UBIGINT),hex(1::UTINYINT)",
    "SELECT '00112233445566778899aabbccddeeff'::UUID::UHUGEINT,1::UHUGEINT::UUID,'00112233445566778899aabbccddeeff'::UUID::BLOB",
    r"SELECT '\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xAA\xBB\xCC\xDD\xEE\xFF'::BLOB::UUID",
    r"SELECT TRY_CAST('\xGG' AS BLOB),TRY_CAST('bad' AS UUID),CASE WHEN false THEN '\xGG'::BLOB ELSE 'ok'::BLOB END,NULL::BLOB||'x'::BLOB,encode(NULL)",
    r"SELECT hex('a'::BLOB||'\x00'::BLOB),concat('a'::BLOB,'b'::BLOB),typeof('a'::BLOB||'b'::BLOB)",
    "SELECT k FROM (VALUES ('ffffffffffffffffffffffffffffffff'::UUID),('80000000000000000000000000000000'::UUID),('00000000000000000000000000000000'::UUID),(NULL)) q(k) ORDER BY k",
    r"SELECT b,count(*) FROM (VALUES ('\x00'::BLOB),(''::BLOB),('\xFF'::BLOB),('\x00'::BLOB),(NULL)) q(b) GROUP BY b ORDER BY b",
    r"SELECT count(*),count(DISTINCT a.b) FROM (VALUES ('\x00'::BLOB),(''::BLOB),('\xFF'::BLOB),(NULL)) a(b) JOIN (VALUES ('\xFF'::BLOB),('\x00'::BLOB),(NULL)) b(b) ON a.b=b.b",
    r"SELECT first_value(b) OVER(ORDER BY b),lag(b) OVER(ORDER BY b),min(b) OVER() FROM (VALUES ('\x00'::BLOB),(''::BLOB),('\xFF'::BLOB)) q(b) ORDER BY b",
    "SELECT min(k),max(k),count(DISTINCT k) FROM (VALUES ('ffffffffffffffffffffffffffffffff'::UUID),('00000000000000000000000000000000'::UUID),(NULL)) q(k)",
    r"SELECT b FROM (VALUES ('\xFF'::BLOB),(''::BLOB),(NULL)) q(b) UNION SELECT b FROM (VALUES ('\xFF'::BLOB),('\x00'::BLOB)) p(b) ORDER BY b",
]
ERRORS = [
    (r"SELECT '\xGG'::BLOB", "Conversion Error"),
    ("SELECT 'é'::BLOB", "Conversion Error"),
    ("SELECT 'oops'::UUID", "Conversion Error"),
    ("SELECT 'abc'::BLOB::UUID", "Conversion Error"),
    (r"SELECT decode('\xFF'::BLOB)", "Conversion Error"),
    ("SELECT unhex('not hex')", "Invalid Input Error"),
    ("SELECT length('a'::BLOB)", "Binder Error"),
    ("SELECT 'a'::BLOB(3)", "Binder Error"),
    ("SELECT 'a'::BINARY(3)", "Binder Error"),
    ("SELECT 'a'::VARBINARY(3)", "Binder Error"),
]


def persistence(rust, cpp, directory):
    results = []
    definition = r"CREATE TABLE t(k UUID PRIMARY KEY DEFAULT '00000000000000000000000000000000', b BLOB UNIQUE DEFAULT '\x00\xFF', d DECIMAL(38,3) DEFAULT 1.125); INSERT INTO t DEFAULT VALUES; INSERT INTO t VALUES ('ffffffffffffffffffffffffffffffff','\x00\x01\xFF',2.250),('80000000000000000000000000000000','',NULL)"
    query = "SELECT k::VARCHAR AS k,b::VARCHAR AS b,d::VARCHAR AS d,typeof(k) AS kt,typeof(b) AS bt FROM t ORDER BY k"
    for label, producer in [("cpp", cpp), ("rust-checkpoint", rust), ("rust-wal", Engine(rust.binary, True, ("--durability", "wal")))]:
        result = {"producer": label, "passed": False}
        results.append(result)
        try:
            path = directory / (label + ".duckdb")
            command(producer, path, definition)
            result["checkpoint_sha256"] = digest(path)
            wal = Path(str(path) + ".wal")
            if wal.exists():
                result["wal_sha256"] = digest(wal)
            expected = command(cpp, path, query, json_output=True, readonly=True)
            actual = command(rust, path, query, json_output=True, readonly=True)
            if expected != actual:
                raise AssertionError({"cpp": expected, "rust": actual})
            command(rust, path, "BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET b='updated'::BLOB WHERE k='00000000000000000000000000000000'::UUID")
            expected = command(cpp, path, query, json_output=True, readonly=True)
            actual = command(rust, path, query, json_output=True, readonly=True)
            if expected != actual or len(actual) != 3:
                raise AssertionError({"cpp": expected, "rust": actual})
            command(cpp, path, "DELETE FROM t WHERE k='80000000000000000000000000000000'::UUID; CHECKPOINT")
            expected = command(cpp, path, query, json_output=True, readonly=True)
            actual = command(rust, path, query, json_output=True, readonly=True)
            if expected != actual or len(actual) != 2:
                raise AssertionError({"cpp": expected, "rust": actual})
            result.update(passed=True, final_rows=actual)
        except Exception as error:
            result["error"] = str(error)
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve earlier reports; select a new report path")
    before = source_fingerprint()
    build = ["cargo", "build", "--offline", "--release", "--no-default-features", "--bin", "duckdb-rust", "--bin", "duckdb-rust-test-worker"]
    subprocess.run(build, cwd=ROOT, check=True)
    worker = ROOT / "target/release/duckdb-rust-test-worker"
    rust = Engine(ROOT / "target/release/duckdb-rust", True)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "source_sha256": before,
              "build_command": build, "rust_worker_sha256": digest(worker), "rust_cli_sha256": digest(rust.binary),
              "script_sha256": digest(Path(__file__)), "targets": [], "full_parity": False,
              "scope": "Selected typed BLOB/UUID SQL, error categories, and native checkpoint/WAL/default/index/mutation round trips. Development is authoritative; no full scalar, diagnostic, performance or engine parity claim."}
    for target, selected in TARGETS.items():
        trial = {"target": target, "sql": [], "persistence": [], "passed": False}
        report["targets"].append(trial)
        try:
            require_checkout(selected.source, target)
            cpp_path, trial["reference_identity"] = require_reference(target=target)
            library = selected.build / "src" / ("libduckdb.dylib" if platform.system() == "Darwin" else "libduckdb.so")
            reference = ROOT / f"target/binary-scalar-reference-{target}"
            compile_command = ["c++", "-std=c++17", "-O3", "-DNDEBUG", "-I"+str(selected.source / "src/include"), str(ROOT / "test/runner/reference.cpp"), str(library), "-Wl,-rpath,"+str(library.parent), "-o", str(reference)]
            subprocess.run(compile_command, check=True)
            trial.update(compile_command=compile_command, cpp_library_sha256=digest(library), cpp_worker_sha256=digest(reference))
            with tempfile.TemporaryDirectory(prefix="ddb-binary-scalar-") as scratch:
                cpp = CppEngine(reference, scratch, time.monotonic()+180)
                actual = RustEngine(worker, scratch, time.monotonic()+180)
                try:
                    if not selected.revision.startswith(cpp.identity["source_id"]):
                        raise ValueError("Loaded reference library identity differs")
                    trial["rust_adapters"] = actual.request({"operation": "describe"})
                    for sql, expected_error in [(sql, None) for sql in SQL] + ERRORS:
                        request = {"operation": "query", "sql": sql}
                        a, b = actual.request(request), cpp.request(request)
                        trial["sql"].append({"sql": sql, "expected_error": expected_error, "rust": a, "cpp": b, "passed": equivalent(a, b, expected_error)})
                finally:
                    actual.close()
                    cpp.close()
                trial["persistence"] = persistence(rust, Engine(cpp_path, False, serialize_json_rows=selected.serialize_json_rows), Path(scratch))
            trial["passed"] = all(case["passed"] for case in trial["sql"] + trial["persistence"])
        except Exception as error:
            trial["error"] = str(error)
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(trial["passed"] for trial in report["targets"])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps({"passed": report["passed"], "report": str(args.report), "targets": [{"target": trial["target"], "failures": [case for case in trial["sql"]+trial["persistence"] if not case["passed"]], "error": trial.get("error")} for trial in report["targets"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
