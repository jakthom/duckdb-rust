"""Generate independent DuckDB checkpoint fixtures; DuckDB is a test oracle only."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from reference_version import TARGETS, require_reference

ROOT = Path(__file__).resolve().parents[1]
CASES = {
    "bit_scalar": """
        SET force_compression='uncompressed';
        CREATE TABLE t(id INTEGER PRIMARY KEY, b BIT DEFAULT '001', xs BIT[], s STRUCT(b BIT,d DECIMAL(8,2)));
        INSERT INTO t VALUES
          (0,'0',['1',NULL,'001'],{'b':'01','d':1.25}),
          (1,'1111111',[],{'b':NULL,'d':NULL}),
          (2,'11111111',NULL,NULL),(3,'100000000',NULL,NULL),
          (4,NULL,NULL,NULL),(5,repeat('10010',14001)::BIT,NULL,NULL);
        INSERT INTO t(id) VALUES (6);
        CHECKPOINT;
    """,
    "nested_roaring": """
        SET force_compression='roaring';
        CREATE TABLE t AS SELECT i::INTEGER id,
          {'sparse_null':CASE WHEN i%2048 IN (2,8,2047) THEN NULL ELSE i END,
           'sparse_valid':CASE WHEN i%2048 IN (2,8,2047) THEN i ELSE NULL END,
           'single_run':CASE WHEN i%2048 BETWEEN 300 AND 1100 THEN NULL ELSE i END,
           'many_runs':CASE WHEN i%256 BETWEEN 30 AND 90 THEN NULL ELSE i END,
           'alternating':CASE WHEN i%2=0 THEN NULL ELSE i END,
           'sparse_bit':i%2048 IN (2,8,2047),
           'run_bit':i%2048 BETWEEN 300 AND 1100,
           'many_runs_bit':i%256 BETWEEN 30 AND 90,
           'alternating_bit':i%2=0} s
        FROM range(125013) r(i);
        CHECKPOINT;
    """,
    "nested_scalar": """
        SET force_compression='uncompressed';
        CREATE TABLE t(id INTEGER, xs INTEGER[], a INTEGER[2],
          s STRUCT(x DECIMAL(8,2),y VARCHAR), m MAP(INTEGER,VARCHAR),
          u UNION(i INTEGER,s VARCHAR));
        INSERT INTO t VALUES
          (1,[1,NULL,3],[1,2],{'x':1.25,'y':'a'},map([1,2],['a',NULL]),union_value(i:=7)),
          (2,[],[NULL,4],{'x':NULL,'y':NULL},map([],[]),union_value(s:='x')),
          (3,NULL,NULL,NULL,NULL,NULL);
        CHECKPOINT;
    """,
    "nested_bitpacking": """
        SET force_compression='bitpacking';
        CREATE TABLE t AS SELECT i::INTEGER id,
          CASE WHEN i%17=0 THEN NULL WHEN i%13=0 THEN [] ELSE [i,NULL,i+1] END xs,
          [i,NULL]::BIGINT[2] a,
          {'x':(i%100)::DECIMAL(8,2),'y':CASE WHEN i%19=0 THEN NULL ELSE 'v'||i END} s
        FROM range(10013) r(i);
        CHECKPOINT;
    """,
    "nested_rowgroups": """
        SET force_compression='bitpacking';
        CREATE TABLE t AS SELECT i::INTEGER id,
          CASE WHEN i%17=0 THEN NULL WHEN i%13=0 THEN [] ELSE [i,NULL,i+1] END xs,
          {'x':i::BIGINT,'y':CASE WHEN i%19=0 THEN NULL ELSE i::INTEGER END} s
        FROM range(125013) r(i);
        CHECKPOINT;
    """,
    "dict_fsst_dictionary": """
        SET force_compression='dict_fsst';
        CREATE TABLE t AS SELECT i::INTEGER id,
          CASE WHEN i%11=0 THEN NULL WHEN i%13=0 THEN '' ELSE 'category-' || (i%7) END AS text
        FROM range(10013) r(i);
        CHECKPOINT;
    """,
    "dict_fsst_combined": """
        SET force_compression='dict_fsst';
        CREATE TABLE t AS SELECT i::INTEGER id,
          CASE WHEN i%29=0 THEN NULL WHEN i%31=0 THEN '' ELSE repeat('duckdb-scalar-🦆-',8) || (i%5003) END AS text
        FROM range(10013) r(i);
        CHECKPOINT;
    """,
    "dict_fsst_unique": """
        SET force_compression='dict_fsst';
        CREATE TABLE t AS SELECT i::INTEGER id, repeat('unique-scalar-',8) || i AS text
        FROM range(10013) r(i);
        CHECKPOINT;
    """,
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

for bit_codec in ("dictionary", "fsst", "dict_fsst"):
    CASES[f"bit_{bit_codec}"] = f"""
        SET force_compression='{bit_codec}';
        CREATE TABLE t AS SELECT i::INTEGER id,
          CASE WHEN i%11=0 THEN NULL ELSE (repeat('01',(i%37)::INTEGER+1)||'1')::BIT END b
        FROM range(10013) r(i);
        CHECKPOINT;
    """

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
    parser.add_argument("--target", choices=TARGETS, default="release")
    parser.add_argument("--duckdb", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True, help="Explicit destination; retained historical fixtures are not replaced by default")
    args = parser.parse_args()
    binary, identity = require_reference(args.duckdb, target=args.target)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    version = identity["version"]
    manifest_path = args.output_dir / "manifest.json"
    manifest = json.loads(manifest_path.read_text()) if args.case and manifest_path.exists() else {}
    with tempfile.TemporaryDirectory() as directory:
        for name in args.case or [*CASES, *HISTORICAL]:
            sql = CASES.get(name)
            path = Path(directory) / f"{name}.duckdb"
            if name in HISTORICAL:
                path.write_bytes(HISTORICAL[name].read_bytes())
            else:
                subprocess.run([str(binary), str(path), "-c", sql], check=True, stdout=subprocess.DEVNULL)
            data = path.read_bytes()
            (args.output_dir / f"{name}.duckdb.gz").write_bytes(gzip.compress(data, mtime=0))
            table = "temperatures_double" if name in HISTORICAL else "t"
            codecs = json.loads(subprocess.check_output([str(binary), str(path), "-readonly", "-json", "-c", f"SELECT column_name, segment_type, compression, sum(count) AS n FROM pragma_storage_info('{table}') GROUP BY ALL ORDER BY ALL"], text=True))
            expected_codec = {"alp": "ALP", "alp_float": "ALP", "alprd": "ALPRD", "chimp": "Chimp", "patas": "Patas", "dates_scalar": "Uncompressed", "dates_bitpacking": "BitPacking", "dates_rle": "RLE"}.get(name)
            if expected_codec:
                assert any(row["compression"] == expected_codec and (not name.startswith("dates_") or row["segment_type"] == "DATE") for row in codecs), f"fixture must actually contain {expected_codec} segments of the expected type"
            if name.startswith("bit_"):
                bit_codec = {"bit_scalar":"Uncompressed", "bit_dictionary":"Dictionary", "bit_fsst":"FSST", "bit_dict_fsst":"DICT_FSST"}[name]
                # Development disables legacy Dictionary/FSST writing after
                # storage V1_2_0 (common/enums/compression_type.cpp). A forced
                # disabled codec falls back to Uncompressed; do not claim that
                # such a fixture exercises the requested legacy decoder.
                if args.target == "development" and name in ("bit_dictionary", "bit_fsst"):
                    bit_codec = "Uncompressed"
                assert any(row["compression"] == bit_codec and row["segment_type"] == "BIT" for row in codecs), f"BIT fixture must actually exercise {bit_codec}: {codecs}"
            metadata = {"writer": version, "reference_identity": identity, "size": len(data), "sha256": hashlib.sha256(data).hexdigest(), "observed_compression": codecs, "observed_table": table}
            if name.startswith("dict_fsst_"):
                assert any(row["compression"] == "DICT_FSST" for row in codecs), "fixture must exercise DICT_FSST"
                metadata["dict_fsst_modes"] = json.loads(subprocess.check_output([str(binary),str(path),"-readonly","-json","-c","SELECT segment_info,count FROM pragma_storage_info('t',include_segment_info=true) WHERE compression='DICT_FSST'"],text=True))
                expected_mode = {"dict_fsst_dictionary":"DICTIONARY", "dict_fsst_combined":"DICT_FSST", "dict_fsst_unique":"FSST_ONLY"}[name]
                assert any(row["segment_info"].startswith(expected_mode+":") for row in metadata["dict_fsst_modes"]), f"fixture must actually contain {expected_mode} mode"
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
