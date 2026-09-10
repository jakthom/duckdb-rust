"""Retain qualified nested-path results and known gaps against both pinned CLIs.

Exact result rows and error categories are compared; error wording, complete
result metadata, file publication, and performance are not claimed here.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


CASES = [
    ("table_list", "SELECT b.xs[1] AS v FROM (VALUES ([11])) b(xs)"),
    ("column_struct_list", "SELECT a.b.xs[1] AS v FROM (SELECT {'b':{'xs':[12]}} a) t"),
    ("table_struct_list", "SELECT t.a.b.xs[1] AS v FROM (SELECT {'b':{'xs':[12]}} a) t"),
    ("deep_dotted_fields", "SELECT t.a.b.c.d AS v FROM (SELECT {'b':{'c':{'d':13}}} a) t"),
    ("table_precedence", "SELECT b.xs[1] AS v FROM (SELECT [11] xs, {'xs':[99]} b) b"),
    ("parenthesized_value", "SELECT (b).xs[1] AS v FROM (SELECT [11] xs, {'xs':[99]} b) b"),
    ("unqualified_struct", "SELECT b.xs[1] AS v FROM (SELECT [11] xs, {'xs':[99]} b) other"),
    ("quoted_dots", 'SELECT "t.q"."s.x"."i.n".xs[1] AS v FROM (SELECT {\'i.n\':{\'xs\':[14]}} AS "s.x") AS "t.q"'),
    ("grouped_path", "SELECT t.a.b[1] AS v FROM (SELECT {'b':[1]} a) t GROUP BY t.a.b[1]"),
    ("grouped_base", "SELECT t.a.b[1] AS v FROM (SELECT {'b':[1]} a) t GROUP BY t.a"),
    ("outer_list", "SELECT (SELECT t.xs[1]) AS v FROM (SELECT [1] xs) t"),
    ("inner_struct_precedence", "SELECT (SELECT t.xs[1] FROM (SELECT {'xs':[2]} t) q) AS v FROM (SELECT [1] xs) t"),
    ("grouped_outer_struct", "SELECT (SELECT t.a.b[1]) AS v FROM (SELECT {'b':[3]} a) t GROUP BY t.a"),
    ("qualify_alias", "SELECT [42] xs,row_number() OVER () n QUALIFY xs[1]=42"),
    ("qualify_alias_scalar_projection", "SELECT n FROM (SELECT [42] xs,row_number() OVER () n QUALIFY xs[1]=42) q"),
    ("ambiguous_table", "SELECT b.xs[1] AS v FROM (SELECT [1] xs) a CROSS JOIN (SELECT [2] xs) b CROSS JOIN (SELECT [3] xs) b"),
    ("ambiguous_struct", "SELECT s.a.xs[1] AS v FROM (SELECT {'a':{'xs':[1]}} s) a CROSS JOIN (SELECT {'a':{'xs':[2]}} s) b"),
    ("ungrouped_path", "SELECT t.a.b[1] AS v,count(*) n FROM (SELECT {'b':[1]} a) t"),
    ("ungrouped_outer", "SELECT (SELECT t.a.b[1]) AS v,count(*) n FROM (SELECT {'b':[1]} a) t"),
    ("missing_field", "SELECT t.s.missing[1] AS v FROM (SELECT {'xs':[1]} s) t"),
    ("scalar_shadows_outer", "SELECT (SELECT t.xs[1] FROM (SELECT 2 t) q) AS v FROM (SELECT [1] xs) t"),
    ("struct_select_alias_rejected", "SELECT {'xs':[42]} z,z.xs[1] AS v"),
    # This development-supported behavior remains an explicit Rust gap.
    ("same_select_list_alias_gap", "SELECT [42] xs,xs[1] AS v"),
]


def outcome(engine, sql):
    try:
        return {"rows": command(engine, Path(":memory:"), sql, json_output=True)}
    except RuntimeError as error:
        message = str(error)
        category = re.search(r"(?:^|\n)([A-Za-z ]+ Error):", message)
        return {"error_category": category.group(1) if category else "unclassified",
                "error": message}


def matches(left, right):
    if "rows" in left and "rows" in right:
        return left["rows"] == right["rows"]
    return (left.get("error_category") not in (None, "unclassified")
            and left.get("error_category") == right.get("error_category"))


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
    for name, sql in CASES:
        case = {"name": name, "sql": sql}
        for target, engine in engines.items():
            case[target] = outcome(engine, sql)
        case["development_passed"] = matches(case["rust"], case["development"])
        case["release_passed"] = matches(case["rust"], case["release"])
        case["release_agrees_with_development"] = matches(case["release"], case["development"])
        report["cases"].append(case)
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(c["development_passed"] for c in report["cases"])
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"report": str(args.report), "source_unchanged": report["source_unchanged"],
                      "development_matches": sum(c["development_passed"] for c in report["cases"]),
                      "release_matches": sum(c["release_passed"] for c in report["cases"]),
                      "total": len(CASES),
                      "development_failures": [c["name"] for c in report["cases"] if not c["development_passed"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
