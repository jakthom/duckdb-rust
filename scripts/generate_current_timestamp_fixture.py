"""Generate a pinned C++ checkpoint with retained transaction-time defaults.

The raw parsed-expression oracle proves that the SQL keyword uses DuckDB's
class-4/kind-203 column-reference node, while the callable aliases remain
ordinary FUNCTION nodes.  The checkpoint then preserves both shapes for Rust
native-codec and lifecycle tests.
"""

import argparse
import gzip
import hashlib
import json
from pathlib import Path
import tempfile

from native_nested_expression_reference import compile_helper, oracle
from reference_version import ROOT, TARGETS, require_reference
from verify_reference import Engine, command


SETUP = """
BEGIN TRANSACTION;
CREATE TABLE reference_current_defaults(
    id INTEGER PRIMARY KEY,
    keyword TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    get_call TIMESTAMPTZ DEFAULT get_current_timestamp(),
    now_call TIMESTAMPTZ DEFAULT now(),
    transaction_call TIMESTAMPTZ DEFAULT transaction_timestamp()
);
INSERT INTO reference_current_defaults(id) VALUES (1);
INSERT INTO reference_current_defaults(id) VALUES (2);
COMMIT;
CHECKPOINT;
"""

PARSED_CASES = {
    "keyword": "CURRENT_TIMESTAMP",
    "get_call": "get_current_timestamp()",
    "now_call": "now()",
    "transaction_call": "transaction_timestamp()",
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def parsed_oracle(target, directory):
    helper, identity = compile_helper(target, directory)
    cases = {
        name: oracle(helper, "parse", "v1.5.0", sql)
        for name, sql in PARSED_CASES.items()
    }
    for name, result in cases.items():
        if "error" in result:
            raise RuntimeError(f"{target} failed to parse {name}: {result['error']}")
        expected = "node 0 4 203 " if name == "keyword" else "node 0 9 140 "
        if result["inventory"][0] != expected:
            raise AssertionError(
                f"{target} {name} has {result['inventory'][0]!r}, expected {expected!r}"
            )
        decoded = oracle(helper, "decode", "v1.5.0", result["wire_hex"])
        if decoded != result:
            raise AssertionError(f"{target} {name} is not stable after native decode")
        result["stable_after_decode"] = True
    if cases["keyword"]["inventory"][1] != (
        "column 43555252454e545f54494d455354414d50"
    ):
        raise AssertionError("CURRENT_TIMESTAMP did not retain the exact identifier vector")
    identity = {
        key: identity[key]
        for key in [
            "target",
            "required_version",
            "required_revision",
            "version",
            "sha256",
            "library_sha256",
            "helper_sha256",
            "helper_source_sha256",
        ]
    }
    return cases, identity


def checkpoint_oracle(target, reference, destination):
    engine = Engine(
        reference,
        False,
        serialize_json_rows=TARGETS[target].serialize_json_rows,
    )
    with tempfile.TemporaryDirectory(
        prefix=f"duckdb-current-timestamp-{target}-", dir=ROOT / "target"
    ) as temporary:
        path = Path(temporary) / "current_timestamp.duckdb"
        command(
            engine,
            Path(":memory:"),
            "ATTACH '"
            + str(path)
            + "' AS fixture (STORAGE_VERSION 'v1.5.0'); USE fixture; "
            + SETUP,
        )
        defaults = command(
            engine,
            path,
            "SELECT column_name,column_default FROM duckdb_columns() "
            "WHERE table_name='reference_current_defaults' ORDER BY column_index",
            json_output=True,
            readonly=True,
        )
        equality = command(
            engine,
            path,
            "SELECT count(*) AS rows,"
            "bool_and(keyword=get_call AND keyword=now_call "
            "AND keyword=transaction_call) AS aliases_equal,"
            "count(DISTINCT keyword) AS transaction_values "
            "FROM reference_current_defaults",
            json_output=True,
            readonly=True,
        )
        expected_defaults = [
            {"column_name": "id", "column_default": None},
            {"column_name": "keyword", "column_default": "CURRENT_TIMESTAMP"},
            {
                "column_name": "get_call",
                "column_default": "get_current_timestamp()",
            },
            {"column_name": "now_call", "column_default": "now()"},
            {
                "column_name": "transaction_call",
                "column_default": "transaction_timestamp()",
            },
        ]
        if defaults != expected_defaults:
            raise AssertionError(f"unexpected retained defaults: {defaults!r}")
        expected_equality = [
            {"rows": 2, "aliases_equal": True, "transaction_values": 1}
        ]
        if equality != expected_equality:
            raise AssertionError(f"transaction-time aliases diverged: {equality!r}")
        checkpoint = path.read_bytes()
    compressed = gzip.compress(checkpoint, mtime=0)
    destination.write_bytes(compressed)
    return {
        "storage_version": "v1.5.0",
        "sql": SETUP,
        "column_defaults": defaults,
        "transaction_invariants": equality[0],
        "checkpoint_bytes": len(checkpoint),
        "checkpoint_sha256": digest(checkpoint),
        "fixture_sha256": digest(compressed),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--duckdb", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    reference, binary_identity = require_reference(args.duckdb, target=args.target)
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = output / "current_timestamp.duckdb.gz"
    manifest = output / "manifest.json"
    if fixture.exists() or manifest.exists():
        raise FileExistsError("refusing to overwrite current-timestamp fixture")
    (ROOT / "target").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=f"current-timestamp-expression-{args.target}-", dir=ROOT / "target"
    ) as temporary:
        parsed, helper_identity = parsed_oracle(args.target, temporary)
    checkpoint = checkpoint_oracle(args.target, reference, fixture)
    manifest.write_text(
        json.dumps(
            {
                "writer": binary_identity,
                "raw_expression_writer": helper_identity,
                "parsed_expressions": parsed,
                "checkpoint": checkpoint,
            },
            indent=2,
        )
        + "\n"
    )
    print(f"generated {fixture} from {binary_identity['version']}")


if __name__ == "__main__":
    main()
