"""DuckDB SQLLogicTest records, oracles and execution, independent of transport."""
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
import hashlib
import re


class Unsupported(Exception):
    pass


@dataclass(frozen=True)
class Record:
    line: int
    words: tuple
    sql: str = ""
    expected: tuple = ()


def parse(source):
    lines = source.replace("\r", "").splitlines()
    result, position = [], 0
    while position < len(lines):
        text = lines[position].strip()
        position += 1
        if not text or text.startswith("#"):
            continue
        line, words = position, tuple(text.split())
        sql, expected = [], []
        if words[0] in ("statement", "query"):
            while position < len(lines) and lines[position].strip() and lines[position].strip() != "----":
                sql.append(lines[position])
                position += 1
            if position < len(lines) and lines[position].strip() == "----":
                position += 1
                while position < len(lines) and lines[position] != "":
                    expected.append(lines[position])
                    position += 1
            if not sql:
                raise ValueError(f"line {line}: empty SQL")
        result.append(Record(line, words, "\n".join(sql), tuple(expected)))
    return result


def matches(actual, expected):
    for prefix, positive in [("<REGEX>:", True), ("<!REGEX>:", False)]:
        if expected.startswith(prefix):
            pattern = expected[len(prefix):]
            if re.search(r"\\[1-9]|\(\?[=!]|\(\?<[=!]", pattern):
                raise Unsupported("regular-expression feature outside upstream RE2")
            return (re.fullmatch(pattern, actual, re.DOTALL) is not None) == positive
    return actual == expected


def numeric_matches(actual, expected, kind):
    """Conservative exact subset of upstream CompareValues' typed fallback.

    Authority is the returned logical type, not the SQLLogicTest I/R marker.
    No tolerance, rounding, cast engine, or formatted/hash-value rewriting is
    introduced here. Approximate FLOAT comparisons and expected values requiring
    a lossy cast remain explicit oracle limitations.
    """
    integer_bits = {'TINYINT': 8, 'SMALLINT': 16, 'INTEGER': 32, 'BIGINT': 64,
                    'HUGEINT': 128, 'UTINYINT': 8, 'USMALLINT': 16,
                    'UINTEGER': 32, 'UBIGINT': 64, 'UHUGEINT': 128}
    decimal = re.fullmatch(r'DECIMAL\(([0-9]+),\s*([0-9]+)\)', kind)
    floating = kind in ('FLOAT', 'DOUBLE')
    if kind not in integer_bits and not decimal and not floating:
        return False
    number = r'[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?'
    special = r'[+-]?(?:nan|inf|infinity)'
    if any(not re.fullmatch(number, value)
           and not (floating and re.fullmatch(special, value, re.IGNORECASE))
           for value in (actual, expected)):
        return False
    try:
        left, right = Decimal(actual), Decimal(expected)
    except (InvalidOperation, ValueError):
        return False
    if left.is_nan() or right.is_nan():
        return floating and left.is_nan() and right.is_nan()
    if left != right:
        return False
    if not left.is_finite():
        return floating
    if kind in integer_bits:
        # Bound the exponent before integer construction, then check exact
        # integrality and the full declared signed/unsigned domain.
        if left.adjusted() > 38 or left != left.to_integral_value():
            return False
        bits = integer_bits[kind]
        lower, upper = (0, 1 << bits) if kind.startswith('U') else (-(1 << (bits - 1)), 1 << (bits - 1))
        return lower <= int(left) < upper
    if decimal:
        width, scale = map(int, decimal.groups())
        if not 1 <= width <= 38 or not 0 <= scale <= width:
            return False
        if left.is_zero():
            return True
        _, digits, exponent = left.as_tuple()
        trailing = 0
        for digit in reversed(digits):
            if digit != 0:
                break
            trailing += 1
        return exponent + trailing >= -scale and left.adjusted() < width - scale
    return True


def hash_values(values):
    digest = hashlib.md5(usedforsecurity=False)
    for value in values:
        digest.update(value.encode() + b"\n")
    return f"{len(values)} values hashing to {digest.hexdigest()}"


def check_query(record, response, labels):
    words = record.words
    if len(words) < 2 or len(words) > 4 or not words[1] or set(words[1]) - set("IRT"):
        raise ValueError("invalid query signature")
    columns = len(words[1])
    if len(response["columns"]) != columns or any(len(row) != columns for row in response["rows"]):
        raise AssertionError("result column count differs")
    rows = response["rows"]
    mode = words[2] if len(words) > 2 else "nosort"
    if mode == "rowsort":
        rows = sorted(rows)
    actual = [value for row in rows for value in row]
    if mode == "valuesort":
        actual.sort()
    digest = hash_values(actual)
    expected = record.expected
    if len(expected) == 1 and re.fullmatch(r"\d+ values hashing to [0-9a-f]{32}", expected[0]):
        if digest != expected[0]:
            raise AssertionError(f"expected {expected[0]}, got {digest}")
    elif expected:
        if len(expected) == 1 and expected[0].startswith("<FILE>:"):
            raise Unsupported("external expected-result file oracle")
        # Upstream permits either one value per line or a tab-separated row.
        row_wise = columns > 1 and all(len(line.split("\t")) == columns for line in expected)
        values = [cell for line in expected for cell in (line.split("\t") if row_wise else [line])]
        # Flattened mixed-type valuesort loses column ownership. Until that
        # transport carries it explicitly, do not guess a numeric type from
        # the sorted position and accidentally relax VARCHAR comparisons.
        numeric_fallback = mode != 'valuesort' or len(set(response['columns'])) == 1
        if len(actual) != len(values) or not all(
                matches(a, e) or (numeric_fallback and numeric_matches(a, e, response['columns'][index % columns]))
                for index, (a, e) in enumerate(zip(actual, values))):
            raise AssertionError(f"expected {values[:12]}, got {actual[:12]} ({len(actual)} values)")
    elif len(words) < 4 and actual:
        raise AssertionError(f"expected an empty result, got {actual[:12]}")
    if len(words) == 4:
        previous = labels.setdefault(words[3], digest)
        if previous != digest:
            raise AssertionError(f"label {words[3]} changed: {previous} != {digest}")


class Runner:
    """An engine supplies request(dict)->dict; unsupported controls never pass."""
    def __init__(self, engine, substitutions=None, max_records=100000):
        self.engine = engine
        self.substitutions = substitutions or {}
        self.reserved = frozenset(self.substitutions)
        self.max_records = max_records
        self.passed = 0
        self.skipped = 0
        self.labels = {}
        self.line = 0

    def replace(self, text, variables):
        for key in sorted(self.substitutions, key=len, reverse=True):
            text = text.replace(key, str(self.substitutions[key]))
        for key, value in variables.items():
            text = text.replace("${" + key + "}", str(value))
            text = text.replace("{" + key + "}", str(value))
        return text

    def run(self, records, variables=None, depth=0):
        if depth > 64:
            raise Unsupported("test loop nesting exceeds 64")
        variables = variables or {}
        position, conditions = 0, []
        while position < len(records):
            original = records[position]
            position += 1
            self.line = original.line
            record = Record(original.line, tuple(self.replace(w, variables) for w in original.words),
                            self.replace(original.sql, variables), tuple(self.replace(v, variables) for v in original.expected))
            words, op = record.words, record.words[0]
            if op in ("skipif", "onlyif"):
                if len(words) != 2 or words[1] not in ("duckdb", "sqlite", "mysql", "postgresql", "mssql"):
                    raise Unsupported(f"condition {words}")
                conditions.append((op == "onlyif") == (words[1] == "duckdb"))
                continue
            if op in ("query", "statement"):
                if self.passed + self.skipped >= self.max_records:
                    raise Unsupported("expanded test record limit exceeded")
                selected = all(conditions)
                conditions.clear()
                if not selected:
                    self.skipped += 1
                    continue
                connection = ""
                if op == "statement":
                    if len(words) not in (2, 3) or words[1] not in ("ok", "error"):
                        raise ValueError("invalid statement signature")
                    if len(words) == 3:
                        connection = words[2]
                elif len(words) > 2 and words[2] not in ("none", "nosort", "rowsort", "valuesort"):
                    connection = words[2]
                response = self.engine.request({"operation": op, "sql": record.sql, "connection": connection})
                if response.get("unsupported"):
                    raise Unsupported(response["message"])
                if op == "statement" and words[1] == "error":
                    if response["ok"]:
                        raise AssertionError("expected SQL failure")
                    expected = "\n".join(record.expected)
                    if expected and not (matches(response["message"], expected) if expected.startswith(("<REGEX>:", "<!REGEX>:")) else expected in response["message"]):
                        raise AssertionError(f"wrong error: {response['message']}")
                else:
                    if not response["ok"]:
                        raise AssertionError(response["message"])
                    if op == "query":
                        check_query(record, response, self.labels)
                self.passed += 1
            elif op in ("loop", "foreach"):
                if conditions:
                    raise Unsupported("conditions on loop controls")
                begin, nesting = position, 1
                while position < len(records) and nesting:
                    next_op = records[position].words[0]
                    nesting += int(next_op in ("loop", "foreach", "concurrentloop", "concurrentforeach")) - int(next_op == "endloop")
                    position += 1
                if nesting:
                    raise ValueError("unterminated loop")
                if len(words) < 3:
                    raise ValueError("invalid loop")
                if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", words[1]):
                    raise Unsupported("compound loop variables")
                if op == "loop":
                    if len(words) != 4:
                        raise ValueError("loop requires name, inclusive start and exclusive end")
                    values = range(int(words[2]), int(words[3]))
                else:
                    values = words[2:]
                if len(values) > self.max_records:
                    raise Unsupported("loop expansion exceeds record limit")
                for value in values:
                    self.run(records[begin:position-1], {**variables, words[1]: value}, depth+1)
            elif op == "hash-threshold" and len(words) == 2:
                if int(words[1]) < 0:
                    raise ValueError("negative hash threshold")
                # Explicit hashes and labels are always checked. Literal results
                # remain checked directly even when the threshold is exceeded.
            elif op == "set" and len(words) == 4 and words[1] == "variable":
                key = "{" + words[2] + "}"
                if key in self.reserved:
                    raise Unsupported("reserved harness variable")
                self.substitutions[key] = words[3]
                self.substitutions["${" + words[2] + "}"] = words[3]
            elif op == "reset" and len(words) == 3 and words[1] == "label":
                self.labels.pop(words[2], None)
            elif op in ("load", "restart", "reconnect"):
                request = {"operation": op}
                if op == "load":
                    if len(words) > 3 or (len(words) == 3 and words[2] not in ("readonly", "readwrite")):
                        raise Unsupported(f"load options {words[1:]}")
                    if len(words) >= 2:
                        request["path"] = words[1]
                    request["read_only"] = len(words) == 3 and words[2] == "readonly"
                elif len(words) > 1:
                    raise Unsupported(f"{op} options")
                response = self.engine.request(request)
                if not response["ok"]:
                    raise Unsupported(response["message"]) if response.get("unsupported") else AssertionError(response["message"])
            elif op == "halt":
                if position < len(records):
                    raise Unsupported("halt leaves unexecuted records")
            else:
                raise Unsupported(f"test directive {op}: {' '.join(words[1:])}")
        if conditions:
            raise ValueError("dangling test condition")
