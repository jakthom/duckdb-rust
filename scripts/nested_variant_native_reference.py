"""Read independently produced VARIANT checkpoints through typed SQL.

This is a bounded read-side compatibility diagnostic, not a native writer,
WAL, performance, or full-family parity claim. Earlier reports are immutable.
"""
import argparse
from datetime import datetime, timezone
import gzip
import json
from pathlib import Path
import tempfile

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


FIXTURES = [
    ("development", "nested_variant_unshredded"),
    ("development", "nested_variant_shredded"),
    ("release", "nested_variant_shredded"),
]
QUERIES = [
    ("dynamic_children", "SELECT id,variant_typeof(v) type,v::VARCHAR value FROM t ORDER BY id"),
    ("mixed_schema", "SELECT id,s.d,s.ts::VARCHAR timestamp,xs::VARCHAR children FROM t ORDER BY id"),
    ("root_validity", "SELECT id,v IS NULL missing,variant_typeof(v) type FROM t ORDER BY id"),
    ("counts", "SELECT count(*) total,count(v) present,count(DISTINCT v) unique_values FROM t"),
    ("nested_nulls", "SELECT count(*) total FROM t WHERE xs[2] IS NULL"),
    ("child_comparison", "SELECT id FROM t WHERE v=xs[1] ORDER BY id"),
    ("distinct", "SELECT count(*) total FROM (SELECT DISTINCT v FROM t) q"),
    ("groups", "SELECT count(*) total FROM (SELECT v,count(*) FROM t GROUP BY v) q"),
    ("projected_join", "SELECT count(*) total FROM t a JOIN (SELECT xs[1] k FROM t) b ON a.v=b.k"),
    ("qualified_subscript_join", "SELECT count(*) total FROM t a JOIN t b ON a.v=b.xs[1]"),
    ("sort", "SELECT id FROM t ORDER BY v,id"),
    ("windows", "SELECT id,count(*) OVER (PARTITION BY v) peers FROM t ORDER BY id"),
]
UNSHREDDED = [
    ("bignum_payload", "SELECT id,v::BIGNUM::VARCHAR value FROM t WHERE id IN (1,2) ORDER BY id"),
    ("exact_scalar_tags", "SELECT id,variant_typeof(v) type FROM t WHERE id IN (3,13,16,17,18,19) ORDER BY id"),
    ("blob_payload", "SELECT v::BLOB=from_hex('610062') exact_bytes FROM t WHERE id=11"),
    ("bit_payload", "SELECT v::BIT::VARCHAR value FROM t WHERE id=12"),
    ("temporal_payload", "SELECT v::TIMESTAMP_NS::VARCHAR value FROM t WHERE id=13"),
]
SHREDDED = [
    ("missing_vs_null", "SELECT id,variant_exists(v,'a') present,v.a::VARCHAR value FROM t WHERE id IN (2,4) ORDER BY id"),
    ("object_overlay", "SELECT v.a::VARCHAR value,v.extra::VARCHAR leftover FROM t WHERE id=3"),
    ("typed_decimal", "SELECT v.d::DECIMAL(12,2) value FROM t WHERE id=0"),
    ("typed_array", "SELECT v.items::INTEGER[]::VARCHAR value FROM t WHERE id IN (0,1,2,4) ORDER BY id"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/debug/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve earlier evidence; choose a new report path")
    references = {}
    for target in ("development", "release"):
        require_checkout(TARGETS[target].source, target)
        references[target] = require_reference(target=target)
    before = source_fingerprint()
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "source_sha256": before,
        "rust_binary_sha256": digest(args.rust),
        "script_sha256": digest(Path(__file__)),
        "reference_identities": {key: value[1] for key, value in references.items()},
        "scope": "Read-only native VARIANT unshredded/shredded checkpoints through typed SQL. Development semantics take precedence; release producer coverage is separate. No Rust VARIANT writer/WAL or full compatibility claim.",
        "unsupported": [
            "Native VARIANT publication and WAL are still rejected.",
            "Native GEOMETRY payload tag 33 is explicitly unsupported.",
            "The development unshredded fixture includes empty STRUCT; release struct_pack() rejects that expression, so no identical release unshredded producer is claimed.",
            "The known direct qualified-subscript join witness is retained separately from its projected-key equivalent.",
        ],
        "fixtures": [],
        "full_parity": False,
    }
    rust = Engine(args.rust, True)
    with tempfile.TemporaryDirectory(prefix="nested-variant-native-reference-") as directory:
        for target, name in FIXTURES:
            source = ROOT / "test/data/duckdb" / f"nested-variant-{target}"
            manifest = json.loads((source / "manifest.json").read_text())[name]
            path = Path(directory) / f"{target}-{name}.duckdb"
            path.write_bytes(gzip.decompress((source / f"{name}.duckdb.gz").read_bytes()))
            if digest(path) != manifest["sha256"]:
                raise ValueError("Fixture checksum does not match retained producer manifest")
            cpp = Engine(references[target][0], False, serialize_json_rows=False)
            case = {"target": target, "name": name, "fixture_sha256": digest(path),
                    "producer_identity": manifest["reference_identity"], "queries": []}
            report["fixtures"].append(case)
            for label, sql in QUERIES + (UNSHREDDED if name.endswith("unshredded") else SHREDDED):
                query = {"name": label, "sql": sql}
                case["queries"].append(query)
                for engine_name, engine in [("rust", rust), (target, cpp)]:
                    try:
                        query[engine_name] = {"rows": command(engine, path, sql, json_output=True, readonly=True)}
                    except Exception as error:
                        query[engine_name] = {"error": str(error)}
                query["passed"] = "rows" in query["rust"] and query["rust"] == query[target]
            case["checkpoint_unchanged"] = digest(path) == case["fixture_sha256"]
            case["passed"] = case["checkpoint_unchanged"] and all(q["passed"] for q in case["queries"])
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(case["passed"] for case in report["fixtures"])
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"report": str(args.report), "passed": report["passed"],
                      "fixtures": [{"target": case["target"], "name": case["name"],
                                    "matches": sum(q["passed"] for q in case["queries"]),
                                    "total": len(case["queries"]),
                                    "failures": [q["name"] for q in case["queries"] if not q["passed"]]}
                                   for case in report["fixtures"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
