"""Generate independent DuckDB checkpoint fixtures; DuckDB is a test oracle only."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "test" / "data" / "duckdb"
CASES = {
    "dates_scalar": """
        SET force_compression='uncompressed';
        CREATE TABLE t(id INTEGER, d DATE, c DATE DEFAULT DATE '2000-02-29');
        INSERT INTO t(id,d) VALUES (0,DATE '-infinity'),(1,DATE 'infinity'),
          (2,DATE '5877642-06-25 (BC)'),(3,DATE '5881580-07-10'),
          (4,DATE '0001-01-01 (BC)'),(5,DATE '0001-01-01'),
          (6,DATE '1969-12-31'),(7,DATE '1970-01-01'),(8,DATE '2000-02-29'),(9,NULL);
        CHECKPOINT;
    """,
    "dates_bitpacking": """
        SET force_compression='bitpacking';
        CREATE TABLE t AS SELECT i::INTEGER id,
          CASE WHEN i%29=0 THEN NULL ELSE DATE '1800-01-01' + i::INTEGER END AS d,
          CASE WHEN i%31=0 THEN NULL ELSE DATE '2000-02-29' END AS c
        FROM range(125013) r(i);
        CHECKPOINT;
    """,
    "dates_rle": """
        SET force_compression='rle';
        CREATE TABLE t AS SELECT i::INTEGER id,
          CASE WHEN i%1000<100 THEN NULL ELSE DATE '1960-01-01' + (i//100)::INTEGER END AS d,
          DATE 'infinity' AS c
        FROM range(10000) r(i);
        CHECKPOINT;
    """,
    "alp": """
        SET force_compression='alp';
        CREATE TABLE t AS SELECT i::INTEGER id,
          CASE WHEN i%29=0 THEN NULL WHEN i%1001=1 THEN 'NaN'::DOUBLE WHEN i%1001=2 THEN 'Infinity'::DOUBLE WHEN i%1001=3 THEN '-Infinity'::DOUBLE WHEN i%1001=4 THEN 1.7976931348623157e308 WHEN i%1001=5 THEN 4.9406564584124654e-324 ELSE (i-65000)::DOUBLE / 100 END AS value,
          (1000000000000000 + i%13)::DOUBLE AS large,
          (i%1000)::DOUBLE / 1e18 AS tiny
        FROM range(125013) r(i);
        CHECKPOINT;
    """,
    "defaults": """
        CREATE TABLE t(a TINYINT DEFAULT -128, b SMALLINT DEFAULT 32767, c INTEGER DEFAULT -2147483648, d BIGINT DEFAULT 9223372036854775807, e HUGEINT DEFAULT '-170141183460469231731687303715884105728'::HUGEINT, f DOUBLE DEFAULT 5e-1, g VARCHAR DEFAULT 'quack''🦆', h BOOLEAN DEFAULT true, n INTEGER DEFAULT NULL);
        INSERT INTO t DEFAULT VALUES;
        CHECKPOINT;
    """,
    "indexes": """
        CREATE TABLE t(id INTEGER PRIMARY KEY, u VARCHAR UNIQUE, a INTEGER, b VARCHAR, UNIQUE(a,b));
        INSERT INTO t SELECT i, CASE WHEN i%11=0 THEN NULL ELSE 'prefix-' || chr(0) || chr(1) || i END, i%32, 'part-' || i//32 FROM range(6000) r(i);
        CHECKPOINT;
    """,
    "deletions": """
        CREATE TABLE t AS SELECT i::INTEGER id FROM range(130000) r(i);
        CHECKPOINT;
        DELETE FROM t WHERE id=1 OR (id>=2048 AND id<4096) OR (id>=4096 AND id<6144 AND id%2=0) OR (id>=6144 AND id<8192 AND id<>7000) OR id=125000;
        CHECKPOINT;
    """,
    "schemas": """
        CREATE SCHEMA empty;
        CREATE SCHEMA nested;
        CREATE TABLE nested.extra(i INTEGER);
        INSERT INTO nested.extra VALUES (99);
        CREATE TABLE t(i INTEGER);
        INSERT INTO t VALUES (1);
        CHECKPOINT;
    """,
    "rowgroups": """
        SET force_compression='uncompressed';
        CREATE TABLE t AS SELECT i::BIGINT id FROM range(125000) r(i);
        CHECKPOINT;
    """,
    "scalar": """
        SET force_compression='uncompressed';
        CREATE TABLE t(id INTEGER, name VARCHAR, value DOUBLE, active BOOLEAN);
        INSERT INTO t VALUES (1,'one',1.5,true),(2,NULL,2.5,false),(3,'three',NULL,NULL);
        CHECKPOINT;
    """,
    "bitpacking": """
        CREATE TABLE t AS SELECT i::INTEGER id, (i % 7)::BIGINT k FROM range(10000) r(i);
        CHECKPOINT;
    """,
    "rle": """
        SET force_compression='rle';
        CREATE TABLE t AS SELECT (i // 100)::INTEGER id FROM range(10000) r(i);
        CHECKPOINT;
    """,
    "dictionary": """
        SET force_compression='dictionary';
        CREATE TABLE t AS SELECT CASE WHEN i%11=0 THEN NULL ELSE 'category-' || (i%7) END AS text FROM range(10000) r(i);
        CHECKPOINT;
    """,
    "fsst": """
        SET force_compression='fsst';
        CREATE TABLE t AS SELECT CASE WHEN i%11=0 THEN NULL ELSE repeat('duckdb-',10) || i END AS text FROM range(10000) r(i);
        CHECKPOINT;
    """,
    "overflow": """
        SET force_compression='uncompressed';
        CREATE TABLE t(id INTEGER, text VARCHAR);
        INSERT INTO t VALUES (1,repeat('a',8000)),(2,repeat('b',300000)),(3,''),(4,NULL);
        CHECKPOINT;
    """,
}

FLOATING_SQL = """
    SET force_compression='{codec}';
    CREATE TABLE t AS SELECT i::INTEGER id,
      CASE WHEN i%29=0 THEN NULL WHEN i%1001=1 THEN 'NaN'::DOUBLE WHEN i%1001=2 THEN 'Infinity'::DOUBLE WHEN i%1001=3 THEN '-Infinity'::DOUBLE WHEN i%1001=4 THEN 1.7976931348623157e308 WHEN i%1001=5 THEN 4.9406564584124654e-324 WHEN i%1001=6 THEN '-0'::DOUBLE ELSE (i-65000)::DOUBLE / 17 END AS d,
      CASE WHEN i%29=0 THEN NULL WHEN i%1001=1 THEN 'NaN'::FLOAT WHEN i%1001=2 THEN 'Infinity'::FLOAT WHEN i%1001=3 THEN '-Infinity'::FLOAT WHEN i%1001=4 THEN 3.4028234663852886e38::FLOAT WHEN i%1001=5 THEN 1.401298464324817e-45::FLOAT WHEN i%1001=6 THEN '-0'::FLOAT ELSE (i-65000)::FLOAT / 17::FLOAT END AS f
    FROM range(125013) r(i);
    CHECKPOINT;
"""
CASES.update({name: FLOATING_SQL.format(codec=codec) for name, codec in [("alprd", "alprd"), ("alp_float", "alp")]})
HISTORICAL = {codec: ROOT.parent / f"duckdb/test/sql/storage/compression/{codec}/{codec}.db" for codec in ["chimp", "patas"]}



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", action="append", choices=[*CASES, *HISTORICAL], help="Generate only the named fixture; repeat to select several")
    args = parser.parse_args()
    FIXTURES.mkdir(parents=True, exist_ok=True)
    version = subprocess.check_output(["duckdb", "--version"], text=True).strip()
    manifest_path = FIXTURES / "manifest.json"
    manifest = json.loads(manifest_path.read_text()) if args.case and manifest_path.exists() else {}
    with tempfile.TemporaryDirectory() as directory:
        for name in args.case or [*CASES, *HISTORICAL]:
            sql = CASES.get(name)
            path = Path(directory) / f"{name}.duckdb"
            if name in HISTORICAL:
                path.write_bytes(HISTORICAL[name].read_bytes())
            else:
                subprocess.run(["duckdb", str(path), "-c", sql], check=True, stdout=subprocess.DEVNULL)
            data = path.read_bytes()
            (FIXTURES / f"{name}.duckdb.gz").write_bytes(gzip.compress(data, mtime=0))
            table = "temperatures_double" if name in HISTORICAL else "t"
            codecs = json.loads(subprocess.check_output(["duckdb", str(path), "-readonly", "-json", "-c", f"SELECT column_name, segment_type, compression, sum(count) AS n FROM pragma_storage_info('{table}') GROUP BY ALL ORDER BY ALL"], text=True))
            expected_codec = {"alp": "ALP", "alp_float": "ALP", "alprd": "ALPRD", "chimp": "Chimp", "patas": "Patas", "dates_scalar": "Uncompressed", "dates_bitpacking": "BitPacking", "dates_rle": "RLE"}.get(name)
            if expected_codec:
                assert any(row["compression"] == expected_codec and (not name.startswith("dates_") or row["segment_type"] == "DATE") for row in codecs), f"fixture must actually contain {expected_codec} segments of the expected type"
            metadata = {"writer": version, "size": len(data), "sha256": hashlib.sha256(data).hexdigest(), "observed_compression": codecs, "observed_table": table}
            if name in HISTORICAL:
                metadata.update({"writer": "historical DuckDB artifact; original writer revision unrecorded", "reference_reader": version, "source": str(HISTORICAL[name].relative_to(ROOT.parent)), "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT.parent/"duckdb", text=True).strip(), "query": "SELECT temperature::DOUBLE AS value FROM temperatures_double UNION ALL SELECT temperature::DOUBLE AS value FROM temperatures_float"})
            else:
                metadata["sql"] = sql.strip()
                if name.startswith("dates_"):
                    metadata["query"] = "SELECT id, d::VARCHAR AS d, c::VARCHAR AS c FROM t ORDER BY id"
                if name in ["alprd", "alp_float"]:
                    metadata["query"] = "SELECT id, d, f::DOUBLE AS f FROM t"
            manifest[name] = metadata
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
