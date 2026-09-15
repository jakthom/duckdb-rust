"""Compare named-ENUM checkpoint and WAL interchange with pinned DuckDBs."""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import subprocess
import tempfile

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


def q(schema, name):
    return name if schema == "main" else f"{schema}.{name}"


def lifecycle(schema, create_schema=False):
    prefix = f"CREATE SCHEMA {schema};" if create_schema else ""
    mood, live = q(schema, "mood"), q(schema, "live")
    old, new, rows = q(schema, "old_rows"), q(schema, "new_rows"), q(schema, "live_rows")
    return f"""{prefix}
        CREATE TYPE {mood} AS ENUM ('old'); CREATE TABLE {old}(v {mood}); INSERT INTO {old} VALUES ('old');
        CREATE OR REPLACE TYPE {mood} AS ENUM ('new'); CREATE TABLE {new}(v {mood}); INSERT INTO {new} VALUES ('new');
        DROP TYPE {mood}; CREATE TYPE {live} AS ENUM ('cold','hot'); CREATE TABLE {rows}(v {live}); INSERT INTO {rows} VALUES ('hot');"""


def rows(schema):
    return f"""SELECT source,value FROM (
        SELECT 'old' source,v::VARCHAR value FROM {q(schema, 'old_rows')}
        UNION ALL SELECT 'new',v::VARCHAR FROM {q(schema, 'new_rows')}
        UNION ALL SELECT 'live',v::VARCHAR FROM {q(schema, 'live_rows')}) x ORDER BY source,value"""


EXPECTED = [{"source": "live", "value": "hot"}, {"source": "new", "value": "new"}, {"source": "old", "value": "old"}]


def verify_rows(rust, reference, path, schema):
    actual = [command(engine, path, rows(schema), json_output=True, readonly=True) for engine in (rust, reference)]
    if actual != [EXPECTED, EXPECTED]:
        raise AssertionError(f"named ENUM values differ: {actual!r}")


def expect_failure(engine, path, sql):
    try:
        command(engine, path, sql)
    except RuntimeError:
        return
    raise AssertionError(f"statement unexpectedly succeeded: {sql}")


def mutate(engine, path, schema):
    old, new, live = q(schema, "old_rows"), q(schema, "new_rows"), q(schema, "live")
    continued, mood = q(schema, "continued"), q(schema, "mood")
    command(engine, path, f"INSERT INTO {old} VALUES ('old'); INSERT INTO {new} VALUES ('new'); CREATE TABLE {continued}(v {live}); INSERT INTO {continued} VALUES ('cold'),('hot'); CHECKPOINT")
    expect_failure(engine, path, f"INSERT INTO {old} VALUES ('new')")
    expect_failure(engine, path, f"CREATE TABLE unavailable(v {mood})")
    return command(engine, path, f"SELECT (SELECT count(*) FROM {old}) old_count,(SELECT count(*) FROM {new}) new_count,(SELECT count(*) FROM {continued}) continued_count", json_output=True, readonly=True)


def case(producer_name, producer, consumer, rust, reference, path, schema, wal):
    if schema != "main":
        command(producer, path, f"CREATE SCHEMA {schema}; CHECKPOINT")
    sql = lifecycle(schema)
    if wal and not producer.rust:
        sql = "PRAGMA disable_checkpoint_on_shutdown;" + sql
    command(producer, path, sql + ("" if wal else " CHECKPOINT"))
    verify_rows(rust, reference, path, schema)
    result = mutate(consumer, path, schema)
    if result != [{"old_count": 2, "new_count": 2, "continued_count": 2}]:
        raise AssertionError(f"continued writes differ: {result!r}")
    return {"producer": producer_name, "schema": schema, "wal": wal, "checkpoint_sha256": digest(path), "passed": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    report = args.report.resolve()
    if not report.is_relative_to((ROOT / "target").resolve()):
        raise ValueError("raw named-ENUM reports must be written under target/")
    if report.exists():
        raise FileExistsError("preserve earlier evidence; choose a new report path")
    before = source_fingerprint()
    subprocess.run(["cargo", "build", "--offline", "--release", "--no-default-features", "--bin", "duckdb-rust"], cwd=ROOT, check=True)
    binary = ROOT / "target/release/duckdb-rust"
    rust, rust_wal = Engine(binary, True), Engine(binary, True, ("--durability", "wal"))
    trials = []
    for target, selected in TARGETS.items():
        trial = {"target": target, "cases": []}
        try:
            require_checkout(selected.source, target)
            reference_path, trial["reference_identity"] = require_reference(target=target)
            reference = Engine(reference_path, False, serialize_json_rows=selected.serialize_json_rows)
            with tempfile.TemporaryDirectory(prefix=f"ddb-named-enum-{target}-") as directory:
                directory = Path(directory)
                trial["cases"] = [
                    case("reference", reference, rust, rust, reference, directory / "reference-checkpoint.duckdb", "app", False),
                    case("rust", rust, reference, rust, reference, directory / "rust-checkpoint.duckdb", "app", False),
                    case("reference", reference, rust, rust, reference, directory / "reference-wal.duckdb", "main", True),
                    case("rust", rust_wal, reference, rust, reference, directory / "rust-wal.duckdb", "main", True),
                    case("rust", rust_wal, reference, rust, reference, directory / "rust-qualified-wal.duckdb", "app", True),
                ]
            trial["passed"] = True
        except Exception as error:
            trial.update(passed=False, error=f"{type(error).__name__}: {error}")
        trials.append(trial)
    output = {"recorded_at": datetime.now(timezone.utc).isoformat(), "source_sha256": before, "source_unchanged": before == source_fingerprint(), "targets": trials}
    output["passed"] = output["source_unchanged"] and all(trial.get("passed") for trial in trials)
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps({"passed": output["passed"], "report": str(report)}))
    raise SystemExit(not output["passed"])


if __name__ == "__main__":
    main()
