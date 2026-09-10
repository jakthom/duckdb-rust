"""Retain exact IEEE-bit floating VARCHAR oracles and selected SQL/native paths."""
import argparse
from datetime import datetime, timezone
import gzip
import hashlib
import json
from pathlib import Path
import platform
import random
import subprocess
import tempfile
import time

from bit_reference import equivalent_bit
from reference_version import TARGETS, require_checkout, require_reference
from run_upstream import RustEngine
from session_reference import CppEngine, source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


SQL = [
    "SELECT concat(1.0::DOUBLE),concat(-0.0::FLOAT),[1.0::DOUBLE,-0.0::DOUBLE,NULL]::VARCHAR,{'a':1.0::DOUBLE,'b':[1e16::DOUBLE]}::VARCHAR",
    "SELECT map([1.0::DOUBLE],[-0.0::DOUBLE])::VARCHAR,union_value(a:=1.0::DOUBLE)::VARCHAR,union_value(a:=NULL::DOUBLE)::VARCHAR,(1.0::DOUBLE)::VARIANT::VARCHAR",
    "SELECT concat([1.0::DOUBLE],[-0.0::DOUBLE])::VARCHAR,concat({'a':1.0::DOUBLE}),concat([1.0::DOUBLE]::VARIANT)::VARCHAR",
    "SELECT [[NULL::DOUBLE,1.0]]::VARCHAR,{'a':NULL::DOUBLE,'b':['','NULL','a,b','a\\b','a''b']}::VARCHAR",
    "SELECT row(1.0::DOUBLE)::VARCHAR,row(1.0::DOUBLE,NULL::DOUBLE)::VARCHAR",
    "SELECT 0.0001::DOUBLE::VARCHAR,0.00001::DOUBLE::VARCHAR,1e15::DOUBLE::VARCHAR,1e16::DOUBLE::VARCHAR,1e100::DOUBLE::VARCHAR",
    "SELECT k FROM (VALUES (1.0::DOUBLE::VARCHAR),((-0.0::DOUBLE)::VARCHAR),(NULL)) t(k) ORDER BY k",
    "SELECT concat(d),[d]::VARCHAR,first_value(d::VARCHAR) OVER(ORDER BY k) FROM (VALUES (1,1.0::DOUBLE),(2,-0.0::DOUBLE),(3,NULL)) t(k,d) ORDER BY k",
]
ERRORS = [
    ("SELECT [make_timestamp(-9223372036854775806)]::VARCHAR", "Internal Error"),
    ("SELECT TRY_CAST({'a':make_timestamp(-9223372036854775806)} AS VARCHAR)", "Internal Error"),
]


def words(kind):
    bits, fraction, exponents = (32, 23, 256) if kind == "FLOAT" else (64, 52, 2048)
    values = set()
    for sign in [0, 1 << (bits - 1)]:
        for exponent in range(exponents):
            for mantissa in [0, 1, (1 << fraction) - 1]:
                values.add(sign | (exponent << fraction) | mantissa)
    rng = random.Random(204693)
    # Retain the original exploratory stream: 5000 FLOAT then 5000 DOUBLE.
    if kind == "DOUBLE":
        for _ in range(5000):
            rng.getrandbits(32)
    values.update(rng.getrandbits(bits) for _ in range(5000))
    return bits, sorted(values)


def persistence(rust, cpp, directory):
    outcomes = []
    definition = ("CREATE TABLE t(k VARCHAR PRIMARY KEY DEFAULT (1.0::DOUBLE),f FLOAT,d DOUBLE,n DOUBLE[]); "
                  "INSERT INTO t DEFAULT VALUES; INSERT INTO t VALUES (concat(-0.0::DOUBLE),1050.46875::FLOAT,-0.0::DOUBLE,[-0.0::DOUBLE,1e16::DOUBLE])")
    query = "SELECT k,f::VARCHAR AS f,d::VARCHAR AS d,n::VARCHAR AS n FROM t ORDER BY k"
    for label, producer in [("cpp", cpp), ("rust-checkpoint", rust), ("rust-wal", Engine(rust.binary, True, ("--durability", "wal")))]:
        case = {"producer": label, "passed": False}
        outcomes.append(case)
        try:
            path = directory / f"floating-text-{label}.duckdb"
            command(producer, path, definition)
            case["checkpoint_sha256"] = digest(path)
            wal = Path(str(path) + ".wal")
            if wal.exists():
                case["wal_sha256"] = digest(wal)
            for stage, mutation in [("initial", None), ("rust-mutation", "BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET d=1e-5,f=1e15 WHERE k='1.0'"), ("cpp-mutation", "UPDATE t SET n=[d,1.0::DOUBLE]; CHECKPOINT")]:
                if mutation:
                    command(rust if stage == "rust-mutation" else cpp, path, mutation)
                expected = command(cpp, path, query, json_output=True, readonly=True)
                actual = command(rust, path, query, json_output=True, readonly=True)
                lookup = "SELECT k FROM t WHERE k=concat(-0.0::DOUBLE)"
                cpp_lookup = command(cpp, path, lookup, json_output=True, readonly=True)
                rust_lookup = command(rust, path, lookup, json_output=True, readonly=True)
                case.setdefault("stages", []).append({"stage": stage, "rust": actual, "cpp": expected, "rust_lookup": rust_lookup, "cpp_lookup": cpp_lookup})
                if actual != expected or cpp_lookup != rust_lookup or len(rust_lookup) != 1:
                    raise AssertionError("native text values or selected-cast lookup differ")
            case["passed"] = True
        except Exception as error:
            case["error"] = str(error)
    return outcomes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--rust", type=Path, help="Diagnostic worker only; omits production build and native campaign")
    args = parser.parse_args()
    if args.report.exists() or args.fixtures.exists():
        raise FileExistsError("Preserve previous evidence; choose fresh report and fixture paths")
    before = source_fingerprint()
    build = None
    if not args.rust:
        build = ["cargo", "build", "--offline", "--release", "--no-default-features", "--bin", "duckdb-rust", "--bin", "duckdb-rust-test-worker"]
        subprocess.run(build, cwd=ROOT, check=True)
    worker = (args.rust or ROOT / "target/release/duckdb-rust-test-worker").resolve(strict=True)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "source_sha256": before, "build_command": build, "diagnostic_only": bool(args.rust), "rust_worker_sha256": digest(worker), "script_sha256": digest(Path(__file__)), "targets": [], "full_parity": False, "scope": "Exact selected FLOAT/DOUBLE VARCHAR conversion over seeded raw IEEE words and every exponent boundary, nested/concat SQL and native interchange. Development is correctness authority. No diagnostic Display or performance acceptance claim."}
    args.fixtures.mkdir(parents=True)
    for target, selected in TARGETS.items():
        trial = {"target": target, "raw_bits": [], "sql": [], "persistence": [], "passed": False}
        report["targets"].append(trial)
        try:
            require_checkout(selected.source, target)
            cpp_path, trial["reference_identity"] = require_reference(target=target)
            library = selected.build / "src" / ("libduckdb.dylib" if platform.system() == "Darwin" else "libduckdb.so")
            reference = ROOT / f"target/floating-text-reference-{target}"
            compile_command = ["c++", "-std=c++17", "-O3", "-DNDEBUG", "-I" + str(selected.source / "src/include"), str(ROOT / "test/runner/reference.cpp"), str(library), "-Wl,-rpath," + str(library.parent), "-o", str(reference)]
            subprocess.run(compile_command, check=True)
            trial.update(compile_command=compile_command, cpp_library_sha256=digest(library), cpp_worker_sha256=digest(reference))
            trial["independent_cli_witnesses"] = []
            for power, expected_text in [(81,"4.835703278458517e+24"),(91,"4.951760157141521e+27"),(807,"A.070116948172427e+242")]:
                decimal = str(1 << power)
                sql = f"SELECT '{decimal}'::DOUBLE::VARCHAR AS text,('{decimal}'::DOUBLE)::BIT AS bits"
                argv = [str(cpp_path), "-csv", "-noheader", "-c", sql]
                result = subprocess.run(argv, text=True, capture_output=True, check=True)
                expected_bits = format((power + 1023) << 52,"064b")
                passed = result.stdout.strip() == expected_text + "," + expected_bits
                trial["independent_cli_witnesses"].append({"command":argv,"stdout":result.stdout,"stderr":result.stderr,"expected_raw_bits":expected_bits,"passed":passed})
                if not passed:
                    raise AssertionError("independent exact-decimal CLI witness differs")
            with tempfile.TemporaryDirectory(prefix="ddb-floating-text-") as scratch:
                cpp = CppEngine(reference, scratch, time.monotonic() + 240)
                actual = RustEngine(worker, scratch, time.monotonic() + 240)
                try:
                    if not selected.revision.startswith(cpp.identity["source_id"]):
                        raise ValueError("Loaded reference identity differs")
                    trial["rust_adapters"] = actual.request({"operation": "describe"})
                    for kind in ["FLOAT", "DOUBLE"]:
                        width, inputs = words(kind)
                        records = []
                        summary = {"kind": kind, "cases": len(inputs), "passed_cases": 0, "failures": []}
                        trial["raw_bits"].append(summary)
                        for start in range(0, len(inputs), 100):
                            query = "SELECT k,v::VARCHAR FROM (VALUES " + ",".join(f"({i},'{word:0{width}b}'::BIT::{kind})" for i, word in enumerate(inputs[start:start + 100], start)) + ") t(k,v) ORDER BY k"
                            a, b = actual.request({"operation": "query", "sql": query}), cpp.request({"operation": "query", "sql": query})
                            if not a.get("ok") or not b.get("ok") or a.get("columns") != ["INTEGER", "VARCHAR"] or b.get("columns") != ["INTEGER", "VARCHAR"] or len(a["rows"]) != len(b["rows"]) or len(a["rows"]) != min(100, len(inputs) - start):
                                raise AssertionError({"kind": kind, "start": start, "rust": a, "cpp": b})
                            for left, right in zip(a["rows"], b["rows"]):
                                if left[0] != right[0]:
                                    raise AssertionError("raw IEEE row identity differs")
                                record = {"bits": format(inputs[int(right[0])], "x"), "cpp": right[1], "rust": left[1]}
                                records.append(record)
                                if left == right:
                                    summary["passed_cases"] += 1
                                else:
                                    summary["failures"].append(record)
                        fixture = args.fixtures / f"{target}-{kind.lower()}.json.gz"
                        payload = json.dumps(records, separators=(",", ":")).encode()
                        fixture.write_bytes(gzip.compress(payload, mtime=0))
                        summary.update(fixture=str(fixture), fixture_sha256=digest(fixture), records_sha256=hashlib.sha256(payload).hexdigest())
                    for sql, expected_error in [(sql, None) for sql in SQL] + ERRORS:
                        request = {"operation": "query", "sql": sql}
                        a, b = actual.request(request), cpp.request(request)
                        trial["sql"].append({"sql": sql, "expected_development_error": expected_error, "rust": a, "cpp": b, "passed": equivalent_bit(a, b, expected_error)})
                finally:
                    actual.close()
                    cpp.close()
                if not args.rust:
                    rust = Engine(ROOT / "target/release/duckdb-rust", True)
                    report["rust_cli_sha256"] = digest(rust.binary)
                    trial["persistence"] = persistence(rust, Engine(cpp_path, False, serialize_json_rows=selected.serialize_json_rows), Path(scratch))
            trial["passed"] = all(item["passed_cases"] == item["cases"] for item in trial["raw_bits"]) and all(item["passed"] for item in trial["sql"] + trial["persistence"])
        except Exception as error:
            trial["error"] = str(error)
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(trial["passed"] for trial in report["targets"])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "report": str(args.report), "targets": [{"target": trial["target"], "raw_bits": [{"kind": item["kind"], "passed": item["passed_cases"], "cases": item["cases"], "failures": item["failures"][:10]} for item in trial["raw_bits"]], "sql_passed": sum(item["passed"] for item in trial["sql"]), "sql_total": len(trial["sql"]), "persistence": trial["persistence"], "error": trial.get("error")} for trial in report["targets"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
