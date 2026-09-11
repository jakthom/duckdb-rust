"""Run the shared subquery corpus through independent native and Rust engines."""
from dataclasses import replace
import hashlib
from pathlib import Path
from sql_reference import verify_corpus

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "test/sql/subqueries.test"


def verify(rust, reference, command, directory):
    report = {
        "corpus": str(CORPUS.relative_to(ROOT)),
        "corpus_sha256": hashlib.sha256(CORPUS.read_bytes()).hexdigest(),
        "configurations": [],
        "scope": "Shared SQL corpus with independent DuckDB values/errors, both subquery adapters, checkpoint/WAL durability, and continued cross-engine reads/writes. No decorrelation or subquery-performance claim.",
    }
    for adapter in ["streaming", "materializing"]:
        for durability in ["checkpoint", "wal"]:
            engine = replace(rust, arguments=("--subqueries", adapter, "--durability", durability))
            path = directory / f"subquery-{adapter}-{durability}.duckdb"
            oracle = directory / f"subquery-{adapter}-{durability}-oracle.duckdb"
            configuration = {"adapter": adapter, "durability": durability, "records": []}
            configuration["records"] = verify_corpus(CORPUS, [(engine, path), (reference, oracle)], command)
            expected = [{"i": 1, "v": 31}, {"i": 2, "v": 32}, {"i": 4, "v": 63}]
            for selected, file in [(reference, path), (rust, oracle)]:
                assert command(selected, file, "SELECT * FROM target ORDER BY i", json_output=True, readonly=True) == expected
            command(reference, path, "UPDATE target SET v=(SELECT max(v) FROM target b)+i; CHECKPOINT")
            command(engine, path, "UPDATE target SET v=(SELECT min(v) FROM target b)-i; CHECKPOINT")
            assert command(reference, path, "SELECT * FROM target ORDER BY i", json_output=True, readonly=True) == [{"i": 1, "v": 63}, {"i": 2, "v": 62}, {"i": 4, "v": 60}]
            configuration["passed"] = True
            report["configurations"].append(configuration)
    return report
