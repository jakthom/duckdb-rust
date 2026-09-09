"""Run the shared subquery corpus through independent native and Rust engines."""
from dataclasses import replace
import hashlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "test/sql/subqueries.test"


def records():
    for block in CORPUS.read_text().split("\n\n"):
        lines = [line for line in block.splitlines() if not line.startswith("#")]
        if not lines:
            continue
        directive = lines[0].split()
        delimiter = lines.index("----") if "----" in lines else len(lines)
        sql = "\n".join(lines[1:delimiter])
        expected = [value for line in lines[delimiter + 1:] for value in line.split("\t")]
        yield directive, sql, expected


def cells(rows, width, ordering):
    def cell(value):
        if value is None:
            return "NULL"
        if isinstance(value, bool):
            return str(int(value))
        return str(value)

    rows = [[cell(row[f"c{i}"]) for i in range(width)] for row in rows]
    if ordering == "rowsort":
        rows.sort()
    result = [value for row in rows for value in row]
    if ordering == "valuesort":
        result.sort()
    return result


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
            for ordinal, (directive, sql, expected) in enumerate(records()):
                if directive[:2] == ["statement", "error"]:
                    for selected, file in [(engine, path), (reference, oracle)]:
                        try:
                            command(selected, file, sql)
                        except RuntimeError:
                            pass
                        else:
                            raise AssertionError(f"expected SQL failure: {sql}")
                elif directive[:2] == ["statement", "ok"]:
                    command(engine, path, sql)
                    command(reference, oracle, sql)
                elif directive[0] == "query":
                    width = len(directive[1])
                    ordering = directive[2] if len(directive) > 2 else "nosort"
                    # Explicit output aliases avoid shell-specific expression names.
                    aliases = ",".join(f"c{i}" for i in range(width))
                    wrapped = f"SELECT * FROM ({sql}) subquery_result({aliases})"
                    for selected, file in [(engine, path), (reference, oracle)]:
                        actual = cells(command(selected, file, wrapped, json_output=True), width, ordering)
                        wanted = sorted(expected) if ordering == "valuesort" else expected
                        assert actual == wanted, (adapter, durability, sql, actual, wanted)
                else:
                    raise AssertionError(f"unsupported oracle directive: {directive}")
                configuration["records"].append({"ordinal": ordinal, "directive": directive, "sql": sql, "passed": True})
            expected = [{"i": 1, "v": 31}, {"i": 2, "v": 32}, {"i": 4, "v": 63}]
            for selected, file in [(reference, path), (rust, oracle)]:
                assert command(selected, file, "SELECT * FROM target ORDER BY i", json_output=True, readonly=True) == expected
            command(reference, path, "UPDATE target SET v=(SELECT max(v) FROM target b)+i; CHECKPOINT")
            command(engine, path, "UPDATE target SET v=(SELECT min(v) FROM target b)-i; CHECKPOINT")
            assert command(reference, path, "SELECT * FROM target ORDER BY i", json_output=True, readonly=True) == [{"i": 1, "v": 63}, {"i": 2, "v": 62}, {"i": 4, "v": 60}]
            configuration["passed"] = True
            report["configurations"].append(configuration)
    return report
