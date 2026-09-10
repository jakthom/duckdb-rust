"""Selected BIGNUM/VARIANT development diagnostics, including unresolved witnesses."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


SETUP = """CREATE TABLE t(id INTEGER PRIMARY KEY,v VARIANT);
INSERT INTO t VALUES(1,(-0.5::DOUBLE)::BIGNUM::VARIANT),(2,0::BIGNUM::VARIANT),
(3,0::INTEGER::VARIANT),(4,1::BIGNUM::VARIANT),(5,1.00::DECIMAL(12,2)::VARIANT),
(6,'340282366920938463463374607431768211456'::BIGNUM::VARIANT),
(7,1.0::DOUBLE::VARIANT),(8,NULL);"""

CASES = [
    ("signed_zero", "", "SELECT (-0.5::DOUBLE)::BIGNUM::VARIANT::BIGNUM::VARCHAR value,((-0.5::DOUBLE)::BIGNUM::VARIANT)=(0::BIGNUM::VARIANT) equal,((-0.5::DOUBLE)::BIGNUM)=(0::BIGNUM) ordinary_equal"),
    ("wide_exact", "", "SELECT variant_typeof('340282366920938463463374607431768211456'::BIGNUM::VARIANT) type,('340282366920938463463374607431768211456'::BIGNUM::VARIANT)::BIGNUM::VARCHAR value"),
    ("number_real_categories", "", "SELECT (1::BIGNUM::VARIANT)=(1::INTEGER::VARIANT) integer_equal,(1::BIGNUM::VARIANT)=(1::DOUBLE::VARIANT) real_equal,('340282366920938463463374607431768211456'::BIGNUM::VARIANT)=(340282366920938463463374607431768211456.0::DOUBLE::VARIANT) wide_real_equal"),
    ("decimal_number_keys", "", "SELECT (100::BIGNUM::VARIANT)=(100.00::DECIMAL(12,2)::VARIANT) equal,(-100::BIGNUM::VARIANT)<(-99.99::DECIMAL(12,2)::VARIANT) less"),
    ("negative_decimal_number_keys", "", "SELECT ('-100'::BIGNUM::VARIANT)<('-99.99'::DECIMAL(12,2)::VARIANT) less,('-340282366920938463463374607431768211456'::BIGNUM::VARIANT)<('-170141183460469231731687303715884105728'::HUGEINT::VARIANT) wide_less"),
    ("object_order", "", "SELECT variant_typeof({'z':1::BIGNUM,'a':2}::VARIANT) value"),
    ("nested_roundtrip", "", "SELECT ({'xs':['340282366920938463463374607431768211456'::BIGNUM,(-0.5::DOUBLE)::BIGNUM]}::VARIANT).xs[2]::BIGNUM::VARCHAR zero,([1::BIGNUM,NULL]::VARIANT)::BIGNUM[]::VARCHAR list"),
    ("union_roundtrip", "", "SELECT (1::BIGNUM::UNION(n BIGNUM)).n::VARCHAR child,(union_value(n:=(-0.5::DOUBLE)::BIGNUM)::VARIANT)::BIGNUM::VARCHAR zero"),
    ("distinct_subquery", SETUP, "SELECT count(*) n FROM(SELECT DISTINCT v FROM t)"),
    ("join", SETUP, "SELECT count(*) n FROM t a JOIN t b ON a.v=b.v"),
    ("window_sort", SETUP, "SELECT id,count(*) OVER(PARTITION BY v) peers FROM t ORDER BY v,id"),
    ("group", SETUP, "SELECT v::VARCHAR value,count(*) n FROM t GROUP BY v ORDER BY v"),
    ("stored_variant_null_gap", SETUP, "SELECT v IS NULL missing,variant_typeof(v) type,v::VARCHAR value FROM t WHERE id=8"),
    ("stored_count_distinct_gap", SETUP, "SELECT count(DISTINCT v) distincts,count(v) present FROM t"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/debug/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve previous evidence; choose a new report path")
    require_checkout(TARGETS["development"].source, "development")
    binary, identity = require_reference(target="development")
    before = source_fingerprint()
    rust = Engine(args.rust, True)
    cpp = Engine(binary, False, serialize_json_rows=False)
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "source_sha256": before,
        "rust_binary_sha256": digest(args.rust),
        "script_sha256": digest(Path(__file__)),
        "reference_identity": identity,
        "scope": "Selected exact BIGNUM payloads, nested casts, keys and relational execution. Includes unresolved stored VARIANT_NULL/count witnesses; no native VARIANT storage, throughput or full parity claim.",
        "cases": [],
        "development_optimizer_diagnostics": [],
        "full_parity": False,
    }
    for name, setup, sql in CASES:
        case = {"name": name, "setup": setup, "sql": sql, "passed": False}
        report["cases"].append(case)
        for label, engine in [("rust", rust), ("development", cpp)]:
            try:
                case[label] = {"rows": command(engine, ":memory:", setup + sql, json_output=True)}
            except Exception as error:
                case[label] = {"error": str(error)}
        case["passed"] = "rows" in case["rust"] and case["rust"] == case["development"]
    # These are C++-only diagnostic modes, not alternate acceptance baselines.
    # The default-development discrepancies above remain failing obligations.
    for setting in ["", "SET disabled_optimizers='statistics_propagation';"]:
        for name in ["stored_variant_null_gap", "stored_count_distinct_gap", "distinct_subquery"]:
            _, setup, sql = next(case for case in CASES if case[0] == name)
            case = {"name": name, "setting": setting, "setup": setup, "sql": sql}
            report["development_optimizer_diagnostics"].append(case)
            try:
                case["rows"] = command(cpp, ":memory:", setting + setup + sql, json_output=True)
            except Exception as error:
                case["error"] = str(error)
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(case["passed"] for case in report["cases"]) and all("rows" in case for case in report["development_optimizer_diagnostics"])
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"report": str(args.report), "passed": report["passed"], "matches": sum(case["passed"] for case in report["cases"]), "total": len(CASES), "mismatches": [case["name"] for case in report["cases"] if not case["passed"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
