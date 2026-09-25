"""Exact BIGNUM SQL and native interchange against both pinned C++ references."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import platform
import subprocess
import tempfile
import time

from bit_reference import equivalent_bit
from reference_version import TARGETS, require_checkout, require_reference
from run_upstream import RustEngine
from session_reference import CppEngine, source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


WIDE = "340282366920938463463374607431768211456"
SQL = [
    "SELECT 1::BIGNUM,NULL::BIGNUM,typeof(1::VARINT),typeof(NULL::BIGNUM)",
    "SELECT '1.5'::BIGNUM,'-1.5'::BIGNUM,(1.5::FLOAT)::BIGNUM,(1.5::DOUBLE)::BIGNUM,(-0.5::DOUBLE)::BIGNUM,('-0.0'::DOUBLE)::BIGNUM,'-0'::BIGNUM",
    "SELECT '0.400000000000000000001'::BIGNUM,'0.400000000000000000000'::BIGNUM,'0.1000000000000000000001'::BIGNUM,'0.184467440737095516151'::BIGNUM,'0.184467440737095516150'::BIGNUM",
    "SELECT TRY_CAST('' AS BIGNUM),TRY_CAST('.' AS BIGNUM),TRY_CAST('1e2' AS BIGNUM),TRY_CAST('1_2' AS BIGNUM),TRY_CAST(' 1' AS BIGNUM),TRY_CAST('1.0'::DECIMAL(3,1) AS BIGNUM),TRY_CAST('inf'::DOUBLE AS BIGNUM),TRY_CAST('nan'::FLOAT AS BIGNUM)",
    "SELECT TRY_CAST(1::BIGNUM AS FLOAT),TRY_CAST(1::BIGNUM AS DECIMAL(4,0)),TRY_CAST(1::BIGNUM AS BOOLEAN),TRY_CAST(1::BIGNUM AS BLOB),TRY_CAST(1::BIGNUM AS BIT),TRY_CAST(1::BIGNUM AS UUID)",
    f"SELECT '{WIDE}'::BIGNUM::UTINYINT,'{WIDE}'::BIGNUM::HUGEINT,'{WIDE}'::BIGNUM::UHUGEINT,(-1::BIGNUM)::UHUGEINT,(-1::BIGNUM)::HUGEINT",
    f"SELECT ('{WIDE}'::BIGNUM+1)-1,('{WIDE}'::BIGNUM-1)+1,-('{WIDE}'::BIGNUM),'{WIDE}'::BIGNUM+'{WIDE}'::BIGNUM",
    "SELECT (-0.5::DOUBLE)::BIGNUM=0::BIGNUM,(-0.5::DOUBLE)::BIGNUM<0::BIGNUM,-((-0.5::DOUBLE)::BIGNUM),(-0.5::DOUBLE)::BIGNUM+(-0.5::DOUBLE)::BIGNUM,(-0.5::DOUBLE)::BIGNUM+0::BIGNUM",
    "SELECT 1::BIGNUM+1.5::FLOAT,1::BIGNUM+1.5::DOUBLE,1::BIGNUM+1.5::DECIMAL(3,1),typeof(1::BIGNUM+1.5::FLOAT),typeof(1::BIGNUM+1.5::DOUBLE),typeof(1::BIGNUM+1.5::DECIMAL(3,1))",
    "SELECT typeof(+(1::BIGNUM)),typeof(-(1::BIGNUM)),1::BIGNUM*2::BIGNUM,3::BIGNUM/2::BIGNUM,3::BIGNUM//2::BIGNUM,3::BIGNUM%2::BIGNUM",
    "SELECT abs(-1::BIGNUM),round(1::BIGNUM),trunc(1::BIGNUM),sqrt(4::BIGNUM),typeof(abs(1::BIGNUM)),typeof(round(1::BIGNUM)),typeof(trunc(1::BIGNUM))",
    "SELECT coalesce(NULL::BIGNUM,1.5::FLOAT),coalesce(NULL::BIGNUM,1.5::DOUBLE),1::BIGNUM=1.5::FLOAT,1::BIGNUM=1.5::DOUBLE",
    "SELECT hex(1::BIGNUM),hex(0::BIGNUM),hex(256::BIGNUM),to_hex(-256::BIGNUM),hex(-1::BIGNUM),hex((-0.5::DOUBLE)::BIGNUM)",
    "SELECT bin(1::BIGNUM),to_binary(-1::BIGNUM),bin('a'),bin('é'),bin(-1::TINYINT),bin(-1::HUGEINT),bin('340282366920938463463374607431768211455'::UHUGEINT)",
    f"SELECT k FROM (VALUES ('{WIDE}'::BIGNUM),(0::BIGNUM),((-0.5::DOUBLE)::BIGNUM),(-1::BIGNUM),(NULL)) t(k) ORDER BY k",
    f"SELECT k,count(*) FROM (VALUES ('{WIDE}'::BIGNUM),(0::BIGNUM),((-0.5::DOUBLE)::BIGNUM),(-1::BIGNUM),(-1::BIGNUM),(NULL)) t(k) GROUP BY k ORDER BY k",
    f"SELECT sum(v),sum(DISTINCT v),min(v),max(v),count(DISTINCT v),typeof(sum(v)) FROM (VALUES ('{WIDE}'::BIGNUM),(1::BIGNUM),(-1::BIGNUM),(NULL)) t(v)",
    "SELECT sum(v),sum(DISTINCT v),avg(v) FROM (VALUES ((-0.5::DOUBLE)::BIGNUM),(NULL)) t(v)",
    "SELECT sum(v),sum(DISTINCT v),avg(v) FROM (VALUES (1::BIGNUM)) t(v) WHERE false",
    f"SELECT sum(v) OVER(ORDER BY k ROWS UNBOUNDED PRECEDING),sum(DISTINCT v) OVER(ORDER BY k ROWS UNBOUNDED PRECEDING),sum(v) OVER(ORDER BY k ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM (VALUES (1,'{WIDE}'::BIGNUM),(2,1::BIGNUM),(3,-1::BIGNUM),(4,NULL)) t(k,v) ORDER BY k",
    "SELECT count(*) FROM (VALUES (0::BIGNUM),((-0.5::DOUBLE)::BIGNUM),(-1::BIGNUM),(NULL)) a(k) JOIN (VALUES (0::BIGNUM),((-0.5::DOUBLE)::BIGNUM),(-1::BIGNUM),(NULL)) b(k) USING(k)",
    f"SELECT v FROM (VALUES ('{WIDE}'::BIGNUM),(0::BIGNUM),((-0.5::DOUBLE)::BIGNUM),(NULL)) a(v) INTERSECT SELECT v FROM (VALUES ('{WIDE}'::BIGNUM),(0::BIGNUM),(NULL)) b(v) ORDER BY v",
    "SELECT '1.5'::ENUM('1.5','x')::BIGNUM,TRY_CAST('x'::ENUM('1.5','x') AS BIGNUM)",
]
for kind in ("TINYINT", "SMALLINT", "INTEGER", "BIGINT", "HUGEINT", "UTINYINT", "USMALLINT", "UINTEGER", "UBIGINT", "UHUGEINT"):
    SQL.append(f"SELECT 0::{kind}::BIGNUM,1::{kind}::BIGNUM,(-1::BIGNUM)::{kind},'{WIDE}'::BIGNUM::{kind}")
ERRORS = [
    ("SELECT '1e2'::BIGNUM", "Conversion Error"),
    ("SELECT 1.5::DECIMAL(3,1)::BIGNUM", "Conversion Error"),
    ("SELECT 1::BIGNUM::FLOAT", "Conversion Error"),
    ("SELECT 1::BIGNUM::HUGEINT", "Out of Range Error"),
    ("SELECT 1::BIGNUM::UHUGEINT", "Out of Range Error"),
    ("SELECT 128::BIGNUM::TINYINT", "Out of Range Error"),
    ("SELECT TRY_CAST(128::BIGNUM AS TINYINT)", "Internal Error"),
    ("SELECT TRY_CAST(1::BIGNUM AS HUGEINT)", "Internal Error"),
    (f"SELECT '{'9' * 310}'::BIGNUM::DOUBLE", "Conversion Error"),
    (f"SELECT TRY_CAST('{'9' * 310}'::BIGNUM AS DOUBLE)", "Internal Error"),
    ("SELECT coalesce(1::BIGNUM,1.5::DECIMAL(3,1))", "Binder Error"),
    ("SELECT 1::BIGNUM=1.5::DECIMAL(3,1)", "Conversion Error"),
]


def persistence(rust, cpp, directory):
    outcomes = []
    definition = ("CREATE TABLE t(k BIGNUM PRIMARY KEY DEFAULT '1.5',v BIGNUM DEFAULT '-1.5'); INSERT INTO t DEFAULT VALUES; "
                  f"INSERT INTO t VALUES (0,1),((-0.5::DOUBLE)::BIGNUM,NULL),('{WIDE}','{'9' * 10000}')")
    query = "SELECT k::VARCHAR AS k,v::VARCHAR AS v,typeof(k) AS kt,hex(k) AS h FROM t ORDER BY k"
    for label, producer in [("cpp", cpp), ("rust-checkpoint", rust), ("rust-wal", Engine(rust.binary, True, ("--durability", "wal")))]:
        case = {"producer": label, "passed": False}
        outcomes.append(case)
        try:
            path = directory / f"bignum-{label}.duckdb"
            command(producer, path, definition)
            case["checkpoint_sha256"] = digest(path)
            wal = Path(str(path) + ".wal")
            if wal.exists():
                case["wal_sha256"] = digest(wal)
            for stage, mutation in [("initial", None), ("rust-mutation", "BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET v=v+1 WHERE k=2::BIGNUM"), ("cpp-mutation", "DELETE FROM t WHERE k=(-0.5::DOUBLE)::BIGNUM; CHECKPOINT")]:
                if mutation:
                    command(rust if stage == "rust-mutation" else cpp, path, mutation)
                expected = command(cpp, path, query, json_output=True, readonly=True)
                actual = command(rust, path, query, json_output=True, readonly=True)
                lookup = "SELECT k::VARCHAR AS k,v::VARCHAR AS v FROM t WHERE k=0::BIGNUM"
                cpp_lookup = command(cpp, path, lookup, json_output=True, readonly=True)
                rust_lookup = command(rust, path, lookup, json_output=True, readonly=True)
                case.setdefault("stages", []).append({"stage": stage, "rust": actual, "cpp": expected, "rust_lookup": rust_lookup, "cpp_lookup": cpp_lookup})
                if actual != expected or cpp_lookup != rust_lookup or len(rust_lookup) != 1:
                    raise AssertionError("typed native rows or indexed lookup differ")
            case["passed"] = True
        except Exception as error:
            case["error"] = str(error)
    return outcomes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve earlier evidence; choose a new report")
    before = source_fingerprint()
    build = ["cargo", "build", "--offline", "--release", "--no-default-features", "--bin", "duckdb-rust", "--bin", "duckdb-rust-test-worker"]
    subprocess.run(build, cwd=ROOT, check=True)
    worker = ROOT / "target/release/duckdb-rust-test-worker"
    rust = Engine(ROOT / "target/release/duckdb-rust", True)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "source_sha256": before, "build_command": build, "rust_worker_sha256": digest(worker), "rust_cli_sha256": digest(rust.binary), "script_sha256": digest(Path(__file__)), "targets": [], "full_parity": False, "scope": "Selected typed BIGNUM casts, exact arithmetic, aggregate/window semantics, binary functions, error categories and native checkpoint/WAL/default/index/mutation paths. Development governs disagreements. Raw diagnostics retained; exact diagnostic text, full numeric and performance parity are not claimed."}
    for target, selected in TARGETS.items():
        trial = {"target": target, "sql": [], "persistence": [], "passed": False}
        report["targets"].append(trial)
        try:
            require_checkout(selected.source, target)
            cpp_path, trial["reference_identity"] = require_reference(target=target)
            library = selected.build / "src" / ("libduckdb.dylib" if platform.system() == "Darwin" else "libduckdb.so")
            reference = ROOT / f"target/bignum-reference-{target}"
            compile_command = ["c++", "-std=c++17", "-O3", "-DNDEBUG", "-I" + str(selected.source / "src/include"), str(ROOT / "test/runner/reference.cpp"), str(library), "-Wl,-rpath," + str(library.parent), "-o", str(reference)]
            subprocess.run(compile_command, check=True)
            trial.update(compile_command=compile_command, cpp_library_sha256=digest(library), cpp_worker_sha256=digest(reference))
            with tempfile.TemporaryDirectory(prefix="ddb-bignum-reference-") as scratch:
                cpp = CppEngine(reference, scratch, time.monotonic() + 240)
                actual = RustEngine(worker, scratch, time.monotonic() + 240)
                try:
                    if not selected.revision.startswith(cpp.identity["source_id"]):
                        raise ValueError("Loaded reference library identity differs")
                    trial["rust_adapters"] = actual.request({"operation": "describe"})
                    for sql, expected_error in [(sql, None) for sql in SQL] + ERRORS:
                        request = {"operation": "query", "sql": sql}
                        a, b = actual.request(request), cpp.request(request)
                        trial["sql"].append({"sql": sql, "expected_development_error": expected_error, "rust": a, "cpp": b, "passed": equivalent_bit(a, b, expected_error)})
                finally:
                    actual.close()
                    cpp.close()
                trial["persistence"] = persistence(rust, Engine(cpp_path, False, serialize_json_rows=selected.serialize_json_rows), Path(scratch))
            trial["passed"] = all(case["passed"] for case in trial["sql"] + trial["persistence"])
        except Exception as error:
            trial["error"] = str(error)
    report["source_unchanged"] = before == source_fingerprint()
    development = next(trial for trial in report["targets"] if trial["target"] == "development")
    report["development_passed"] = report["source_unchanged"] and development["passed"] and len(development["sql"]) == len(SQL) + len(ERRORS) and len(development["persistence"]) == 3
    report["passed"] = report["source_unchanged"] and all(trial["passed"] for trial in report["targets"])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "development_passed": report["development_passed"], "report": str(args.report), "targets": [{"target": trial["target"], "sql_passed": sum(case["passed"] for case in trial["sql"]), "sql_total": len(trial["sql"]), "persistence": [{"producer": case["producer"], "passed": case["passed"], "error": case.get("error")} for case in trial["persistence"]], "error": trial.get("error")} for trial in report["targets"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
