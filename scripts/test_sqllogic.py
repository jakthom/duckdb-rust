import copy
import pickle
import unittest
from unittest.mock import patch

from sqllogic import Record, Runner, Unsupported, boolean_matches, check_query, numeric_matches, parse


class ConcurrentEngine:
    def __init__(self):
        self.requests = []

    def request(self, request):
        self.requests.append(request)
        if request["operation"] != "concurrent":
            return {"ok": True}
        streams = []
        for stream in request["streams"]:
            responses = []
            for item in stream:
                if item["operation"] == "query":
                    value = item["sql"].removeprefix("SELECT ")
                    if " - " in value:
                        left, right = value.split(" - ", 1)
                        value = str(int(left) - int(right))
                    responses.append({"ok": True, "columns": ["INTEGER"], "rows": [[value]]})
                else:
                    responses.append({"ok": True})
            streams.append(responses)
        return {"ok": True, "streams": streams}


class SQLLogicSchedulingTests(unittest.TestCase):
    def test_boolean_oracle_matches_pinned_typed_fallback(self):
        for actual, expected in (("1", "True"), ("0", "FALSE"),
                                 ("TrUe", "1"), ("FaLsE", "0"),
                                 ("unknown", "also-unknown"), ("NULL", "unknown")):
            with self.subTest(actual=actual, expected=expected):
                self.assertTrue(boolean_matches(actual, expected))
        for actual, expected in (("1", "false"), ("0", "true"),
                                 ("true", "unknown"), ("false", "NULL")):
            with self.subTest(actual=actual, expected=expected):
                self.assertFalse(boolean_matches(actual, expected))
        check_query(
            Record(130, ("query", "I"), expected=("True", "False", "True")),
            {"columns": ["BOOLEAN"], "rows": [["1"], ["0"], ["1"]]},
            {},
        )

    def test_typed_float_oracle_matches_pinned_approx_equal(self):
        # The worker's shortest FLOAT rendering re-casts to the fixture value.
        self.assertTrue(numeric_matches("-2147483600", "-2147483648", "FLOAT"))
        self.assertTrue(numeric_matches("2147483600", "2147483647", "FLOAT"))
        self.assertTrue(numeric_matches("100", "101", "FLOAT"))
        self.assertFalse(numeric_matches("100", "101.01", "FLOAT"))
        # Pinned epsilon is based on the actual (right-hand C++ value).
        self.assertTrue(numeric_matches("101.005", "100", "FLOAT"))
        self.assertFalse(numeric_matches("100", "101.005", "FLOAT"))
        self.assertTrue(numeric_matches("100", "101", "DOUBLE"))
        self.assertFalse(numeric_matches("100", "101.01", "DOUBLE"))
        self.assertTrue(numeric_matches("nan", "-NaN", "FLOAT"))
        self.assertTrue(numeric_matches("infinity", "inf", "DOUBLE"))
        self.assertFalse(numeric_matches("inf", "-inf", "FLOAT"))
        self.assertTrue(numeric_matches("-0", "0", "FLOAT"))
        self.assertFalse(numeric_matches("not-a-number", "1", "FLOAT"))
        self.assertTrue(numeric_matches("1e100", "inf", "FLOAT"))
        self.assertTrue(numeric_matches("1e10000", "infinity", "DOUBLE"))
        self.assertFalse(numeric_matches("1e100", "1", "FLOAT"))
        self.assertFalse(numeric_matches("1e10000", "-inf", "DOUBLE"))
        self.assertFalse(numeric_matches("100", "101", "VARCHAR"))
        self.assertTrue(numeric_matches("1.0", "1", "INTEGER"))
        check_query(
            Record(115, ("query", "I"), expected=("-2147483648", "0", "2147483647")),
            {"columns": ["FLOAT"], "rows": [["-2147483600"], ["0"], ["2147483600"]]},
            {},
        )

    def test_record_is_a_frozen_value_with_dataclass_compatible_basics(self):
        record = Record(7, ("query", "I"))
        self.assertEqual((record.line, record.words, record.sql, record.expected),
                         (7, ("query", "I"), "", ()))
        self.assertEqual(repr(record), "Record(line=7, words=('query', 'I'), sql='', expected=())")
        self.assertEqual(record, Record(7, ("query", "I")))
        self.assertEqual(hash(record), hash((7, ("query", "I"), "", ())))
        self.assertEqual(record.__match_args__, ("line", "words", "sql", "expected"))
        with self.assertRaises(AttributeError):
            record.sql = "SELECT 7"
        with self.assertRaises(AttributeError):
            del record.expected
        self.assertEqual(copy.copy(record), record)
        self.assertEqual(copy.deepcopy(record), record)
        self.assertEqual(pickle.loads(pickle.dumps(record)), record)

        class DerivedRecord(Record):
            pass

        self.assertNotEqual(record, DerivedRecord(7, ("query", "I")))

    def test_loop_binding_splits_only_tuple_iterators(self):
        self.assertEqual(
            Runner.bind_loop({"outer": "0"}, "datatype", "DECIMAL(4,1)"),
            {"outer": "0", "datatype": "DECIMAL(4,1)"},
        )
        self.assertEqual(Runner.bind_loop({}, "datatype", ""), {"datatype": ""})
        self.assertEqual(
            Runner.bind_loop({}, ",right", ",first"),
            {"right": "first", ",right": ",first"},
        )
        self.assertEqual(
            Runner.bind_loop({}, "left,,right", "first,,last"),
            {"left": "first", "right": "last", "left,,right": "first,,last"},
        )
        self.assertEqual(Runner.bind_loop({}, "left,", "first,"), {"left": "first", "left,": "first,"})
        self.assertEqual(Runner.bind_loop({}, ",", ","), {",": ","})
        for name, value in (("left,right", ",first"), ("left,right", "first,"),
                            ("left,right,", "first,second,third"), ("left,right", "")):
            with self.subTest(name=name, value=value), self.assertRaisesRegex(ValueError, name):
                Runner.bind_loop({}, name, value)
        bound = Runner.bind_loop({}, ",right", ",first")
        self.assertEqual(bound[",right"], ",first")
        self.assertEqual(
            Runner(None).replace("'{,right}' '${,right}' '{right}' '${right}'", bound),
            "'{,right}' '${,right}' 'first' 'first'",
        )
        comma_only = Runner.bind_loop({}, ",", ",")
        self.assertEqual(Runner(None).replace("'{,}' '${,}'", comma_only), "',' ','")
        engine = ConcurrentEngine()
        Runner(engine).run(parse(
            "loop outer 0 1\nforeach left,right first,,last\nstatement ok\n"
            "SELECT '{outer}:{left}:{right}'\n\nendloop\nendloop\n"))
        self.assertEqual(engine.requests[-1]["sql"], "SELECT '0:first:last'")

    def test_loop_conditions_continue_and_boundaries(self):
        engine = ConcurrentEngine()
        runner = Runner(engine)
        runner.run(parse("""
loop i 0 5
onlyif i>=1&&i<=3
statement ok
SELECT {i}

onlyif i=2
continue

statement ok
SELECT 99

endloop
loop empty 3 1
statement ok
SELECT 100

endloop
"""))
        self.assertEqual([request["sql"] for request in engine.requests],
                         ["SELECT 99", "SELECT 1", "SELECT 99", "SELECT 2",
                          "SELECT 3", "SELECT 99", "SELECT 99", "SELECT 100"])
        self.assertEqual((runner.passed, runner.skipped), (8, 2))
        self.assertEqual(
            runner.loop_values(("loop", "i", "-1", "2")),
            [2**64 - 1, 0, 1],
        )
        self.assertEqual(runner.loop_values(("loop", "i", "1tail", "3tail")), [1, 2])
        with self.assertRaises(Unsupported):
            runner.loop_values(("foreach", "i", "a", "!a"))

    def test_concurrent_loop_compiles_isolated_streams_with_loop_conditions(self):
        engine = ConcurrentEngine()
        runner = Runner(engine)
        runner.run(parse("""
concurrentloop threadid 0 4
onlyif threadid<>2
query I
SELECT {threadid} - {threadid}
----
0

endloop
"""))
        self.assertEqual((runner.passed, runner.skipped), (3, 1))
        batch = engine.requests[0]
        self.assertEqual(batch["operation"], "concurrent")
        self.assertEqual([[item["sql"] for item in stream] for stream in batch["streams"]],
                         [["SELECT 0 - 0"], ["SELECT 1 - 1"], [], ["SELECT 3 - 3"]])

    def test_loop_values_do_not_rewrite_literal_expectations(self):
        class ResultEngine:
            def request(self, request):
                return {"ok": True, "columns": ["VARCHAR"], "rows": [["0"]]}

        with self.assertRaises(AssertionError):
            Runner(ResultEngine()).run(parse("""
loop i 0 1
query T
SELECT '{i}'
----
{i}

endloop
"""))

    def test_concurrent_rejects_nested_parallel_named_sessions_and_continue(self):
        for source, message in [
            ("concurrentloop i 0 2\nstatement ok named\nSELECT 1\n\nendloop\n", "Named connections"),
            ("concurrentloop i 0 2\ncontinue\n\nendloop\n", "not supported"),
            ("concurrentloop i 0 2\nconcurrentloop j 0 2\nstatement ok\nSELECT 1\n\nendloop\nendloop\n", "Nested parallel"),
        ]:
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                Runner(ConcurrentEngine()).run(parse(source))

    def test_concurrent_shared_stop_surfaces_failure_not_truncation(self):
        class StoppingEngine:
            def request(self, request):
                self.request = request
                return {"ok": True, "streams": [
                    [{"ok": False, "message": "sentinel query failure"}],
                    [],
                ]}

        runner = Runner(StoppingEngine())
        with self.assertRaisesRegex(AssertionError, "sentinel query failure"):
            runner.run(parse("""
concurrentloop i 0 2
query I
SELECT {i} - {i}
----
0

query I
SELECT 99
----
99

endloop
"""))

    def test_restart_accepts_pinned_no_extension_load_spelling(self):
        engine = ConcurrentEngine()
        Runner(engine).run(parse("restart no_extension_load\n"))
        self.assertEqual(engine.requests, [{"operation": "restart"}])

    def test_missing_and_non_numeric_loop_conditions_fail(self):
        for source in [
            "loop i 0 1\nonlyif missing=0\nstatement ok\nSELECT 1\n\nendloop\n",
            "foreach i text\nonlyif i>0\nstatement ok\nSELECT 1\n\nendloop\n",
        ]:
            with self.assertRaises(ValueError):
                Runner(ConcurrentEngine()).run(parse(source))

    def test_exact_comparators_and_integer_domains(self):
        for source in [
            "loop i 0 1\nonlyif i==0\nstatement ok\nSELECT 1\n\nendloop\n",
            "loop i 2147483648 2147483649\nstatement ok\nSELECT 1\n\nendloop\n",
            "foreach i 9223372036854775808\nonlyif i>0\nstatement ok\nSELECT 1\n\nendloop\n",
        ]:
            with self.subTest(source=source), self.assertRaises(ValueError):
                Runner(ConcurrentEngine()).run(parse(source))
        with self.assertRaisesRegex(Exception, "condition"):
            Runner(ConcurrentEngine()).run(parse(
                "loop i 0 1\nonlyif i!=0\nstatement ok\nSELECT 1\n\nendloop\n"))

    def test_relational_conditions_follow_stoll_prefix_and_i64_rules(self):
        runner = Runner(ConcurrentEngine())
        self.assertTrue(runner.condition("i>+11tail", {"i": " \t12suffix"}, True))
        for value in ("tail", "١٢tail", "１２tail", "9223372036854775808"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                runner.condition("i>0", {"i": value}, True)

    def test_nested_foreach_variable_is_not_looked_up_before_execution(self):
        class VariableEngine:
            def __init__(self):
                self.defined = False

            def request(self, request):
                if request["operation"] == "foreach":
                    if not self.defined:
                        return {"ok": False, "message": "variable not defined"}
                    return {"ok": True, "values": ["alpha"]}
                if "SET VARIABLE choices" in request.get("sql", ""):
                    self.defined = True
                return {"ok": True}

        engine = VariableEngine()
        Runner(engine).run(parse(
            "loop outer 0 1\n"
            "statement ok\nSET VARIABLE choices = ['alpha']\n\n"
            "foreach choice <variable:choices>\n"
            "statement ok\nSELECT '{choice}'\n\nendloop\n\nendloop\n"))
        self.assertTrue(engine.defined)

    def test_foreach_collections_removal_tuple_and_variable(self):
        engine = ConcurrentEngine()
        engine.request = lambda request: (
            {"ok": True, "values": ["x,y", "z,w"]}
            if request["operation"] == "foreach" else {"ok": True})
        runner = Runner(engine)
        values = runner.foreach_values(["<signed>", "!integer", "<compression>"])
        self.assertEqual(values[:4], ["tinyint", "smallint", "bigint", "hugeint"])
        self.assertEqual(values[-9:],
                         "none uncompressed rle bitpacking dictionary fsst dict_fsst alp alprd".split())
        self.assertEqual(len(runner.foreach_values(["<all_types_columns>"])), 53)
        runner.run(parse(
            "foreach left,right <variable:pairs>\n"
            "statement ok\nSELECT '{left}:{right}'\n\nendloop\n"))

    def test_hash_prefilter_keeps_unicode_digit_fullmatch_and_malformed_literals(self):
        unicode_hash = "١ values hashing to " + "0" * 32
        with patch("sqllogic.hash_values", return_value=unicode_hash) as digest:
            check_query(
                Record(1, ("query", "T"), expected=(unicode_hash,)),
                {"columns": ["VARCHAR"], "rows": [["x"]]},
                {},
            )
        digest.assert_called_once_with(["x"])

        malformed = "١ values hashing to xyz"
        with patch("sqllogic.hash_values", side_effect=AssertionError("must stay literal")):
            check_query(
                Record(2, ("query", "T"), expected=(malformed,)),
                {"columns": ["VARCHAR"], "rows": [[malformed]]},
                {},
            )


if __name__ == "__main__":
    unittest.main()
