"""Compare ordinary mutations and reopen of independent native nested files.

Development is the semantic oracle. Both pinned readers are recorded, including
release version/type limitations. No WAL, performance or full parity claim.
"""
import argparse
from datetime import datetime, timezone
import gzip
import json
from pathlib import Path
import tempfile

from native_version_reference import header
from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


VARIANT_QUERY = "SELECT id,variant_typeof(v) AS dtype,v::VARCHAR AS value_text,xs::VARCHAR AS list_text,s.d::VARCHAR AS decimal_text,s.ts::VARCHAR AS timestamp_text FROM t ORDER BY id"
TUPLE_QUERY = "SELECT id,v::VARCHAR AS value_text,e::VARCHAR AS empty_tuple,s::VARCHAR AS empty_struct,one::VARCHAR AS single_tuple,typeof(v) AS dtype,typeof(e) AS empty_type FROM t ORDER BY id"
CASES = [
    ("development", "nested_variant_unshredded", "nested-variant", VARIANT_QUERY),
    ("development", "nested_variant_shredded", "nested-variant", VARIANT_QUERY),
    ("release", "nested_variant_shredded", "nested-variant", VARIANT_QUERY),
    ("development", "nested_tuple", "nested", TUPLE_QUERY),
]


def result(engine, path, sql):
    try:
        return {"rows": command(engine, path, sql, json_output=True, readonly=True)}
    except Exception as error:
        return {"error": str(error)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/release/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve earlier evidence; choose a new report path")
    before = source_fingerprint()
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(),
              "source_sha256": before, "rust_binary_sha256": digest(args.rust),
              "script_sha256": digest(Path(__file__)), "references": {}, "cases": [],
              "scope": __doc__, "full_parity": False}
    engines = {"rust": Engine(args.rust, True)}
    for target in ("development", "release"):
        require_checkout(TARGETS[target].source, target)
        binary, identity = require_reference(target=target)
        report["references"][target] = identity
        engines[target] = Engine(binary, False, serialize_json_rows=False)
    with tempfile.TemporaryDirectory(prefix="nested-publication-reference-") as directory:
        for target, name, family, sql in CASES:
            source = ROOT / "test/data/duckdb" / f"{family}-{target}"
            manifest = json.loads((source / "manifest.json").read_text())[name]
            original = gzip.decompress((source / f"{name}.duckdb.gz").read_bytes())
            paths = {kind: Path(directory) / f"{target}-{name}-{kind}.duckdb"
                     for kind in ("rust", "development")}
            for path in paths.values():
                path.write_bytes(original)
                if digest(path) != manifest["sha256"]:
                    raise ValueError("Independent fixture digest differs from its manifest")
            case = {"producer": target, "name": name, "producer_identity": manifest["reference_identity"],
                    "fixture_sha256": manifest["sha256"], "sql": sql,
                    "header_before": header(paths["rust"]), "stages": []}
            report["cases"].append(case)
            mutations = [
                ("original", ""),
                ("rollback", "BEGIN; UPDATE t SET id=id+100; DELETE FROM t WHERE id=101; ROLLBACK"),
                ("commit", "UPDATE t SET id=id+100"),
            ]
            if family == "nested-variant":
                mutations.append(("mixed_children", "UPDATE t SET v=CAST({'d':s.d,'ts':s.ts,'items':[s.d,NULL]} AS VARIANT),xs=[v,NULL] WHERE id=100"))
            else:
                mutations.append(("mixed_children", "UPDATE t SET one=row(42),e=row(),s=struct_pack() WHERE id=100"))
            mutations.append(("delete", "DELETE FROM t WHERE id=101"))
            for label, mutation in mutations:
                stage = {"name": label, "mutation": mutation, "mutation_errors": {}}
                case["stages"].append(stage)
                for kind, path in paths.items():
                    if mutation:
                        try:
                            command(engines[kind], path, mutation)
                        except Exception as error:
                            stage["mutation_errors"][kind] = str(error)
                stage["expected"] = result(engines["development"], paths["development"], sql)
                for kind, engine in engines.items():
                    stage[kind] = result(engine, paths["rust"], sql)
                stage["header"] = header(paths["rust"])
                stage["identity_preserved"] = all(stage["header"][key] == case["header_before"][key]
                                                   for key in ("main", "database", "identifier"))
                stage["passed"] = (not stage["mutation_errors"] and stage["identity_preserved"]
                                   and "rows" in stage["expected"]
                                   and stage["rust"] == stage["development"] == stage["expected"])
                stage["release_agrees"] = stage["release"] == stage["expected"]
                if stage["mutation_errors"]:
                    # Later mutations would no longer share the same basis.
                    case["unexecuted_stages"] = [name for name, _ in mutations[len(case["stages"]):]]
                    break
            case["passed"] = len(case["stages"]) == len(mutations) and all(s["passed"] for s in case["stages"])
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(c["passed"] for c in report["cases"])
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"report": str(args.report), "passed": report["passed"],
                      "cases": [{"producer": c["producer"], "name": c["name"],
                                 "stages": [(s["name"], s["passed"], s["mutation_errors"]) for s in c["stages"]]} for c in report["cases"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
