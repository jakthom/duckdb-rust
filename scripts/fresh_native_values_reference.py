"""Check Rust-created native storage 64–69 with both pinned C++ readers.

Development-mutated twin files are the content oracle. Release version limits
remain explicit. This is selected bidirectional compatibility, not performance.
"""
import argparse
from datetime import datetime, timezone
import json
import gzip
from pathlib import Path
import tempfile

from native_version_reference import header
from nested_publication_reference import result
from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


VERSIONS = [(64, "v1.0.0"), (65, "v1.2.0"), (66, "v1.3.0"),
            (67, "v1.4.0"), (68, "v1.5.0"), (69, "v2.0.0")]


def workload(version):
    columns = "id INTEGER PRIMARY KEY,n STRUCT(d DECIMAL(12,2),ts TIMESTAMP_NS),xs INTEGER[],b BLOB,u UHUGEINT"
    rows = ["1,{'d':12.34,'ts':TIMESTAMP_NS '2024-01-02 03:04:05.123456789'},[1,NULL,2],'a\\x00b'::BLOB,'340282366920938463463374607431768211455'::UHUGEINT",
            "2,NULL,[],''::BLOB,0"]
    query = "SELECT id,n.d::VARCHAR AS amount,n.ts::VARCHAR AS stamp,xs::VARCHAR AS items,hex(b) AS binary_text,u::VARCHAR AS unsigned_text"
    if version >= 68:
        columns += ",v VARIANT,vs VARIANT[]"
        rows[0] += ",CAST({'d':12.34::DECIMAL(8,2),'ts':TIMESTAMP_NS '2024-01-02 03:04:05.123456789','items':[1,NULL,2]} AS VARIANT),[1::VARIANT,NULL]"
        rows[1] += ",NULL,[]"
        query += ",v::VARCHAR AS dynamic_text,variant_typeof(v) AS dynamic_type,vs::VARCHAR AS dynamic_items"
    if version == 69:
        columns += ",p TUPLE(DECIMAL(8,2),TIMESTAMP_NS)"
        rows[0] += ",row(12.34,TIMESTAMP_NS '2024-01-02 03:04:05.123456789')"
        rows[1] += ",row(NULL,NULL)"
        query += ",p::VARCHAR AS tuple_text,typeof(p) AS tuple_type"
    sql = f"CREATE TABLE t({columns}); INSERT INTO t VALUES " + ",".join(f"({row})" for row in rows)
    if version == 69:
        sql += "; CREATE TABLE empty_values AS SELECT row() e,struct_pack() s"
        query += ",e::VARCHAR AS empty_tuple,s::VARCHAR AS empty_struct"
    return sql, query + " FROM t" + (" CROSS JOIN empty_values" if version == 69 else "") + " ORDER BY id"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/release/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--failure-fixtures", type=Path,
                        help="Export failed Rust/development twin images into a new fixture directory")
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve earlier evidence; choose a new report path")
    if args.failure_fixtures:
        args.failure_fixtures.mkdir(parents=True, exist_ok=False)
    before = source_fingerprint()
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(),
              "source_sha256": before, "rust_binary_sha256": digest(args.rust),
              "shell_sha256": digest(ROOT / "tools/shell/main.rs"),
              "script_sha256": digest(Path(__file__)), "references": {}, "cases": [],
              "scope": __doc__, "full_parity": False}
    engines = {"rust": Engine(args.rust, True)}
    for target in ("development", "release"):
        require_checkout(TARGETS[target].source, target)
        binary, identity = require_reference(target=target)
        report["references"][target] = identity
        engines[target] = Engine(binary, False, serialize_json_rows=False)
    with tempfile.TemporaryDirectory(prefix="fresh-native-values-") as directory:
        for version, spelling in VERSIONS:
            path = Path(directory) / f"rust-{version}.duckdb"
            twin = Path(directory) / f"development-{version}.duckdb"
            sql, query = workload(version)
            case = {"version": version, "setup": sql, "query": query, "stages": []}
            report["cases"].append(case)
            try:
                command(Engine(args.rust, True, ("--storage-version", str(version))), path, sql)
                command(engines["development"], Path(":memory:"),
                        f"ATTACH '{twin}' AS db (STORAGE_VERSION '{spelling}'); USE db; {sql}; CHECKPOINT")
                initial = header(path)
                case["header_created"] = initial
                for label, mutation, writer in [
                    ("created", "", "rust"),
                    ("rollback", "BEGIN; UPDATE t SET id=id+10; DELETE FROM t WHERE id=12; ROLLBACK", "rust"),
                    ("rust_commit", "UPDATE t SET id=id+10 WHERE id=1; DELETE FROM t WHERE id=2", "rust"),
                    ("cpp_commit", "UPDATE t SET xs=[3,NULL,4],n={'d':56.78,'ts':TIMESTAMP_NS '2025-02-03 04:05:06.987654321'} WHERE id=11; CHECKPOINT", "development"),
                ]:
                    previous = digest(path)
                    if mutation:
                        command(engines[writer], path, mutation)
                        command(engines["development"], twin, mutation)
                    expected = result(engines["development"], twin, query)
                    stage = {"name": label, "mutation": mutation, "expected": expected,
                             "readers": {kind: result(engine, path, query) for kind, engine in engines.items()},
                             "header": header(path)}
                    stage["version_preserved"] = stage["header"]["effective"] == version
                    stage["identity_preserved"] = stage["header"]["identifier"] == initial["identifier"]
                    stage["rollback_unchanged"] = label != "rollback" or digest(path) == previous
                    stage["passed"] = ("rows" in expected and stage["readers"]["rust"] == stage["readers"]["development"] == expected
                                       and stage["version_preserved"] and stage["identity_preserved"] and stage["rollback_unchanged"])
                    stage["release_agrees"] = stage["readers"]["release"] == expected
                    case["stages"].append(stage)
                case["passed"] = all(stage["passed"] for stage in case["stages"])
            except Exception as error:
                case.update(passed=False, error=str(error))
            if not case["passed"] and args.failure_fixtures:
                case["exported_images"] = {}
                for kind, image in [("rust", path), ("development", twin)]:
                    if image.exists():
                        target = args.failure_fixtures / f"storage-{version}-{kind}.duckdb.gz"
                        target.write_bytes(gzip.compress(image.read_bytes(), mtime=0))
                        case["exported_images"][kind] = {"path": str(target), "sha256": digest(image)}
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(case["passed"] for case in report["cases"])
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"report": str(args.report), "passed": report["passed"],
                      "cases": [{"version": c["version"], "passed": c["passed"], "error": c.get("error"),
                                 "stages": [(s["name"], s["passed"], s["release_agrees"]) for s in c["stages"]]} for c in report["cases"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
