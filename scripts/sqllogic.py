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
        # Mathematical zero alone is insufficient: development rejects e.g.
        # '-0.0' when parsing an unsigned value. Conservatively decline all
        # minus-prefixed unsigned fallback spellings, including integral '-0'.
        if kind.startswith('U') and any(value.startswith('-') for value in (actual, expected)):
            return False
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
    def __init__(self, engine, substitutions=None, max_records=100000, max_threads=None):
        self.engine = engine
        self.substitutions = substitutions or {}
        self.reserved = frozenset(self.substitutions)
        self.max_records = max_records
        self.max_threads = max_threads
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

    @staticmethod
    def stoll(value):
        """Match std::stoll's base-10 prefix, sign, whitespace and i64 range."""
        match = re.match(r"[ \t\n\r\f\v]*[+-]?[0-9]+", str(value))
        if match is None:
            raise ValueError(f"not a std::stoll number: {value!r}")
        result = int(match.group())
        if not -(2**63) <= result < 2**63:
            raise ValueError(f"std::stoll value is outside signed 64-bit range: {value!r}")
        return result

    def condition(self, expression, variables, only_if):
        """Evaluate pinned loop-variable/system onlyif and skipif expressions."""
        # The pinned parser lowercases the condition token and does not apply
        # loop/substitution replacement to it.
        expression = expression.lower()
        systems = ("duckdb", "sqlite", "mysql", "postgresql", "mssql")
        if expression in systems:
            holds = expression == "duckdb"
            return holds if only_if else not holds
        outcomes = []
        for term in expression.split("&&"):
            parsed = None
            for operator in ("<>", ">=", ">", "<=", "<", "="):
                if operator not in term:
                    continue
                parts = term.split(operator)
                if len(parts) != 2:
                    raise ValueError(f"invalid loop condition {term}")
                parsed = (parts[0].strip(), operator, parts[1].strip())
                break
            if parsed is None or not parsed[0] or not parsed[2]:
                raise Unsupported(f"condition {expression}")
            name, operator, right = parsed
            if name not in variables:
                raise ValueError(f"condition iterator {name} was not found")
            left = str(variables[name])
            if operator in ("=", "<>"):
                holds = left == right
                if operator == "<>":
                    holds = not holds
            else:
                try:
                    lhs_number, rhs_number = self.stoll(left), self.stoll(right)
                except ValueError as error:
                    raise ValueError(f"non-numeric loop condition {term}") from error
                holds = {"<": lhs_number < rhs_number, "<=": lhs_number <= rhs_number,
                         ">": lhs_number > rhs_number, ">=": lhs_number >= rhs_number}[operator]
            outcomes.append(holds)
        return all(outcomes) if only_if else not any(outcomes)

    @staticmethod
    def loop_body(records, position):
        begin, nesting = position, 1
        while position < len(records) and nesting:
            next_op = records[position].words[0]
            nesting += int(next_op in ("loop", "foreach", "concurrentloop", "concurrentforeach"))
            nesting -= int(next_op == "endloop")
            position += 1
        if nesting:
            raise ValueError("unterminated loop")
        return records[begin:position-1], position

    def foreach_values(self, tokens):
        signed = ["tinyint", "smallint", "integer", "bigint", "hugeint"]
        unsigned = ["utinyint", "usmallint", "uinteger", "ubigint", "uhugeint"]
        all_columns = "bool tinyint smallint int bigint hugeint uhugeint utinyint usmallint uint ubigint date time timestamp timestamp_s timestamp_ms timestamp_ns time_tz timestamp_tz float double dec_4_1 dec_9_4 dec_18_6 dec38_10 uuid interval varchar blob bit small_enum medium_enum large_enum int_array double_array date_array timestamp_array timestamptz_array varchar_array nested_int_array struct struct_of_arrays array_of_structs map union fixed_int_array fixed_varchar_array fixed_nested_int_array fixed_nested_varchar_array fixed_struct_array struct_of_fixed_array fixed_array_of_int_list list_of_fixed_int_array".split()
        collections = {
            "<signed>": signed,
            "<unsigned>": unsigned,
            "<integral>": signed + unsigned,
            "<numeric>": signed + unsigned + ["float", "double"],
            "<alltypes>": signed + unsigned + ["float", "double", "bool", "interval", "varchar"],
            "<compression>": "none uncompressed rle bitpacking dictionary fsst dict_fsst alp alprd".split(),
            "<all_types_columns>": all_columns,
        }
        result = []
        for token in tokens:
            lower = token.strip().lower()
            if lower.startswith("<variable:"):
                if not lower.endswith(">"):
                    raise ValueError(f"invalid foreach variable {token}")
                parts = token[len("<variable:"):-1].split(":")
                if len(parts) == 1:
                    connection, name = "", parts[0]
                elif len(parts) == 2:
                    connection, name = parts
                else:
                    raise ValueError(f"invalid foreach variable {token}")
                response = self.engine.request({"operation": "foreach", "connection": connection, "sql": name})
                if response.get("unsupported"):
                    raise Unsupported(response["message"])
                if not response.get("ok"):
                    raise AssertionError(response.get("message", "foreach variable lookup failed"))
                result.extend(response.get("values", []))
            elif lower in collections:
                result.extend(collections[lower])
            elif lower.startswith("!"):
                try:
                    result.remove(token[1:])
                except ValueError:
                    result.append(token)
            else:
                result.append(token)
        return result

    def loop_values(self, words):
        if len(words) < 3 or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*(?:,[A-Za-z_][A-Za-z0-9_]*)*", words[1]):
            raise ValueError("invalid loop iterator")
        if words[0] in ("loop", "concurrentloop"):
            if len(words) != 4:
                raise ValueError("loop requires name, inclusive start and exclusive end")
            bounds = [re.match(r"\s*[+-]?\d+", value) for value in words[2:]]
            if any(match is None for match in bounds):
                raise ValueError("loop start and end must be numbers")
            start, end = (int(match.group()) for match in bounds)
            if not -(2**31) <= start < 2**31 or not -(2**31) <= end < 2**31:
                raise ValueError("loop bounds are outside std::stoi range")
            current, end = start % (2**64), end % (2**64)
            values = []
            while True:
                if len(values) == self.max_records:
                    raise Unsupported("loop expansion exceeds record limit")
                values.append(current)
                current = (current + 1) % (2**64)
                if current >= end:
                    return values
        values = self.foreach_values(words[2:])
        if not values:
            raise Unsupported("foreach expansion produced no iterations")
        return values

    @staticmethod
    def bind_loop(variables, name, value):
        names, values = name.split(","), str(value).split(",")
        if len(names) != len(values):
            raise ValueError(f"foreach iterator {name} does not match replacement {value}")
        return {**variables, **dict(zip(names, values)), name: value}

    def request_for(self, record):
        words, op = record.words, record.words[0]
        connection = ""
        if op == "statement":
            if len(words) not in (2, 3) or words[1] not in ("ok", "error"):
                raise ValueError("invalid statement signature")
            if len(words) == 3:
                connection = words[2]
        elif len(words) > 2 and words[2] not in ("none", "nosort", "rowsort", "valuesort"):
            connection = words[2]
        return {"operation": op, "sql": record.sql, "connection": connection,
                "expect_error": op == "statement" and words[1] == "error"}

    def check_response(self, record, response):
        if response.get("unsupported"):
            raise Unsupported(response["message"])
        words, op = record.words, record.words[0]
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

    def compile_stream(self, records, variables, depth=0):
        """Expand a concurrent iteration without executing it.

        Pinned concurrent loops accept only statement/query and nested serial
        loops whose leaves support concurrency. Named connections and continue
        are rejected before the worker starts any stream.
        """
        if depth > 64:
            raise Unsupported("test loop nesting exceeds 64")
        result, position, conditions = [], 0, []
        while position < len(records):
            original = records[position]
            position += 1
            record = Record(original.line, original.words,
                            self.replace(original.sql, variables), tuple(self.replace(v, {}) for v in original.expected))
            words, op = record.words, record.words[0]
            if op in ("skipif", "onlyif"):
                if len(words) != 2:
                    raise ValueError("skipif/onlyif requires one condition")
                conditions.append(self.condition(words[1], variables, op == "onlyif"))
                continue
            selected = all(conditions)
            conditions.clear()
            if op in ("query", "statement"):
                if not selected:
                    self.skipped += 1
                    continue
                request = self.request_for(record)
                if request["connection"]:
                    raise ValueError("Named connections not supported in parallel loop")
                result.append(record)
            elif op in ("loop", "foreach"):
                if not selected:
                    raise ValueError("conditions on loop controls are not supported")
                body, position = self.loop_body(records, position)
                values = self.loop_values(words)
                for value in values:
                    result.extend(self.compile_stream(body, self.bind_loop(variables, words[1], value), depth + 1))
            elif op in ("concurrentloop", "concurrentforeach"):
                raise ValueError("Nested parallel loop commands not allowed")
            elif op == "endloop":
                raise ValueError("endloop without active loop")
            elif op == "continue":
                raise ValueError("Concurrent loop is not supported over this command")
            else:
                raise ValueError("Concurrent loop is not supported over this command")
        if conditions:
            raise ValueError("dangling test condition")
        return result

    def run(self, records, variables=None, depth=0):
        if depth > 64:
            raise Unsupported("test loop nesting exceeds 64")
        variables = variables or {}
        position, conditions = 0, []
        while position < len(records):
            original = records[position]
            position += 1
            self.line = original.line
            record = Record(original.line, original.words,
                            self.replace(original.sql, variables), tuple(self.replace(v, {}) for v in original.expected))
            words, op = record.words, record.words[0]
            if op in ("skipif", "onlyif"):
                if len(words) != 2:
                    raise ValueError("skipif/onlyif requires one condition")
                conditions.append(self.condition(words[1], variables, op == "onlyif"))
                continue
            if op in ("query", "statement"):
                if self.passed + self.skipped >= self.max_records:
                    raise Unsupported("expanded test record limit exceeded")
                selected = all(conditions)
                conditions.clear()
                if not selected:
                    self.skipped += 1
                    continue
                response = self.engine.request(self.request_for(record))
                self.check_response(record, response)
                self.passed += 1
            elif op in ("loop", "foreach"):
                if conditions:
                    raise Unsupported("conditions on loop controls")
                body, position = self.loop_body(records, position)
                values = self.loop_values(words)
                if len(values) > self.max_records:
                    raise Unsupported("loop expansion exceeds record limit")
                for value in values:
                    self.run(body, self.bind_loop(variables, words[1], value), depth+1)
            elif op in ("concurrentloop", "concurrentforeach"):
                if conditions:
                    raise Unsupported("conditions on loop controls")
                body, position = self.loop_body(records, position)
                values = list(self.loop_values(words))
                if len(values) > self.max_records:
                    raise Unsupported("loop expansion exceeds record limit")
                streams = [self.compile_stream(body, self.bind_loop(variables, words[1], value), depth + 1)
                           for value in values]
                requests = [[self.request_for(item) for item in stream] for stream in streams]
                request = {"operation": "concurrent", "streams": requests}
                if self.max_threads is not None:
                    request["max_threads"] = self.max_threads
                response = self.engine.request(request)
                if response.get("unsupported"):
                    raise Unsupported(response["message"])
                if not response.get("ok"):
                    raise AssertionError(response.get("message", "concurrent worker failed"))
                responses = response.get("streams")
                if not isinstance(responses, list) or len(responses) != len(streams):
                    raise AssertionError("concurrent worker returned the wrong stream count")
                failure = None
                failure_line = None
                truncated = False
                for stream, stream_responses in zip(streams, responses):
                    if not isinstance(stream_responses, list) or len(stream_responses) > len(stream):
                        raise AssertionError("concurrent worker returned an invalid response stream")
                    truncated |= len(stream_responses) < len(stream)
                    for item, item_response in zip(stream, stream_responses):
                        if self.passed + self.skipped >= self.max_records:
                            raise Unsupported("expanded test record limit exceeded")
                        self.line = item.line
                        try:
                            self.check_response(item, item_response)
                        except Exception as error:
                            if failure is None:
                                failure, failure_line = error, item.line
                        else:
                            self.passed += 1
                if failure is not None:
                    self.line = failure_line
                    raise failure
                if truncated:
                    raise AssertionError("concurrent worker truncated responses without a recorded failure")
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
                        request["path"] = self.replace(words[1], variables)
                    request["read_only"] = len(words) == 3 and words[2] == "readonly"
                elif op == "restart":
                    if len(words) == 2 and words[1] == "no_extension_load":
                        # The Rust engine has no loadable extension state yet;
                        # accepting the pinned spelling is observably equivalent.
                        pass
                    elif len(words) > 1:
                        raise Unsupported(f"{op} options")
                elif len(words) > 1:
                    raise Unsupported(f"{op} options")
                response = self.engine.request(request)
                if not response["ok"]:
                    raise Unsupported(response["message"]) if response.get("unsupported") else AssertionError(response["message"])
            elif op == "continue":
                if depth == 0:
                    raise ValueError("continue cannot be called outside of a loop")
                selected = all(conditions)
                conditions.clear()
                if selected:
                    return
            elif op == "endloop":
                raise ValueError("endloop without active loop")
            elif op == "halt":
                if position < len(records):
                    raise Unsupported("halt leaves unexecuted records")
            else:
                raise Unsupported(f"test directive {op}: {' '.join(words[1:])}")
        if conditions:
            raise ValueError("dangling test condition")
