"""Positional LIST/ARRAY reshaping against the pinned development engine."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import tempfile

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


QUERIES = [
    "SELECT list_where([1,NULL,3],[true,true,false,true])::VARCHAR value,typeof(list_where([1],[true])) type",
    "SELECT array_where([1,2,3]::INTEGER[3],[true,false,true]::BOOLEAN[3])::VARCHAR value,typeof(array_where([1]::INTEGER[1],[true]::BOOLEAN[1])) type",
    "SELECT list_where(NULL::INTEGER[],[true]) first_null,list_where([1],NULL::BOOLEAN[]) second_null,typeof(list_where([1],NULL)) type",
    "SELECT list_zip([1,2],['a'])::VARCHAR value,typeof(list_zip([1],['a'])) type",
    "SELECT list_zip([1,2],['a'],true)::VARCHAR shortest,list_zip([1,2],NULL,true)::VARCHAR null_shortest,list_zip([1,2],NULL)::VARCHAR longest",
    "SELECT array_zip([1,2]::INTEGER[2],[true]::BOOLEAN[1])::VARCHAR value,typeof(array_zip([1]::INTEGER[1])) type",
    "SELECT list_zip(NULL::BOOLEAN[],[1],NULL::BOOLEAN)::VARCHAR value,typeof(list_zip(NULL)) type",
]
ERRORS = [
    "SELECT list_where([1,2],[true,NULL])",
    "SELECT list_where([1,2],[1,0])",
    "SELECT list_where([1])",
    "SELECT list_zip()",
    "SELECT list_zip(true)",
    "SELECT list_zip([1],2)",
]
SETUP = """CREATE TABLE t(id INTEGER PRIMARY KEY,xs INTEGER[],masked INTEGER[],zipped STRUCT(a INTEGER,b VARCHAR)[]);
INSERT INTO t VALUES
(1,[1,NULL,3],list_where([1,NULL,3],[true,true,false]),list_zip([1,2],['a'])),
(2,[4,5],array_where([4,5]::INTEGER[2],[false,true]::BOOLEAN[2]),array_zip([4,5]::INTEGER[2],['b']));
CHECKPOINT;"""
QUERY = "SELECT id,xs::VARCHAR xs,masked::VARCHAR masked,zipped::VARCHAR zipped FROM t ORDER BY id"
DEFAULT_SETUP = """CREATE TABLE defaults(
id INTEGER PRIMARY KEY,
masked INTEGER[] DEFAULT list_where([10,20],[true,false,true]),
zipped STRUCT(a INTEGER,b VARCHAR)[] DEFAULT list_zip([1,2],['a']));
INSERT INTO defaults(id) VALUES(1);
CHECKPOINT;"""
DEFAULT_QUERY = "SELECT id,masked::VARCHAR masked,zipped::VARCHAR zipped FROM defaults ORDER BY id"


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


def compare_readers(case, path, rust, development, sql):
    actual = command(rust, path, sql, json_output=True, readonly=True)
    expected = command(development, path, sql, json_output=True, readonly=True)
    case["stages"].append({"rust": actual, "development": expected})
    if actual != expected:
        raise AssertionError("positional nested rows differ")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/debug/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve earlier evidence; choose a new report path")
    require_checkout(TARGETS["development"].source, "development")
    binary, identity = require_reference(target="development")
    before = source_fingerprint()
    engines = {
        "rust": Engine(args.rust, True),
        "development": Engine(binary, False, serialize_json_rows=False),
    }
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "source_sha256": before,
        "rust_binary_sha256": digest(args.rust),
        "script_sha256": digest(Path(__file__)),
        "reference_identity": identity,
        "scope": (
            "list_where/array_where and list_zip/array_zip SQL, typed ARRAY-to-LIST, "
            "NULL/error contracts, native materialized values in both producer directions, "
            "and Rust-origin retained defaults consumed by both engines. Upstream-origin "
            "retained defaults remain blocked by qualified built-in lookup in G10."
        ),
        "full_parity": False,
        "sql": [],
        "native": [],
    }
    for sql in QUERIES + ERRORS:
        case = {"sql": sql}
        for name, engine in engines.items():
            case[name] = outcome(engine, sql)
        case["passed"] = matches(case["rust"], case["development"])
        report["sql"].append(case)
    with tempfile.TemporaryDirectory(prefix="nested-positional-reference-") as directory:
        directory = Path(directory)
        for producer, engine in engines.items():
            case = {"producer": producer, "kind": "materialized", "stages": [], "passed": False}
            report["native"].append(case)
            try:
                path = directory / f"materialized-{producer}.duckdb"
                command(engine, path, SETUP)
                case["initial_checkpoint_sha256"] = digest(path)
                compare_readers(case, path, engines["rust"], engines["development"], QUERY)
                command(engines["rust"], path,
                        "UPDATE t SET masked=list_where(xs,[true,false,true]),zipped=list_zip(xs,['rust']) WHERE id=1")
                compare_readers(case, path, engines["rust"], engines["development"], QUERY)
                command(engines["development"], path,
                        "UPDATE t SET masked=list_where(xs,[false,true]),zipped=list_zip(xs,['cpp'],true) WHERE id=2; CHECKPOINT")
                compare_readers(case, path, engines["rust"], engines["development"], QUERY)
                case["passed"] = True
            except Exception as error:
                case["error"] = str(error)
        case = {"producer": "rust", "kind": "retained_defaults", "stages": [], "passed": False}
        report["native"].append(case)
        try:
            path = directory / "retained-defaults.duckdb"
            command(engines["rust"], path, DEFAULT_SETUP)
            compare_readers(case, path, engines["rust"], engines["development"], DEFAULT_QUERY)
            command(engines["development"], path, "INSERT INTO defaults(id) VALUES(2); CHECKPOINT")
            compare_readers(case, path, engines["rust"], engines["development"], DEFAULT_QUERY)
            case["passed"] = True
        except Exception as error:
            case["error"] = str(error)
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(
        case["passed"] for case in report["sql"] + report["native"]
    )
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({
        "report": str(args.report),
        "passed": report["passed"],
        "sql_matches": sum(case["passed"] for case in report["sql"]),
        "sql_total": len(report["sql"]),
        "native": [{"producer": case["producer"], "kind": case["kind"],
                    "passed": case["passed"], "error": case.get("error")}
                   for case in report["native"]],
    }))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
