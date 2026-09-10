"""Shared SQL corpus assertions for independent CLI engines and native files."""
from sqllogic import parse


def cells(rows, width, ordering):
    def cell(value):
        if value is None:
            return "NULL"
        if isinstance(value, bool):
            return str(int(value))
        return str(value)

    names = {f"c{i}" for i in range(width)}
    if any(set(row) != names for row in rows):
        raise AssertionError("result column count differs")
    rows = [[cell(row[f"c{i}"]) for i in range(width)] for row in rows]
    if ordering == "rowsort":
        rows.sort()
    result = [value for row in rows for value in row]
    if ordering == "valuesort":
        result.sort()
    return result


def verify_corpus(corpus, engines, command, outcomes=None, *, fail_fast=True):
    """Check unchanged values and success/failure in independent engine states.

    Retain attempted records, including failures, in ``outcomes`` when supplied.
    Continuing after a failure records further observations, never waives the
    failure: the complete corpus still raises. Exact diagnostics are separate.
    """
    if outcomes is None:
        outcomes = []
    failures = 0
    for ordinal, record in enumerate(parse(corpus.read_text())):
        outcome = {"ordinal": ordinal, "line": record.line, "directive": record.words,
                   "sql": record.sql, "passed": False}
        outcomes.append(outcome)
        try:
            verify_record(corpus, record, engines, command)
            outcome["passed"] = True
        except Exception as error:
            outcome["error"] = {"type": type(error).__name__, "message": str(error)}
            if fail_fast:
                raise
            failures += 1
    if failures:
        raise AssertionError(f"{corpus}: {failures} records failed; see retained outcomes")
    return outcomes


def verify_record(corpus, record, engines, command):
    directive, sql = record.words, record.sql
    if directive[:2] == ("statement", "error"):
        for engine, path in engines:
            try:
                command(engine, path, sql)
            except RuntimeError:
                pass
            else:
                raise AssertionError(f"expected SQL failure from {engine}: {sql}")
    elif directive[:2] == ("statement", "ok"):
        for engine, path in engines:
            command(engine, path, sql)
    elif directive[0] == "query":
        width = len(directive[1])
        ordering = directive[2] if len(directive) > 2 else "nosort"
        if ordering not in ("nosort", "rowsort", "valuesort") or len(directive) > 3:
            raise AssertionError(f"unsupported oracle directive: {directive}")
        row_wise = width > 1 and all(len(line.split("\t")) == width for line in record.expected)
        expected = [value for line in record.expected for value in (line.split("\t") if row_wise else [line])]
        wanted = sorted(expected) if ordering == "valuesort" else expected
        aliases = ",".join(f"c{i}" for i in range(width))
        wrapped = f"SELECT * FROM ({sql}) reference_result({aliases})"
        for engine, path in engines:
            actual = cells(command(engine, path, wrapped, json_output=True), width, ordering)
            assert actual == wanted, (str(engine), corpus, record.line, sql, actual, wanted)
    else:
        raise AssertionError(f"unsupported oracle directive: {directive}")
