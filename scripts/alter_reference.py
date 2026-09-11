"""Table alteration assertions and independent native checkpoint/WAL readers."""
from dataclasses import replace
import hashlib
from pathlib import Path

from sql_reference import verify_corpus

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "test/sql/alter.test"
MUTATIONS = ROOT / "test/sql/alter_transactions.sql"
SETUP = "CREATE TABLE t(id INTEGER PRIMARY KEY,v INTEGER); INSERT INTO t VALUES(1,10),(2,NULL),(3,30); CHECKPOINT"
EXPECTED = [{"id": 2, "value": 21}, {"id": 3, "value": 31}, {"id": 4, "value": 41}, {"id": 6, "value": 61}]
# Each native transaction has a name that is already present when DML is logged.
# DuckDB 1.3's own update-then-rename WAL can name a table before its ALTER record;
# that file fails its own reader and is retained as separate failed evidence.
NATIVE_ALTER = """
PRAGMA disable_checkpoint_on_shutdown;
BEGIN;
ALTER TABLE t ADD COLUMN extra SMALLINT DEFAULT 8;
ALTER TABLE t RENAME COLUMN v TO value;
ALTER TABLE t ALTER COLUMN extra SET DEFAULT 9;
ALTER TABLE t ALTER COLUMN extra SET NOT NULL;
ALTER TABLE t RENAME TO renamed;
COMMIT;
INSERT INTO renamed(id,value) VALUES(4,40);
BEGIN;
ALTER TABLE renamed ALTER COLUMN extra DROP NOT NULL;
ALTER TABLE renamed ALTER COLUMN extra DROP DEFAULT;
ALTER TABLE renamed DROP COLUMN value;
COMMIT;
INSERT INTO renamed(id) VALUES(5);
"""


def verify(rust, reference, command, directory):
    report = {
        "mutations_sha256": hashlib.sha256(MUTATIONS.read_bytes()).hexdigest(),
        "configurations": [],
        "scope": "Checkpoint/WAL publication, native ALTER records and continued writes against the selected file oracle. The full SQL corpus uses the separate pinned C++ oracle: DuckDB 1.3 permits DROP NOT NULL on primary keys whereas the pinned source rejects it. Development storage version 999 and complete catalog parity remain open.",
    }
    for durability in ["checkpoint", "wal"]:
        selected = replace(rust, arguments=("--durability", durability))
        mixed = directory / f"alter-mixed-{durability}.duckdb"
        command(selected, mixed, SETUP)
        command(selected, mixed, MUTATIONS.read_text())
        assert command(reference, mixed, "SELECT * FROM renamed ORDER BY id", json_output=True, readonly=True) == EXPECTED
        command(reference, mixed, "ALTER TABLE renamed RENAME TO restored; INSERT INTO restored VALUES(7,70); CHECKPOINT")
        assert command(rust, mixed, "SELECT sum(id) AS ids,sum(value) AS vals FROM restored", json_output=True, readonly=True) == [{"ids": 22, "vals": 224}]
        report["configurations"].append({"durability": durability, "mixed_transaction": True, "passed": True})
    path = directory / "alter-native-wal.duckdb"
    command(reference, path, SETUP)
    command(reference, path, NATIVE_ALTER)
    before = path.read_bytes(), Path(str(path) + ".wal").read_bytes()
    expected = [{"id": 1, "extra": 8}, {"id": 2, "extra": 8}, {"id": 3, "extra": 8}, {"id": 4, "extra": 9}, {"id": 5, "extra": None}]
    for engine in [reference, rust]:
        assert command(engine, path, "SELECT * FROM renamed ORDER BY id", json_output=True, readonly=True) == expected
    assert before == (path.read_bytes(), Path(str(path) + ".wal").read_bytes())
    command(rust, path, "ALTER TABLE renamed RENAME COLUMN extra TO n; INSERT INTO renamed VALUES(6,12); CHECKPOINT")
    assert command(reference, path, "SELECT sum(n) AS total FROM renamed", json_output=True, readonly=True) == [{"total": 45}]
    report["native_wal"] = {"sql": NATIVE_ALTER, "checkpoint_sha256": hashlib.sha256(before[0]).hexdigest(), "wal_sha256": hashlib.sha256(before[1]).hexdigest(), "passed": True}
    return report


def main():
    import argparse
    from datetime import datetime, timezone
    import json
    import tempfile
    from reference_version import TARGETS, require_reference
    from upstream_suite import digest
    from verify_reference import Engine, command

    parser = argparse.ArgumentParser(description="Run unchanged ALTER SQL assertions against an explicit C++ reference and Rust")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--target", choices=TARGETS, default="release")
    parser.add_argument("--duckdb", type=Path)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("preserve prior evidence: choose a new report path")
    reference_path, identity = require_reference(args.duckdb, target=args.target)
    cpp = Engine(reference_path, False, serialize_json_rows=TARGETS[args.target].serialize_json_rows)
    rust = Engine(ROOT / "target/release/duckdb-rust", True)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "reference_identity": identity,
              "cpp_binary_sha256": digest(cpp.binary), "rust_binary_sha256": digest(rust.binary),
              "corpus_sha256": digest(CORPUS), "configurations": [], "full_alter_parity": False,
              "scope": "All local ALTER corpus records and error occurrence against the selected pinned C++ target; unchanged upstream ALTER files and exact diagnostic strings remain separate gates."}
    with tempfile.TemporaryDirectory() as temp:
        directory = Path(temp)
        for durability in ["checkpoint", "wal"]:
            selected = replace(rust, arguments=("--durability", durability))
            try:
                records = verify_corpus(CORPUS, [(selected, directory / f"rust-{durability}.duckdb"), (cpp, directory / f"cpp-{durability}.duckdb")], command)
                report["configurations"].append({"durability": durability, "records": records, "passed": True})
            except Exception as error:
                report["configurations"].append({"durability": durability, "error": str(error), "passed": False})
    report["passed"] = all(c["passed"] for c in report["configurations"])
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "report": str(args.report)}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
