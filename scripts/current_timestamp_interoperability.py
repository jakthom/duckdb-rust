"""Exercise transaction-time defaults in both native-file directions.

Reports are revision-specific evidence and must be written below ``target/``;
only this harness and the separately generated compact fixtures are retained.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import tempfile

from generate_current_timestamp_fixture import SETUP
from reference_version import ROOT, TARGETS, require_reference
from upstream_suite import digest
from verify_reference import Engine, command


EXPECTED_DEFAULTS = [
    {"column_name": "id", "column_default": None},
    {"column_name": "keyword", "column_default": "CURRENT_TIMESTAMP"},
    {"column_name": "get_call", "column_default": "get_current_timestamp()"},
    {"column_name": "now_call", "column_default": "now()"},
    {
        "column_name": "transaction_call",
        "column_default": "transaction_timestamp()",
    },
]


def file_evidence(path):
    return {
        "bytes": path.stat().st_size,
        "sha256": digest(path),
    }


def defaults(engine, path):
    return command(
        engine,
        path,
        "SELECT column_name,column_default FROM duckdb_columns() "
        "WHERE table_name='reference_current_defaults' ORDER BY column_index",
        json_output=True,
        readonly=True,
    )


def invariants(engine, path, first, last):
    rows = command(
        engine,
        path,
        "SELECT id,keyword=get_call AS get_equal,keyword=now_call AS now_equal,"
        "keyword=transaction_call AS transaction_equal,keyword::VARCHAR AS keyword "
        "FROM reference_current_defaults "
        f"WHERE id BETWEEN {first} AND {last} ORDER BY id",
        json_output=True,
        readonly=True,
    )
    return {
        "rows": len(rows),
        "aliases_equal": all(
            row["get_equal"] and row["now_equal"] and row["transaction_equal"]
            for row in rows
        ),
        "transaction_values": len({row["keyword"] for row in rows}),
    }


def assert_batch(engine, path, first, last):
    observed = invariants(engine, path, first, last)
    expected = {
        "rows": last - first + 1,
        "aliases_equal": True,
        "transaction_values": 1,
    }
    if observed != expected:
        raise AssertionError(f"transaction batch {first}..{last}: {observed!r}")
    return observed


def insert_batch(engine, path, first, last):
    values = "; ".join(
        f"INSERT INTO reference_current_defaults(id) VALUES ({row})"
        for row in range(first, last + 1)
    )
    command(engine, path, f"BEGIN TRANSACTION; {values}; COMMIT; CHECKPOINT")


def create_cpp_origin(cpp, target, path):
    command(
        cpp,
        Path(":memory:"),
        "ATTACH '"
        + str(path)
        + "' AS fixture (STORAGE_VERSION 'v1.5.0'); USE fixture; "
        + SETUP,
    )


def cpp_origin(rust, cpp, target, directory):
    path = directory / f"cpp-{target}.duckdb"
    create_cpp_origin(cpp, target, path)
    produced = file_evidence(path)
    first = assert_batch(rust, path, 1, 2)
    insert_batch(rust, path, 3, 4)
    rust_written = file_evidence(path)
    if defaults(cpp, path) != EXPECTED_DEFAULTS:
        raise AssertionError("Rust changed C++-origin default metadata")
    second = assert_batch(cpp, path, 3, 4)
    insert_batch(cpp, path, 5, 6)
    third = assert_batch(rust, path, 5, 6)
    return {
        "producer": "pinned-cpp",
        "produced": produced,
        "after_rust": rust_written,
        "after_cpp": file_evidence(path),
        "batches": [first, second, third],
        "passed": True,
    }


def rust_origin(rust, cpp, target, directory):
    path = directory / f"rust-for-{target}.duckdb"
    command(rust, path, SETUP)
    produced = file_evidence(path)
    if defaults(cpp, path) != EXPECTED_DEFAULTS:
        raise AssertionError("C++ changed Rust-origin default metadata")
    first = assert_batch(cpp, path, 1, 2)
    insert_batch(cpp, path, 3, 4)
    cpp_written = file_evidence(path)
    second = assert_batch(rust, path, 3, 4)
    insert_batch(rust, path, 5, 6)
    third = assert_batch(cpp, path, 5, 6)
    return {
        "producer": "rust",
        "produced": produced,
        "after_cpp": cpp_written,
        "after_rust": file_evidence(path),
        "batches": [first, second, third],
        "passed": True,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--duckdb", type=Path)
    parser.add_argument(
        "--rust", type=Path, default=ROOT / "target/release/duckdb-rust"
    )
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("preserve prior evidence: choose a new report path")
    reference, identity = require_reference(args.duckdb, target=args.target)
    rust = Engine(args.rust.resolve(strict=True), True)
    cpp = Engine(
        reference,
        False,
        serialize_json_rows=TARGETS[args.target].serialize_json_rows,
    )
    with tempfile.TemporaryDirectory(
        prefix=f"duckdb-current-interoperability-{args.target}-", dir=ROOT / "target"
    ) as temporary:
        directory = Path(temporary)
        cases = {
            "cpp_origin": cpp_origin(rust, cpp, args.target, directory),
            "rust_origin": rust_origin(rust, cpp, args.target, directory),
        }
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "reference_identity": identity,
        "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "rust_binary_sha256": digest(rust.binary),
        "cases": cases,
        "passed": all(case["passed"] for case in cases.values()),
        "scope": (
            "Bidirectional v1.5.0 checkpoint acceptance for the native "
            "CURRENT_TIMESTAMP value node and its callable FUNCTION aliases."
        ),
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "report": str(args.report)}))


if __name__ == "__main__":
    main()
