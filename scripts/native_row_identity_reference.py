"""Retain independent row-group-relative deletion masks from both C++ pins."""
import argparse
from datetime import datetime, timezone
import gzip
import json
from pathlib import Path
import tempfile

from nested_publication_reference import result
from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


QUERY = "SELECT count(*) AS n,min(id) AS lo,max(id) AS hi,sum(id)::VARCHAR AS total,sum(amount)::VARCHAR AS amount FROM t"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/debug/duckdb-rust")
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--reuse-fixtures", action="store_true",
                        help="Read retained images without regenerating or overwriting them")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve earlier evidence")
    if args.reuse_fixtures:
        if not args.fixtures.is_dir():
            raise FileNotFoundError(args.fixtures)
    else:
        args.fixtures.mkdir(parents=True, exist_ok=False)
    before = source_fingerprint()
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(),
              "source_sha256": before, "rust_binary_sha256": digest(args.rust),
              "script_sha256": digest(Path(__file__)), "references": {}, "cases": [],
              "full_parity": False, "scope": __doc__, "reuse_fixtures": args.reuse_fixtures}
    rust = Engine(args.rust, True)
    engines = {}
    for target in ("release", "development"):
        require_checkout(TARGETS[target].source, target)
        binary, identity = require_reference(target=target)
        report["references"][target] = identity
        engines[target] = Engine(binary, False, serialize_json_rows=False)
    with tempfile.TemporaryDirectory(prefix="native-deletion-identity-") as directory:
        for target, version, spelling in [("release", 64, "v1.0.0"), ("release", 68, "v1.5.0"),
                                          ("development", 64, "v1.0.0"), ("development", 69, "v2.0.0")]:
            name = f"{target}-{version}"
            path = Path(directory) / f"{name}.duckdb"
            predicate = "id < 6144 OR id=6161" if version == 69 else "id=1 OR (id>=4096 AND id<6144) OR id=6161"
            sql = (f"SET threads=1; SET max_vacuum_tasks=0; ATTACH '{path}' AS d (STORAGE_VERSION '{spelling}', ROW_GROUP_SIZE 4096); USE d; "
                   "CREATE TABLE t(id INTEGER PRIMARY KEY,amount DECIMAL(12,2),stamp TIMESTAMP_NS,xs INTEGER[]); "
                   "INSERT INTO t SELECT i,i::DECIMAL(12,2)/100,TIMESTAMP_NS '2024-01-02 03:04:05.123456789',[i::INTEGER,NULL] FROM range(8192) r(i); "
                   f"CHECKPOINT; DELETE FROM t WHERE {predicate}; CHECKPOINT")
            case = {"name": name, "producer": target, "version": version, "query": QUERY}
            report["cases"].append(case)
            try:
                fixture = args.fixtures / f"{name}.duckdb.gz"
                if args.reuse_fixtures:
                    path.write_bytes(gzip.decompress(fixture.read_bytes()))
                else:
                    case["setup"] = sql
                    command(engines[target], Path(":memory:"), sql)
                case["rowids"] = result(engines[target], path, "SELECT min(rowid) AS lo,max(rowid) AS hi,count(*) AS n,sum(rowid)::VARCHAR AS total FROM t")
                case["groups"] = result(engines[target], path, "SELECT row_group_id,start,count FROM pragma_storage_info('t') WHERE column_id=0")
                case["expected"] = result(engines["development"], path, QUERY)
                case["rust"] = result(rust, path, QUERY)
                case["passed"] = "rows" in case["expected"] and case["rust"] == case["expected"]
                if not args.reuse_fixtures:
                    fixture.write_bytes(gzip.compress(path.read_bytes(), mtime=0))
                case["fixture"] = {"path": str(fixture), "sha256": digest(path)}
            except Exception as error:
                case.update(passed=False, error=str(error))
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(case["passed"] for case in report["cases"])
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
