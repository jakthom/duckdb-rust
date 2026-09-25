"""Compare MAP/sequence literal values, types and errors on both pinned cores.

This retains earlier integer-literal and diagnostic witnesses and reports any
remaining mismatch. MAP values use SQL VARCHAR output to avoid shell-JSON differences.
No performance or new native-publication compatibility claim is made.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path

from qualified_nested_reference import outcome, matches
from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine


CASES = [
    ("map_fields", "SELECT (MAP {'x':1,'y':NULL})::VARCHAR AS v"),
    ("map_expressions", "SELECT (MAP {i+1:i*i,i+10:NULL})::VARCHAR AS v FROM range(3) t(i) ORDER BY i"),
    ("map_string_template", "SELECT (MAP {1:'a','2':'b'})::VARCHAR AS v"),
    ("map_boolean_template", "SELECT (MAP {true:1,2:3})::VARCHAR AS v"),
    ("map_empty", "SELECT (MAP {})::VARCHAR AS v,typeof(MAP {}) AS dtype"),
    ("map_nested", "SELECT (MAP {['x',NULL]:{'n':[1,NULL]},['y']:NULL})::VARCHAR AS v"),
    ("map_exact_names", "SELECT (MAP {'':1,'A':2,'a':3})::VARCHAR AS v"),
    ("map_mixed_types", "SELECT typeof(MAP {1::TINYINT:1.2::DECIMAL(5,1),2::UTINYINT:1.25::DECIMAL(6,2)}) AS dtype"),
    ("map_mixed_children", "SELECT (MAP {1:{'d':1.25::DECIMAL(12,2),'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789','items':[true,2]},2:NULL})::VARCHAR AS v"),
    ("map_lookup", "SELECT (MAP {1:{'d':1.25::DECIMAL(12,2)},2:NULL})[1].d::VARCHAR AS v"),
    ("list_boolean", "SELECT [true,1,NULL]::VARCHAR AS v,[1,false]::VARCHAR AS w"),
    ("list_string_first", "SELECT ['1',NULL,'2',3]::VARCHAR AS v"),
    ("list_strings_first", "SELECT ['1','2',3]::VARCHAR AS v"),
    ("list_integer_first", "SELECT [3,'1','2']::VARCHAR AS v"),
    ("list_null_first_rejected", "SELECT [NULL,'1',2]::VARCHAR AS v"),
    ("list_typed_null_rejected", "SELECT [NULL::VARCHAR,'1',2]::VARCHAR AS v"),
    ("list_typed_varchar_rejected", "SELECT ['1'::VARCHAR,2]::VARCHAR AS v"),
    ("list_column_varchar_rejected", "SELECT [s,2]::VARCHAR AS v FROM (VALUES ('1')) t(s)"),
    ("list_strings_only", "SELECT typeof(['1',NULL,'2']) AS a,typeof([NULL,'1']) AS b"),
    ("list_enum_string", "SELECT typeof(['red'::ENUM('red','blue'),'green']) AS dtype"),
    ("list_typed_widths", "SELECT typeof([1::TINYINT,2::UTINYINT]) AS dtype"),
    ("list_decimal", "SELECT typeof([1::INTEGER,1.25::DECIMAL(5,2)]) AS dtype"),
    ("list_nested_metadata", "SELECT typeof([{'a':NULL::DECIMAL(5,2)},{'b':1::TINYINT}]) AS dtype"),
    ("list_boolean_decimal_rejected", "SELECT [true,1.25::DECIMAL(5,2)]::VARCHAR AS v"),
    ("list_boolean_double_rejected", "SELECT [true,1.0::DOUBLE]::VARCHAR AS v"),
    ("ordinary_concat_not_widened", "SELECT concat([true],[1.0::DOUBLE])::VARCHAR AS v"),
    ("map_null_key", "SELECT MAP {NULL:1}"),
    ("map_duplicate_key", "SELECT MAP {'x':1,'x':2}"),
    ("map_duplicate_converted_key", "SELECT MAP {1:1,'1':2}"),
    ("map_duplicate_boolean_key", "SELECT MAP {true:1,1:2}"),
    ("map_bad_conversion", "SELECT MAP {1:1,'bad':2}"),
    # Keep historical case identities when checking the selected inference repair.
    ("integer_literal_narrowing_gap", "SELECT typeof([1,2::TINYINT]) AS dtype"),
    ("equal_integer_literals_narrowing_gap", "SELECT typeof([1,1,3::TINYINT]) AS dtype"),
    ("typed_first_integer_literal_gap", "SELECT typeof([3::TINYINT,1,2]) AS dtype"),
    ("unequal_integer_literals_normalize", "SELECT typeof([1,2,3::TINYINT]) AS dtype"),
    ("null_first_integer_literal_normalizes", "SELECT typeof([NULL,1,3::TINYINT]) AS dtype"),
    ("string_first_integer_literal_normalizes", "SELECT typeof(['1',1,3::TINYINT]) AS dtype"),
    ("equal_integer_literals_with_null", "SELECT typeof([1,NULL,1,3::TINYINT]) AS dtype"),
    ("parenthesized_integer_literal", "SELECT typeof([(1),2::TINYINT]) AS dtype"),
    ("explicit_integer_cast_not_literal", "SELECT typeof([1::INTEGER,2::TINYINT]) AS dtype"),
    ("integer_expression_not_literal", "SELECT typeof([1+0,2::TINYINT]) AS dtype"),
    ("integer_case_not_literal", "SELECT typeof([CASE WHEN true THEN 1 ELSE 2 END,3::TINYINT]) AS dtype"),
    ("integer_column_not_literal", "SELECT typeof([i,2::TINYINT]) AS dtype FROM (SELECT 1 AS i)t"),
    ("tinyint_lower_boundary", "SELECT typeof([-128,1::TINYINT]) AS dtype,[-128,1::TINYINT]::VARCHAR AS v"),
    ("tinyint_below_boundary", "SELECT typeof([-129,1::TINYINT]) AS dtype"),
    ("tinyint_upper_boundary", "SELECT typeof([127,1::TINYINT]) AS dtype,[127,1::TINYINT]::VARCHAR AS v"),
    ("tinyint_above_boundary", "SELECT typeof([128,1::TINYINT]) AS dtype"),
    ("unsigned_upper_boundary", "SELECT typeof([255,1::UTINYINT]) AS dtype,[255,1::UTINYINT]::VARCHAR AS v"),
    ("unsigned_above_boundary", "SELECT typeof([256,1::UTINYINT]) AS dtype"),
    ("unsigned_negative_literal", "SELECT typeof([-1,1::UTINYINT]) AS dtype"),
    ("ubigint_above_signed_range", "SELECT typeof([9223372036854775808,1::UBIGINT]) AS dtype,[9223372036854775808,1::UBIGINT]::VARCHAR AS v"),
    ("ubigint_upper_boundary", "SELECT typeof([18446744073709551615,1::UBIGINT]) AS dtype,[18446744073709551615,1::UBIGINT]::VARCHAR AS v"),
    ("uhugeint_signed_literal", "SELECT typeof([170141183460469231731687303715884105727,1::UHUGEINT]) AS dtype"),
    ("map_key_narrowing", "SELECT typeof(MAP {1:'a',2::TINYINT:'b'}) AS dtype"),
    ("map_value_narrowing", "SELECT typeof(MAP {'a':1,'b':2::TINYINT}) AS dtype"),
    ("map_unsigned_narrowing", "SELECT typeof(MAP {255:NULL::DECIMAL(6,2),1::UTINYINT:1.25::DECIMAL(6,2)}) AS dtype"),
    ("nested_list_narrowing", "SELECT typeof([[1,2::TINYINT],[3,4::TINYINT]]) AS dtype"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/release/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve prior evidence; choose a new report path")
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
    for name, sql in CASES:
        case = {"name": name, "sql": sql}
        for target, engine in engines.items():
            case[target] = outcome(engine, sql)
        case["development_passed"] = matches(case["rust"], case["development"])
        case["release_passed"] = matches(case["rust"], case["release"])
        case["release_agrees_with_development"] = matches(case["release"], case["development"])
        report["cases"].append(case)
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(case["development_passed"] for case in report["cases"])
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"report": str(args.report), "source_unchanged": report["source_unchanged"],
                      "development_matches": sum(c["development_passed"] for c in report["cases"]),
                      "release_matches": sum(c["release_passed"] for c in report["cases"]),
                      "total": len(CASES),
                      "development_failures": [c["name"] for c in report["cases"] if not c["development_passed"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
