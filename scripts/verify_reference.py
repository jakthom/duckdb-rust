"""Bidirectional file and query checks against an independent DuckDB executable.

This script is a verification tool; the Rust database never invokes DuckDB.
Run after cargo build --release --bin duckdb-rust.
"""
import argparse
from dataclasses import dataclass
from datetime import date, datetime, timedelta, timezone
import gzip
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import tempfile
import time

import wal_reference
import logging_reference
import checkpoint_reference
import subquery_reference

ROOT = Path(__file__).resolve().parents[1]


@dataclass(frozen=True)
class Engine:
    binary: Path
    rust: bool
    arguments: tuple = ()


def command(engine, path, sql, *, json_output=False, readonly=False):
    args = [str(engine.binary), str(path), *engine.arguments]
    if json_output:
        args.append("--json" if engine.rust else "-json")
    if readonly:
        args.append("--read-only" if engine.rust else "-readonly")
    if json_output and not engine.rust:
        # DuckDB v1.3's shell truncates VARCHAR JSON fields at embedded NULs.
        # Serialize each row in the engine before passing it through the shell.
        sql = f"SELECT CAST(to_json(reference_row) AS VARCHAR) AS encoded_row FROM ({sql}) AS reference_row"
    result = subprocess.run(args + ["-c", sql], capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"{engine.binary.name}: {sql[:160]}\n{result.stderr}")
    if not json_output:
        return result.stdout
    # The reference shell emits no bytes for a successful empty result.
    rows = json.loads(result.stdout) if result.stdout.strip() else []
    return rows if engine.rust else [json.loads(row["encoded_row"], parse_constant={"NaN": "NaN", "Infinity": "inf", "-Infinity": "-inf"}.__getitem__) for row in rows]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/release/duckdb-rust")
    parser.add_argument("--duckdb", type=Path, default=Path("duckdb"))
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    sources = hashlib.sha256()
    for path in sorted([ROOT / "Cargo.toml", ROOT / "Cargo.lock", *(ROOT / "src").rglob("*.rs"), *(ROOT / "tools").rglob("*.rs")]):
        sources.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "source_sha256": sources.hexdigest(),
        "reference": subprocess.check_output([str(args.duckdb), "--version"], text=True).strip(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "platform": platform.platform(),
        "rust_binary_sha256": hashlib.sha256(args.rust.read_bytes()).hexdigest(),
        "checks": [],
    }
    rust = Engine(args.rust, True)
    reference = Engine(args.duckdb, False)
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="duckdb-rust-reference-") as directory:
        directory = Path(directory)
        manifest = json.loads((ROOT / "test/data/duckdb/manifest.json").read_text())
        for name, metadata in manifest.items():
            path = directory / f"{name}.duckdb"
            data = gzip.decompress((ROOT / f"test/data/duckdb/{name}.duckdb.gz").read_bytes())
            assert hashlib.sha256(data).hexdigest() == metadata["sha256"]
            path.write_bytes(data)
            if "observed_compression" in metadata:
                table = metadata.get("observed_table", "t")
                codecs = command(reference, path, f"SELECT column_name, segment_type, compression, sum(count) AS n FROM pragma_storage_info('{table}') GROUP BY ALL ORDER BY ALL", json_output=True, readonly=True)
                assert codecs == metadata["observed_compression"], f"fixture codec selection {name}"
            fixture_query = metadata.get("query", "SELECT * FROM t")
            expected = command(reference, path, fixture_query, json_output=True, readonly=True)
            actual = command(rust, path, fixture_query, json_output=True, readonly=True)
            assert actual == expected, f"decoding fixture {name}"
            command(rust, path, "CREATE TABLE written(i INTEGER NOT NULL); INSERT INTO written VALUES (42)")
            actual = command(reference, path, fixture_query, json_output=True, readonly=True)
            assert actual == expected, f"reencoding fixture {name}"
            assert command(reference, path, "SELECT * FROM written", json_output=True) == [{"i": 42}]
            command(reference, path, "INSERT INTO written VALUES (43); CHECKPOINT")
            assert command(rust, path, "SELECT * FROM written ORDER BY i", json_output=True) == [{"i": 42}, {"i": 43}]
            if name == "indexes":
                for binary in [rust, reference]:
                    for sql in ["INSERT INTO t VALUES (3,'unused',7000,'unused')", "INSERT INTO t VALUES (7000,NULL,3,'part-0')", "INSERT INTO t VALUES (NULL,'unused',7000,'unused')"]:
                        try:
                            command(binary, path, sql)
                        except RuntimeError as error:
                            assert "Constraint Error" in str(error), str(error)
                        else:
                            raise AssertionError(f"{binary}: missing constraint enforcement")
                command(rust, path, "DELETE FROM t WHERE id=5; INSERT INTO t VALUES (7000,'new',7000,'new')")
                assert command(reference, path, "SELECT id FROM t WHERE id=7000", json_output=True) == [{"id": 7000}]
                assert "Index Scan" in command(reference, path, "EXPLAIN ANALYZE SELECT id FROM t WHERE id=7000"), "reference must actually traverse the serialized ART"
                assert command(reference, path, "SELECT id FROM t WHERE id=5", json_output=True) == []
                command(reference, path, "UPDATE t SET u='updated' WHERE id=7000; DELETE FROM t WHERE id=7; CHECKPOINT")
                assert command(rust, path, "SELECT u FROM t WHERE id=7000", json_output=True) == [{"u": "updated"}]
                assert command(rust, path, "SELECT id FROM t WHERE id=7", json_output=True) == []
            if name == "schemas":
                assert command(reference, path, "SELECT * FROM nested.extra", json_output=True) == [{"i": 99}]
                command(reference, path, "CREATE TABLE empty.created(i INTEGER); CHECKPOINT")
                command(rust, path, "DROP TABLE empty.created; DROP SCHEMA empty")
            if name == "defaults":
                command(rust, path, "INSERT INTO t DEFAULT VALUES")
                command(reference, path, "INSERT INTO t DEFAULT VALUES; CHECKPOINT")
                assert command(rust, path, "SELECT * FROM t", json_output=True) == expected * 3
                assert command(reference, path, "SELECT * FROM t", json_output=True) == expected * 3
            report["checks"].append(f"{name}: all values equal, Rust publication, DuckDB publication")
        report["file_adapters"] = json.loads(subprocess.check_output([str(args.rust), str(path), "--read-only", "--adapters"], text=True))
        wal_manifest = json.loads((ROOT / "test/data/wal/manifest.json").read_text())
        for name, metadata in wal_manifest["cases"].items():
            path = directory / f"wal-{name}.duckdb"
            wal_path = Path(str(path) + ".wal")
            checkpoint = gzip.decompress((ROOT / f"test/data/wal/{name}.duckdb.gz").read_bytes())
            log = gzip.decompress((ROOT / f"test/data/wal/{name}.wal.gz").read_bytes())
            assert hashlib.sha256(checkpoint).hexdigest() == metadata["checkpoint_sha256"]
            assert hashlib.sha256(log).hexdigest() == metadata["wal_sha256"]
            path.write_bytes(checkpoint)
            for state in metadata["states"]:
                wal_path.write_bytes(log[:state["end"]])
                if state["rows"] is not None:
                    expected = command(reference, path, metadata["query"], json_output=True, readonly=True)
                    assert expected == state["rows"], f"WAL reference manifest {name}"
                    for _ in range(2):
                        assert command(rust, path, metadata["query"], json_output=True, readonly=True) == expected, f"WAL recovery {name}"
                assert path.read_bytes() == checkpoint and wal_path.read_bytes() == log[:state["end"]], f"read-only recovery changed files: {name}"
            command(rust, path, "CREATE TABLE after_recovery(i INTEGER PRIMARY KEY); INSERT INTO after_recovery VALUES(42)")
            assert not wal_path.exists(), f"WAL was not retired: {name}"
            assert command(reference, path, metadata["query"], json_output=True, readonly=True) == metadata["states"][-1]["rows"]
            command(reference, path, "INSERT INTO after_recovery VALUES(43); CHECKPOINT")
            assert command(rust, path, "SELECT * FROM after_recovery ORDER BY i", json_output=True) == [{"i":42},{"i":43}]
            report["checks"].append(f"WAL {name}: interrupted native process, committed states, read-only and writable recovery, log retirement, mutations in both engines")
        report["recovery_publication"] = wal_reference.verify(rust, reference, command, directory)
        report["checks"].append("WAL publication: 16 process-interruption boundaries, retries, native readers, and Rust-origin root collision")
        for case in report["recovery_publication"]["native_checkpoints"]:
            report["checks"].append(f"Native checkpoint {case['abort']}: read-only recovery, writable recovery and log retirement in Rust, independent DuckDB reads")
        report["transaction_logging"] = logging_reference.verify(rust, Engine(rust.binary, True, ('--durability', 'wal')), reference, command, directory)
        report["checks"].append("Transaction WAL writer: primitive types, catalog changes, row-ID remapping, multi-batch changes, native checkpoint continuation and 10 process-interruption boundaries")
        report["online_checkpointing"] = checkpoint_reference.verify(rust, reference, command, directory)
        report["checks"].append("Online checkpoints: both scheduling policies, explicit checkpoints, continued native writes and 26 manual/automatic interruption outcomes")
        report["subqueries"] = subquery_reference.verify(rust, reference, command, directory)
        report["checks"].append("Subqueries: shared SQL corpus, both consumption adapters, checkpoint/WAL durability and continued writes in both engines")
        path = directory / "rust-rowgroups.duckdb"
        command(rust, path, "CREATE TABLE t AS SELECT range AS i, CASE WHEN range%11=0 THEN NULL ELSE 'row-' || range END AS text FROM range(125000)")
        sql = "SELECT count(*) AS n, sum(i) AS s, count(text) AS valid, sum(length(text)) AS chars FROM t"
        before = command(rust, path, sql, json_output=True)
        assert command(reference, path, sql, json_output=True) == before
        command(reference, path, "INSERT INTO t VALUES (125000,'last'); CHECKPOINT")
        expected = command(reference, path, sql, json_output=True)
        assert command(rust, path, sql, json_output=True) == expected
        command(rust, path, "DELETE FROM t WHERE i>=125000")
        assert command(reference, path, sql, json_output=True) == before
        report["checks"].append("125000 rows: row group boundary, metadata chains, mutations in both engines")
        path = directory / "types.duckdb"
        command(rust, path, "CREATE TABLE t(a TINYINT, b SMALLINT, c INTEGER, d BIGINT, e HUGEINT, f DOUBLE, g VARCHAR, h BOOLEAN); INSERT INTO t VALUES (-128,-32768,-2147483648,-9223372036854775808,-170141183460469231731687303715884105728,0.5,'🦆',true),(127,32767,2147483647,9223372036854775807,170141183460469231731687303715884105727,-0.5,'',false),(NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)")
        sql = "SELECT * FROM t ORDER BY a"
        assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True)
        report["checks"].append("all supported primitive types: integer extrema, NULL, UTF-8")
        command(rust, path, "CREATE TABLE floating(f DOUBLE); INSERT INTO floating VALUES ('NaN'::DOUBLE),('Infinity'::DOUBLE),('-Infinity'::DOUBLE),(-0.0),(0.0),(NULL)")
        sql = "SELECT count(*) AS n, count(f) AS valid, count(DISTINCT f) AS distinct_values, sum(CASE WHEN f='NaN'::DOUBLE THEN 1 ELSE 0 END) AS nans FROM floating"
        assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True)
        report["checks"].append("nonfinite doubles and signed zero retain SQL semantics in DuckDB")
        path = directory / "float32.duckdb"
        command(rust, path, "CREATE TABLE t(f FLOAT UNIQUE); INSERT INTO t VALUES ('NaN'::FLOAT),('Infinity'::FLOAT),('-Infinity'::FLOAT),('-0'::FLOAT),(NULL),(3.4028234663852886e38::FLOAT),(1.401298464324817e-45::FLOAT); CREATE TABLE defaults(v FLOAT DEFAULT 1e-1); INSERT INTO defaults DEFAULT VALUES")
        assert command(reference, path, "SELECT data_type FROM information_schema.columns WHERE table_name='t'", json_output=True) == [{"data_type": "FLOAT"}]
        # Widen exactly before JSON serialization: FLOAT's shortest decimal
        # spelling differs from a DOUBLE spelling of the same binary value.
        sql = "SELECT f::DOUBLE AS f FROM t ORDER BY f"
        assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True)
        for binary in [rust, reference]:
            for value in ["'NaN'", "'0'", "'Infinity'", "'-Infinity'"]:
                try:
                    command(binary, path, f"INSERT INTO t VALUES ({value}::FLOAT)")
                except RuntimeError as error:
                    assert "Constraint Error" in str(error), str(error)
                else:
                    raise AssertionError("FLOAT uniqueness was not enforced")
        command(reference, path, "INSERT INTO defaults DEFAULT VALUES; CHECKPOINT")
        command(rust, path, "INSERT INTO defaults DEFAULT VALUES")
        sql = "SELECT v::DOUBLE AS v FROM defaults"
        assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True)
        command(rust, path, "CREATE TABLE keys(f FLOAT PRIMARY KEY); INSERT INTO keys SELECT range::FLOAT FROM range(6000)")
        assert "Index Scan" in command(reference, path, "EXPLAIN ANALYZE SELECT f FROM keys WHERE f=3000::FLOAT")
        command(reference, path, "UPDATE keys SET f=-f WHERE f=3000::FLOAT; INSERT INTO keys VALUES (6000::FLOAT); CHECKPOINT")
        assert command(rust, path, "SELECT f::DOUBLE AS f FROM keys WHERE f=-3000::FLOAT", json_output=True) == [{"f": -3000.0}]
        command(rust, path, "DELETE FROM keys WHERE f=2::FLOAT")
        for sql in ["SELECT count(*) AS n, sum(f) AS s FROM keys", "SELECT (1::FLOAT/3::FLOAT)::DOUBLE AS division, (16777216::FLOAT+1::FLOAT)::DOUBLE AS rounded, (' 1.5 '::FLOAT)::DOUBLE AS parsed, true::FLOAT::DOUBLE AS boolean_value", "SELECT sum(v) AS s, avg(v) AS a FROM defaults"]:
            assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True), sql
        report["checks"].append("FLOAT: physical schema, extrema, native ART index scan, NaN/zero uniqueness, literal defaults, arithmetic and mutations in both engines")
        path = directory / "dates.duckdb"
        # Python supplies independent finite calendar dates; Rust constructs all
        # of the file metadata, values, defaults and serialized ART nodes.
        dates = [(date(1970, 1, 1) + timedelta(days=i-3000)).isoformat() for i in range(6000)]
        dates += ["-infinity", "infinity", "0001-01-01 (BC)", "5877642-06-25 (BC)", "5881580-07-10"]
        values = ",".join(f"(DATE '{value}')" for value in dates)
        command(rust, path, "CREATE TABLE keys(d DATE PRIMARY KEY); INSERT INTO keys VALUES " + values + "; CREATE TABLE defaults(d DATE DEFAULT DATE '2000-02-29', b DATE DEFAULT DATE '0001-01-01 (BC)', n DATE DEFAULT NULL); INSERT INTO defaults DEFAULT VALUES")
        assert command(reference, path, "SELECT data_type FROM information_schema.columns WHERE table_name='keys'", json_output=True) == [{"data_type": "DATE"}]
        sql = "SELECT d::VARCHAR AS text FROM keys ORDER BY d"
        assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True)
        for value in ["1970-01-01", "0001-01-01 (BC)", "-infinity", "infinity", "5881580-07-10"]:
            assert "Index Scan" in command(reference, path, f"EXPLAIN ANALYZE SELECT d FROM keys WHERE d=DATE '{value}'"), "DATE ART must be traversed by DuckDB"
            for engine in [rust, reference]:
                try:
                    command(engine, path, f"INSERT INTO keys VALUES (DATE '{value}')")
                except RuntimeError as error:
                    assert "Constraint Error" in str(error), str(error)
                else:
                    raise AssertionError("DATE uniqueness was not enforced")
        command(reference, path, "UPDATE keys SET d=DATE '1000-01-01' WHERE d=DATE '1970-01-01'; INSERT INTO defaults DEFAULT VALUES; CHECKPOINT")
        command(rust, path, "DELETE FROM keys WHERE d=DATE 'infinity'; INSERT INTO defaults DEFAULT VALUES")
        for sql in ["SELECT d::VARCHAR AS text FROM keys ORDER BY d", "SELECT d::VARCHAR AS d,b::VARCHAR AS b,n FROM defaults", "SELECT min(d)::VARCHAR AS lo,max(d)::VARCHAR AS hi,count(DISTINCT d) AS n FROM keys"]:
            assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True), sql
        texts = ["epoch", "-epoch", "  INFINITY  ", "0000-01-01", "-0001-01-01", "2024/1/2", "2024 1 2", "0001-1-1 (bc)", "2024-02-29", "", "+2024-01-01", "2023-02-29", "1900-02-29", "2024-13-01", "2024-00-01", "2024-01-00", "2024-01-32", "2024/01-02", "5881580-07-11", "5877642-06-24 (BC)", "0000-01-01 (BC)", "-0001-01-01 (BC)", "99999999999999999999999-01-01", "2024-01-012", "infinityx"]
        values = ",".join(f"('{value}')" for value in texts)
        sql = f"SELECT v,TRY_CAST(v AS DATE)::VARCHAR AS d FROM (VALUES {values}) input(v) ORDER BY v"
        assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True)
        report["checks"].append("DATE: calendar casts, BC/finite extrema/infinities, physical schema, native ART traversal and uniqueness, defaults, and mutations in both engines")
        path = directory / "operators.duckdb"
        command(rust, path, "CREATE TABLE widths AS SELECT 1+2 AS i, 2::TINYINT+3::SMALLINT AS s, 1::TINYINT+2 AS t, 5//2 AS q, NULL+NULL AS n, 1::FLOAT/3::FLOAT AS f; CREATE TABLE dates(d DATE DEFAULT DATE '2000-03-01'-1, label VARCHAR DEFAULT 'du'||'ck'); INSERT INTO dates DEFAULT VALUES; INSERT INTO dates(d) SELECT DATE 'epoch'+range::INTEGER FROM range(1000)")
        expected_types = [{"column_name": name, "data_type": kind} for name, kind in [("f","FLOAT"),("i","INTEGER"),("n","BIGINT"),("q","INTEGER"),("s","SMALLINT"),("t","TINYINT")]]
        assert command(reference, path, "SELECT column_name,data_type FROM information_schema.columns WHERE table_name='widths' ORDER BY column_name", json_output=True) == expected_types
        for sql in [
            "SELECT i,s,t,q,n,f::DOUBLE AS f FROM widths",
            "SELECT d::VARCHAR AS d,label FROM dates ORDER BY d",
            "SELECT DATE 'epoch'+1 AS d, 1+DATE 'epoch' AS commuted, DATE 'epoch'-DATE '1969-12-31' AS delta, DATE '5881580-07-10'-DATE '5877642-06-25 (BC)' AS span, DATE '-infinity'-DATE 'infinity' AS infinite_delta, DATE 'infinity'+2147483647 AS infinity",
            "SELECT -5//2 AS q, -5%2 AS r, 1//0 AS zero, 1::FLOAT//0::FLOAT AS fzero, 5.5::DOUBLE//2::DOUBLE AS fdivision",
            "SELECT CASE WHEN false THEN 127::TINYINT+1 ELSE 7 END AS lazy_overflow",
            "SELECT min(d)::VARCHAR AS lo,max(d)::VARCHAR AS hi,sum(d-DATE 'epoch') AS days FROM dates",
        ]:
            assert command(rust,path,sql,json_output=True) == command(reference,path,sql,json_output=True), sql
        values = ["", "a", "ab", "aba", "🦆", "🦅", "a🦆🦅z", "a🦆b", "a_b", "a%b", "é", "è", "中", "丰", "é"]
        patterns = ["", "%", "_", "__", "a%", "%b", "a_b", "%🦆%", "%a%b%", "a__b", "é", "e_", "%🦅_", "%è", "%中_", "%丰"]
        pairs = ",".join(f"('{v}','{p}')" for v in values for p in patterns)
        sql = f"SELECT v,p,v LIKE p AS matched,v NOT LIKE p AS negated FROM (VALUES {pairs}) input(v,p) ORDER BY v,p"
        assert command(rust,path,sql,json_output=True) == command(reference,path,sql,json_output=True)
        for data_type, minimum, maximum in [("TINYINT",-128,127),("SMALLINT",-32768,32767),("INTEGER",-2147483648,2147483647),("BIGINT",-9223372036854775808,9223372036854775807),("HUGEINT",-(2**127),2**127-1)]:
            for expr in [f"({maximum})::{data_type}+1", f"({minimum})::{data_type}%(-1)::{data_type}", f"({minimum})::{data_type}//(-1)::{data_type}"]:
                for engine in [rust,reference]:
                    try:
                        command(engine,path,"SELECT "+expr)
                    except RuntimeError as error:
                        assert ("Execution Error" if engine.rust else "Out of Range Error") in str(error), str(error)
                    else:
                        raise AssertionError(f"missing overflow: {engine} {expr}")
        command(reference,path,"UPDATE dates SET d=d+1 WHERE d=DATE 'epoch'; INSERT INTO dates DEFAULT VALUES; CHECKPOINT")
        command(rust,path,"UPDATE dates SET d=d-1 WHERE d=DATE '2000-02-29'")
        sql="SELECT d::VARCHAR AS d,label FROM dates ORDER BY d"
        assert command(rust,path,sql,json_output=True) == command(reference,path,sql,json_output=True)
        report["checks"].append("operator overloads: integer literal widths, checked arithmetic and division, DATE offsets/differences, Unicode LIKE, constant defaults, and mutations in both engines")
        path = directory / "cast-contracts.duckdb"
        command(rust, path, "CREATE TABLE casts(i INTEGER PRIMARY KEY, n SMALLINT DEFAULT '7'); INSERT INTO casts(i) VALUES ('41'),('42'); UPDATE casts SET n='8' WHERE i=41")
        values = ["-170141183460469231731687303715884105728", "170141183460469231731687303715884105727", "170141183460469231731687303715884105728", "-129", "-128", "127", "128", "-32769", "32768", "9223372036854775808", " +42 ", "", "+", "--1", "bad"]
        inputs = ",".join("('" + value + "')" for value in values)
        for sql in [
            "SELECT i,n FROM casts ORDER BY i",
            "SELECT i FROM casts WHERE i=CAST('42' AS INTEGER)",
            f"SELECT v, try_cast(v AS TINYINT) AS t, try_cast(v AS SMALLINT) AS s, try_cast(v AS INTEGER) AS i, try_cast(v AS BIGINT) AS b, try_cast(v AS HUGEINT) AS h FROM (VALUES {inputs}) input(v) ORDER BY v",
            "SELECT (-1.5::DOUBLE)::INTEGER AS rounded, (16777217::INTEGER)::FLOAT::DOUBLE AS narrowed, true::DOUBLE AS boolean_value",
        ]:
            assert command(rust, path, sql, json_output=True) == command(reference, path, sql, json_output=True), sql
        command(reference, path, "INSERT INTO casts(i) VALUES ('43'); CHECKPOINT")
        assert command(rust, path, "SELECT n FROM casts WHERE i=43", json_output=True) == [{"n": 7}]
        report["checks"].append("cast contracts: integer extrema/invalid inputs, numeric rounding, explicit/assignment casts, defaults and indexed predicates")
        path = directory / "index-boundaries.duckdb"
        command(rust, path, "CREATE TABLE empty(k INTEGER PRIMARY KEY); CREATE TABLE floating(v DOUBLE UNIQUE); INSERT INTO floating VALUES ('NaN'::DOUBLE),('Infinity'::DOUBLE),('-Infinity'::DOUBLE),(-0.0),(NULL)")
        command(reference, path, "INSERT INTO empty VALUES (9); CHECKPOINT")
        assert command(rust, path, "SELECT k FROM empty WHERE k=9", json_output=True) == [{"k": 9}]
        for binary in [rust, reference]:
            for value in ["'NaN'::DOUBLE", "0.0", "'Infinity'::DOUBLE", "'-Infinity'::DOUBLE"]:
                try:
                    command(binary, path, f"INSERT INTO floating VALUES ({value})")
                except RuntimeError as error:
                    assert "Constraint Error" in str(error), str(error)
                else:
                    raise AssertionError("floating-point index did not enforce uniqueness")
        sql = "SELECT count(*) AS n FROM floating WHERE v='NaN'::DOUBLE"
        assert command(reference, path, sql, json_output=True) == [{"n": 1}]
        assert command(rust, path, sql, json_output=True) == [{"n": 1}]
        command(rust, path, "CREATE TABLE prefixes(k VARCHAR PRIMARY KEY)")
        keys = ["a" * i for i in range(301)] + ["z" * 20000]
        command(rust, path, "INSERT INTO prefixes VALUES " + ",".join(f"('{key}')" for key in keys))
        sql = "SELECT count(*) AS n, sum(length(k)) AS chars FROM prefixes"
        assert command(reference, path, sql, json_output=True) == command(rust, path, sql, json_output=True)
        command(reference, path, "DELETE FROM prefixes WHERE k='aaa'; INSERT INTO prefixes VALUES ('replacement'); CHECKPOINT")
        assert command(reference, path, sql, json_output=True) == command(rust, path, sql, json_output=True)
        report["checks"].append("ART traversal: reference index scan, empty root, NaN/zero/extrema uniqueness, deep prefixes and 20 KiB keys")
    report["elapsed_seconds"] = round(time.monotonic() - start, 3)
    report["result"] = "passed"
    output = json.dumps(report, indent=2, ensure_ascii=False) + "\n"
    if args.report:
        args.report.write_text(output)
    print(output, end="")


if __name__ == "__main__":
    main()
