"""Compare nested sort/grade SQL and native checkpoints with both pinned cores.

Development semantics are the acceptance oracle. Release outcomes stay explicit
because ordering, named binding, and error classification changed between the pins.
The materialized campaign uses storage version 68 so every selected reader can
reopen every producer's disposable checkpoint. Retained-default limits are
reported separately.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import tempfile

from native_version_reference import header
from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


ALIASES = (
    ("list_sort", "sort", False),
    ("array_sort", "sort", True),
    ("list_grade_up", "grade", False),
    ("array_grade_up", "grade", True),
    ("grade_up", "grade", False),
    ("list_reverse_sort", "reverse", False),
    ("array_reverse_sort", "reverse", True),
)


def alias_input(array):
    return "[3,NULL,1,3]::INTEGER[4]" if array else "[3,NULL,1,3]"


def explicit_call(name, operation, value):
    if operation == "reverse":
        return f"{name}({value},'NULLS FIRST')"
    return f"{name}({value},'DESC','NULLS FIRST')"


def named_call(name, operation, value, reordered=False):
    if operation == "reverse":
        arguments = (f"null_order := 'NULLS FIRST', list := {value}" if reordered else
                     f"list := {value}, null_order := 'NULLS FIRST'")
    else:
        arguments = (f"null_order := 'NULLS FIRST', list := {value}, sort_order := 'DESC'"
                     if reordered else
                     f"list := {value}, sort_order := 'DESC', null_order := 'NULLS FIRST'")
    return f"{name}({arguments})"


def sql_cases():
    cases = []
    for name, operation, array in ALIASES:
        value = alias_input(array)
        expected_type = "BIGINT[]" if operation == "grade" else "INTEGER[]"
        explicit = explicit_call(name, operation, value)
        named = named_call(name, operation, value)
        reordered = named_call(name, operation, value, reordered=True)
        cases.extend([
            {
                "name": f"{name}_explicit",
                "family": "alias",
                "function": name,
                "sql": (f"SELECT {explicit}::VARCHAR AS result_value,"
                        f"typeof({explicit}) AS result_type,"
                        f"typeof({name}({value}))='{expected_type}' AS type_is_exact"),
            },
            {
                "name": f"{name}_named",
                "family": "named",
                "function": name,
                "sql": f"SELECT {named}::VARCHAR AS result_value,typeof({named}) AS result_type",
            },
            {
                "name": f"{name}_named_reordered",
                "family": "named_reordered",
                "function": name,
                "sql": (f"SELECT {reordered}::VARCHAR AS result_value,"
                        f"typeof({reordered}) AS result_type"),
            },
            {
                "name": f"{name}_settings",
                "family": "settings",
                "function": name,
                "sql": ("SET default_order='DESC'; SET default_null_order='NULLS_FIRST'; "
                        f"SELECT {name}({value})::VARCHAR AS result_value"),
            },
            {
                "name": f"{name}_null_list",
                "family": "null",
                "function": name,
                "sql": (f"SELECT {name}(NULL::INTEGER[]) IS NULL AS result_is_null,"
                        f"typeof({name}(NULL::INTEGER[])) AS result_type"),
            },
            {
                "name": f"{name}_null_option",
                "family": "null",
                "function": name,
                "sql": (f"SELECT {name}({value},NULL) IS NULL AS result_is_null,"
                        f"typeof({name}({value},NULL)) AS result_type"),
            },
            {
                "name": f"{name}_scalar_error",
                "family": "error",
                "function": name,
                "expected_error": True,
                "sql": f"SELECT {name}(42)",
            },
            {
                "name": f"{name}_arity_error",
                "family": "error",
                "function": name,
                "expected_error": True,
                "sql": (f"SELECT {name}({value},'DESC','NULLS LAST','extra')"
                        if operation != "reverse" else
                        f"SELECT {name}({value},'NULLS LAST','extra')"),
            },
        ])
    cases.extend([
        {
            "name": "sort_order_default_spelling",
            "family": "settings",
            "sql": ("SET default_order='DESC'; SET default_null_order='NULLS_FIRST'; "
                    "SELECT list_sort([3,NULL,1],'ORDER_DEFAULT','ORDER_DEFAULT')::VARCHAR "
                    "AS sorted_values,list_grade_up([3,NULL,1],'DEFAULT','DEFAULT')::VARCHAR "
                    "AS grade_values"),
        },
        {
            "name": "invalid_sort_order",
            "family": "error",
            "expected_error": True,
            "sql": "SELECT list_sort([3,1],'sideways')",
        },
        {
            "name": "invalid_null_order",
            "family": "error",
            "expected_error": True,
            "sql": "SELECT list_grade_up([3,1],'ASC','FIRST')",
        },
        {
            "name": "nonconstant_sort_order",
            "family": "error",
            "expected_error": True,
            "sql": "SELECT list_sort([3,1],ordering) FROM (VALUES ('ASC')) t(ordering)",
        },
        {
            "name": "nonconstant_null_order",
            "family": "error",
            "expected_error": True,
            "sql": ("SELECT list_reverse_sort([3,1],ordering) "
                    "FROM (VALUES ('NULLS LAST')) t(ordering)"),
        },
        {
            "name": "unknown_named_argument",
            "family": "error",
            "expected_error": True,
            "sql": "SELECT list_sort(values := [3,1])",
        },
    ])
    type_sql = [
        ("typed_empty", "SELECT list_sort([]::SMALLINT[])::VARCHAR AS result_value,typeof(list_sort([]::SMALLINT[])) AS result_type"),
        ("booleans", "SELECT list_sort([true,false,NULL])::VARCHAR AS result_value,typeof(list_sort([true,false,NULL])) AS result_type"),
        ("signed_unsigned", "SELECT list_sort([255::UTINYINT,0,NULL,7])::VARCHAR AS unsigned_values,list_grade_up([2::HUGEINT,-3,NULL,2])::VARCHAR AS huge_grade"),
        ("bignum", "SELECT list_sort(['340282366920938463463374607431768211456'::BIGNUM,-1::BIGNUM,NULL])::VARCHAR AS result_value"),
        ("decimal", "SELECT list_sort([3.20::DECIMAL(8,2),-1.25,NULL,3.20])::VARCHAR AS result_value,list_grade_up([3.20::DECIMAL(8,2),-1.25,NULL,3.20])::VARCHAR AS grade_value"),
        ("unicode_varchar", "SELECT list_sort(['z','á','A','ß',NULL])::VARCHAR AS result_value"),
        ("bit", "SELECT list_sort(['101'::BIT,'0'::BIT,'11'::BIT,NULL])::VARCHAR AS result_value"),
        ("uuid", "SELECT list_sort(['ffffffff-ffff-ffff-ffff-ffffffffffff'::UUID,'00000000-0000-0000-0000-000000000001'::UUID,NULL])::VARCHAR AS result_value"),
        ("temporal", "SELECT list_sort([DATE '2025-01-01',DATE '1999-01-01',NULL])::VARCHAR AS dates,list_sort([TIMESTAMP_NS '2000-01-01 00:00:00.123456789',TIMESTAMP_NS '1999-12-31 23:59:59.999999999',NULL])::VARCHAR AS timestamps"),
        ("interval", "SELECT list_sort([INTERVAL '1 month',INTERVAL '30 days',INTERVAL '-1 microsecond',NULL])::VARCHAR AS result_value,list_grade_up([INTERVAL '1 month',INTERVAL '30 days',INTERVAL '-1 microsecond',NULL])::VARCHAR AS grade_value"),
        ("nan_infinity_signed_zero", "SELECT list_sort(['NaN'::DOUBLE,0.0::DOUBLE,-0.0::DOUBLE,'-Infinity'::DOUBLE,1.0::DOUBLE,'Infinity'::DOUBLE,NULL])::VARCHAR AS result_value,list_grade_up(['NaN'::DOUBLE,0.0::DOUBLE,-0.0::DOUBLE,'-Infinity'::DOUBLE,1.0::DOUBLE,'Infinity'::DOUBLE,NULL])::VARCHAR AS grade_value"),
        ("nested_lists", "SELECT list_sort([[2,NULL],[1,9],[1,NULL],NULL])::VARCHAR AS result_value,list_grade_up([[2,NULL],[1,9],[1,NULL],NULL])::VARCHAR AS grade_value"),
        ("nested_structs", "SELECT list_sort([{'a':2,'b':NULL},{'a':1,'b':9},{'a':1,'b':NULL},NULL])::VARCHAR AS result_value,list_grade_up([{'a':2,'b':NULL},{'a':1,'b':9},{'a':1,'b':NULL},NULL])::VARCHAR AS grade_value"),
        ("nested_maps", "SELECT list_sort([MAP {'x':2},MAP {'x':1},NULL])::VARCHAR AS result_value,list_grade_up([MAP {'x':2},MAP {'x':1},NULL])::VARCHAR AS grade_value"),
        ("stable_grade", "SELECT list_grade_up([2,1,2,1,NULL,NULL],'ASC','NULLS LAST')::VARCHAR AS ascending,list_grade_up([2,1,2,1,NULL,NULL],'DESC','NULLS FIRST')::VARCHAR AS descending"),
        ("stable_nan_grade", "SELECT list_grade_up(['NaN'::DOUBLE,1.0,'NaN'::DOUBLE,1.0,NULL],'ASC','NULLS LAST')::VARCHAR AS result_value"),
    ]
    cases.extend({"name": name, "family": "type", "sql": sql} for name, sql in type_sql)
    return cases


def outcome(engine, sql, path=Path(":memory:"), readonly=False):
    try:
        return {"rows": command(engine, path, sql, json_output=True, readonly=readonly)}
    except RuntimeError as error:
        message = str(error)
        category = re.search(r"(?:^|\n)([A-Za-z ]+ Error):", message)
        return {
            "error_category": category.group(1) if category else "unclassified",
            "error": message,
        }


def matches(left, right, expected_error=False):
    if expected_error:
        return (left.get("error_category") not in (None, "unclassified") and
                left.get("error_category") == right.get("error_category"))
    return "rows" in left and left == right


def values_sql(verb, row_id, values):
    expressions = {
        "source": values,
        "list_sorted": f"list_sort({values},'ASC','NULLS LAST')",
        "array_sorted": f"array_sort({values}::INTEGER[4],'DESC','NULLS FIRST')",
        "list_grade": f"list_grade_up({values},'ASC','NULLS LAST')",
        "array_grade": f"array_grade_up({values}::INTEGER[4],'DESC','NULLS FIRST')",
        "grade": f"grade_up({values},'ASC','NULLS FIRST')",
        "list_reversed": f"list_reverse_sort({values},'NULLS LAST')",
        "array_reversed": f"array_reverse_sort({values}::INTEGER[4],'NULLS FIRST')",
    }
    if verb == "insert":
        columns = ",".join(["id", *expressions])
        selected = ",".join([str(row_id), *expressions.values()])
        return f"INSERT INTO materialized({columns}) SELECT {selected}"
    assignments = ",".join(f"{column}={expression}" for column, expression in expressions.items())
    return f"UPDATE materialized SET {assignments} WHERE id={row_id}"


DEFAULT_COLUMNS = """list_sorted INTEGER[] DEFAULT list_sort([3,NULL,1],'ASC','NULLS LAST'),
array_sorted INTEGER[] DEFAULT array_sort([3,NULL,1]::INTEGER[3],'DESC','NULLS FIRST'),
list_grade BIGINT[] DEFAULT list_grade_up([3,NULL,1],'ASC','NULLS LAST'),
array_grade BIGINT[] DEFAULT array_grade_up([3,NULL,1]::INTEGER[3],'DESC','NULLS FIRST'),
grade BIGINT[] DEFAULT grade_up([3,NULL,1],'ASC','NULLS FIRST'),
list_reversed INTEGER[] DEFAULT list_reverse_sort([3,NULL,1],'NULLS LAST'),
array_reversed INTEGER[] DEFAULT array_reverse_sort([3,NULL,1]::INTEGER[3],'NULLS FIRST')"""

MATERIALIZED_COLUMNS = """source INTEGER[],list_sorted INTEGER[],array_sorted INTEGER[],
list_grade BIGINT[],array_grade BIGINT[],grade BIGINT[],list_reversed INTEGER[],
array_reversed INTEGER[]"""

NATIVE_QUERY = """SELECT id,source::VARCHAR AS source,list_sorted::VARCHAR AS list_sorted,
array_sorted::VARCHAR AS array_sorted,list_grade::VARCHAR AS list_grade,
array_grade::VARCHAR AS array_grade,grade::VARCHAR AS grade,
list_reversed::VARCHAR AS list_reversed,array_reversed::VARCHAR AS array_reversed,
list_sorted=list_sort(source,'ASC','NULLS LAST') AS list_sorted_valid,
array_sorted=array_sort(source::INTEGER[4],'DESC','NULLS FIRST') AS array_sorted_valid,
list_grade=list_grade_up(source,'ASC','NULLS LAST') AS list_grade_valid,
array_grade=array_grade_up(source::INTEGER[4],'DESC','NULLS FIRST') AS array_grade_valid,
grade=grade_up(source,'ASC','NULLS FIRST') AS grade_valid,
list_reversed=list_reverse_sort(source,'NULLS LAST') AS list_reversed_valid,
array_reversed=array_reverse_sort(source::INTEGER[4],'NULLS FIRST') AS array_reversed_valid
FROM materialized ORDER BY id"""

DEFAULT_QUERY = """SELECT id,list_sorted::VARCHAR AS list_sorted,array_sorted::VARCHAR AS array_sorted,
list_grade::VARCHAR AS list_grade,array_grade::VARCHAR AS array_grade,grade::VARCHAR AS grade,
list_reversed::VARCHAR AS list_reversed,array_reversed::VARCHAR AS array_reversed,
list_sorted=list_sort([3,NULL,1],'ASC','NULLS LAST') AS list_sorted_valid,
array_sorted=array_sort([3,NULL,1]::INTEGER[3],'DESC','NULLS FIRST') AS array_sorted_valid,
list_grade=list_grade_up([3,NULL,1],'ASC','NULLS LAST') AS list_grade_valid,
array_grade=array_grade_up([3,NULL,1]::INTEGER[3],'DESC','NULLS FIRST') AS array_grade_valid,
grade=grade_up([3,NULL,1],'ASC','NULLS FIRST') AS grade_valid,
list_reversed=(CASE WHEN id=1 THEN [3,1,NULL] ELSE [1,3,NULL] END) AS list_reversed_valid,
array_reversed=(CASE WHEN id=1 THEN [NULL,3,1] ELSE [NULL,1,3] END) AS array_reversed_valid
FROM retained_defaults ORDER BY id"""


def materialized_setup_sql():
    return ("SET default_order='ASC'; SET default_null_order='NULLS_LAST'; "
            f"CREATE TABLE materialized(id INTEGER PRIMARY KEY,{MATERIALIZED_COLUMNS}); "
            f"{values_sql('insert', 1, '[3,NULL,1,3]')}; CHECKPOINT")


def defaults_setup_sql():
    return ("SET default_order='ASC'; SET default_null_order='NULLS_LAST'; "
            f"CREATE TABLE retained_defaults(id INTEGER PRIMARY KEY,{DEFAULT_COLUMNS}); "
            "INSERT INTO retained_defaults(id) VALUES(1); CHECKPOINT")


def materialized_mutation_sql(writer):
    if writer == "rust":
        statements = [
            values_sql("update", 1, "[4,NULL,0,4]"),
            values_sql("insert", 2, "[2,2,NULL,-1]"),
        ]
    elif writer == "development":
        statements = [
            values_sql("update", 2, "[9,NULL,8,9]"),
        ]
    else:
        statements = [
            values_sql("update", 1, "[-2,NULL,-2,7]"),
        ]
    return ("SET default_order='ASC'; SET default_null_order='NULLS_LAST'; " +
            "; ".join(statements) + "; CHECKPOINT")


def default_mutation_sql(row_id):
    return ("SET default_order='DESC'; SET default_null_order='NULLS_FIRST'; "
            f"INSERT INTO retained_defaults(id) VALUES({row_id}); CHECKPOINT")


def rows_are_valid(rows):
    return bool(rows) and all(
        value is True
        for row in rows
        for key, value in row.items()
        if key.endswith("_valid")
    )


def create_native(producer, engine, rust_binary, path, sql):
    if producer == "rust":
        writer = Engine(rust_binary, True, ("--storage-version", "68"))
        command(writer, path, sql)
        return
    escaped = str(path).replace("'", "''")
    command(engine, Path(":memory:"),
            f"ATTACH '{escaped}' AS db (STORAGE_VERSION 'v1.5.0'); USE db; {sql}")


def run_native(engines, rust_binary, directory):
    cases = []
    for producer in ("rust", "development", "release"):
        path = directory / f"nested-sort-materialized-{producer}.duckdb"
        case = {"producer": producer, "kind": "materialized", "storage_version": 68,
                "stages": [], "passed": False}
        cases.append(case)
        try:
            create_native(producer, engines[producer], rust_binary, path,
                          materialized_setup_sql())
            initial = header(path)
            case["initial_header"] = initial
            stages = [("created", None)] + [
                (f"{writer}_mutation", writer)
                for writer in ("rust", "development", "release")
            ]
            for stage_name, writer in stages:
                if writer is not None:
                    command(engines[writer], path, materialized_mutation_sql(writer))
                current = header(path)
                readers = {
                    name: outcome(engine, NATIVE_QUERY, path, readonly=True)
                    for name, engine in engines.items()
                }
                baseline = readers["development"]
                rows_match = all(reader == baseline for reader in readers.values())
                stage = {
                    "name": stage_name,
                    "writer": writer,
                    "header": current,
                    "readers": readers,
                    "rows_match": rows_match,
                    "version_preserved": current["effective"] == 68,
                    "identity_preserved": current["identifier"] == initial["identifier"],
                }
                stage["passed"] = (stage["rows_match"] and stage["version_preserved"] and
                                   stage["identity_preserved"] and
                                   all("rows" in reader and rows_are_valid(reader["rows"])
                                       for reader in readers.values()))
                case["stages"].append(stage)
            case["passed"] = all(stage["passed"] for stage in case["stages"])
        except Exception as error:
            case["error"] = str(error)

    path = directory / "nested-sort-defaults-rust.duckdb"
    case = {"producer": "rust", "kind": "retained_defaults", "storage_version": 68,
            "stages": [], "passed": False}
    cases.append(case)
    try:
        create_native("rust", engines["rust"], rust_binary, path, defaults_setup_sql())
        initial = header(path)
        case["initial_header"] = initial
        for row_id, (stage_name, writer) in enumerate([
            ("created", None),
            ("rust_default_insert", "rust"),
            ("development_default_insert", "development"),
            ("release_default_insert", "release"),
        ], 1):
            if writer is not None:
                command(engines[writer], path, default_mutation_sql(row_id))
            current = header(path)
            readers = {
                name: outcome(engine, DEFAULT_QUERY, path, readonly=True)
                for name, engine in engines.items()
            }
            baseline = readers["development"]
            stage = {
                "name": stage_name,
                "writer": writer,
                "header": current,
                "readers": readers,
                "rows_match": all(reader == baseline for reader in readers.values()),
                "version_preserved": current["effective"] == 68,
                "identity_preserved": current["identifier"] == initial["identifier"],
            }
            stage["passed"] = (stage["rows_match"] and stage["version_preserved"] and
                               stage["identity_preserved"] and
                               all("rows" in reader and rows_are_valid(reader["rows"])
                                   for reader in readers.values()))
            case["stages"].append(stage)
        case["passed"] = all(stage["passed"] for stage in case["stages"])
    except Exception as error:
        case["error"] = str(error)

    for producer in ("development", "release"):
        path = directory / f"nested-sort-defaults-{producer}.duckdb"
        case = {"producer": producer, "kind": "retained_defaults", "storage_version": 68,
                "supported_by_rust": False, "stages": [], "passed": False}
        cases.append(case)
        try:
            create_native(producer, engines[producer], rust_binary, path, defaults_setup_sql())
            initial = header(path)
            case["initial_header"] = initial
            for row_id, (stage_name, writer) in enumerate([
                ("created", None),
                ("development_default_insert", "development"),
                ("release_default_insert", "release"),
            ], 1):
                if writer is not None:
                    command(engines[writer], path, default_mutation_sql(row_id))
                current = header(path)
                readers = {
                    name: outcome(engine, DEFAULT_QUERY, path, readonly=True)
                    for name, engine in engines.items()
                }
                rust_limit = readers["rust"].get("error", "")
                references_match = (readers["development"] == readers["release"] and
                                    "rows" in readers["development"] and
                                    rows_are_valid(readers["development"]["rows"]))
                stage = {
                    "name": stage_name,
                    "writer": writer,
                    "header": current,
                    "readers": readers,
                    "references_match": references_match,
                    "rust_limitation_observed": "native literal logical type 4" in rust_limit,
                    "version_preserved": current["effective"] == 68,
                    "identity_preserved": current["identifier"] == initial["identifier"],
                }
                stage["passed"] = (stage["references_match"] and
                                   stage["rust_limitation_observed"] and
                                   stage["version_preserved"] and stage["identity_preserved"])
                case["stages"].append(stage)
            case["limitation"] = (
                "Rust cannot open an upstream-created catalog containing retained LIST "
                "literals: Not implemented: native literal logical type 4"
            )
            case["passed"] = all(stage["passed"] for stage in case["stages"])
        except Exception as error:
            case["error"] = str(error)
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/release/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    target = (ROOT / "target").resolve()
    target.mkdir(parents=True, exist_ok=True)
    report_path = args.report.resolve()
    if target not in report_path.parents:
        raise ValueError("Reference reports and disposable evidence must stay under target/")
    if report_path.exists():
        raise FileExistsError("Preserve earlier evidence; choose a new report path")
    report_path.parent.mkdir(parents=True, exist_ok=True)
    rust_binary = args.rust.resolve(strict=True)
    before = source_fingerprint()
    engines = {"rust": Engine(rust_binary, True)}
    references = {}
    for reference_name in ("development", "release"):
        require_checkout(TARGETS[reference_name].source, reference_name)
        binary, identity = require_reference(target=reference_name)
        references[reference_name] = identity
        # Every selected value is normalized in SQL; direct JSON avoids wrapping
        # multi-statement setting cases in an invalid release subquery.
        engines[reference_name] = Engine(binary, False, serialize_json_rows=False)
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "source_sha256": before,
        "rust_binary_sha256": digest(rust_binary),
        "script_sha256": digest(Path(__file__)),
        "references": references,
        "scope": __doc__,
        "functions": [name for name, _, _ in ALIASES],
        "native_reader_mode": "Every stage is reopened by a new read-only CLI process.",
        "native_storage_version": 68,
        "sql": [],
        "native": [],
        "full_parity": False,
    }
    for definition in sql_cases():
        case = dict(definition)
        for name, engine in engines.items():
            case[name] = outcome(engine, case["sql"])
        expected_error = case.get("expected_error", False)
        case["development_passed"] = matches(
            case["rust"], case["development"], expected_error
        )
        case["release_passed"] = matches(case["rust"], case["release"], expected_error)
        case["references_agree"] = matches(
            case["development"], case["release"], expected_error
        )
        report["sql"].append(case)
    with tempfile.TemporaryDirectory(prefix="nested-sort-reference-", dir=target) as directory:
        report["native"] = run_native(engines, rust_binary, Path(directory))
    report["source_unchanged"] = before == source_fingerprint()
    report["development_sql_passed"] = all(case["development_passed"] for case in report["sql"])
    report["release_sql_full_match"] = all(case["release_passed"] for case in report["sql"])
    report["release_differences"] = [case["name"] for case in report["sql"]
                                     if not case["release_passed"]]
    report["native_supported_passed"] = all(
        case["passed"] for case in report["native"]
        if case.get("supported_by_rust", True)
    )
    report["native_limits_observed"] = all(
        case["passed"] for case in report["native"]
        if not case.get("supported_by_rust", True)
    )
    report["native_passed"] = (report["native_supported_passed"] and
                               report["native_limits_observed"])
    report["limitations"] = [
        "Rust cannot open reference-origin retained defaults containing native LIST literals (native logical type 4); reference-only mutations and reopens are checked instead.",
        "The release pin differs from development for the SQL cases named in release_differences; development is the semantic acceptance oracle.",
        "Rust imposes a 16,777,216-child allocation bound that the pinned implementations do not expose as the same fixed contract.",
        "This packet does not claim WAL, spill/performance, every nested child type, or full compatibility parity.",
    ]
    report["passed"] = (report["source_unchanged"] and report["development_sql_passed"] and
                        report["native_passed"])
    report_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    summary = {
        "report": str(report_path),
        "passed": report["passed"],
        "sql_total": len(report["sql"]),
        "development_matches": sum(case["development_passed"] for case in report["sql"]),
        "release_matches": sum(case["release_passed"] for case in report["sql"]),
        "release_differences": report["release_differences"],
        "native": [{
            "producer": case["producer"],
            "kind": case["kind"],
            "supported_by_rust": case.get("supported_by_rust", True),
            "passed": case["passed"],
            "error": case.get("error"),
            "stages": [(stage["name"], stage["passed"]) for stage in case["stages"]],
        } for case in report["native"]],
    }
    print(json.dumps(summary, ensure_ascii=False))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
