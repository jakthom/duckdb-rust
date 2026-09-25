"""Recursive SQL and native-file interoperability against independent DuckDB."""
from dataclasses import replace
import hashlib
from pathlib import Path

from sql_reference import verify_corpus

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "test/sql/recursive.test"


def verify(rust, reference, command, directory):
    report = {
        "corpus": str(CORPUS.relative_to(ROOT)),
        "corpus_sha256": hashlib.sha256(CORPUS.read_bytes()).hexdigest(),
        "configurations": [],
        "scope": "Recursive SQL values and error occurrence with default recursive execution, checkpoint/WAL durability, and native files written by each engine. Exact diagnostics and full CTE parity remain open.",
    }
    for durability in ["checkpoint", "wal"]:
        engine = replace(rust, arguments=("--durability", durability))
        path = directory / f"recursive-{durability}.duckdb"
        oracle = directory / f"recursive-{durability}-oracle.duckdb"
        records = verify_corpus(CORPUS, [(engine, path), (reference, oracle)], command)
        for selected, file in [(reference, path), (rust, oracle)]:
            assert command(selected, file, "SELECT x FROM recursive_result ORDER BY x", json_output=True, readonly=True) == [{"x": 1}, {"x": 2}, {"x": 3}]
        command(reference, path, "INSERT INTO recursive_result VALUES(4); CHECKPOINT")
        command(engine, path, "INSERT INTO recursive_result WITH RECURSIVE r(x) AS (SELECT 5 UNION ALL SELECT x+1 FROM r WHERE x<6) SELECT * FROM r; CHECKPOINT")
        assert command(reference, path, "SELECT sum(x) AS total FROM recursive_result", json_output=True, readonly=True) == [{"total": 21}]
        report["configurations"].append({"durability": durability, "records": records, "passed": True})
    return report
