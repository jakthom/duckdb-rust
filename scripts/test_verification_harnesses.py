"""Tests of the test oracles: deliberate wrong results must be detected."""
import unittest

from compare_native import compare
from run_upstream import REQUIRED_SCOPES, summarize
from sqllogic import Record, Runner, Unsupported, check_query, hash_values, parse


class RecordingEngine:
    def __init__(self, responses=None):
        self.requests = []
        self.responses = iter(responses or [])

    def request(self, request):
        self.requests.append(request)
        return next(self.responses, {"ok": True})


class LogicTests(unittest.TestCase):
    def test_parse_comments_conditions_errors_and_literal_hash_marks(self):
        records = parse("# comment\n\nskipif mysql\nstatement error c1\nSELECT bad\n----\n<REGEX>:.*bad.*\n\nquery T\nSELECT '#text'\n----\n#text\n")
        self.assertEqual([r.words[0] for r in records], ["skipif", "statement", "query"])
        self.assertEqual(records[1].line, 4)
        self.assertEqual(records[2].expected, ("#text",))

    def test_hashes_and_labels_detect_changed_values_and_cardinality(self):
        labels = {}
        record = Record(1, ("query", "I", "rowsort", "same"), expected=(hash_values(["1", "2"]),))
        check_query(record, {"columns": ["BIGINT"], "rows": [["2"], ["1"]]}, labels)
        for rows in [[["1"]], [["1"], ["3"]]]:
            with self.assertRaises(AssertionError):
                check_query(record, {"columns": ["BIGINT"], "rows": rows}, labels)
        with self.assertRaises(AssertionError):
            check_query(Record(2, ("query", "I", "nosort", "same")), {"columns": ["BIGINT"], "rows": [["9"]]}, labels)

    def test_regex_and_empty_results_are_actual_oracles(self):
        query = Record(1, ("query", "T"), expected=("<REGEX>:a.*",))
        check_query(query, {"columns": ["VARCHAR"], "rows": [["abc"]]}, {})
        with self.assertRaises(AssertionError):
            check_query(query, {"columns": ["VARCHAR"], "rows": [["ba"]]}, {})
        with self.assertRaises(AssertionError):
            check_query(Record(1, ("query", "I")), {"columns": ["INTEGER"], "rows": [["1"]]}, {})

    def test_tabs_in_a_single_text_column_are_not_extra_expected_rows(self):
        record = Record(1, ("query", "T"), expected=("a\tb",))
        check_query(record, {"columns": ["VARCHAR"], "rows": [["a\tb"]]}, {})
        with self.assertRaises(AssertionError):
            check_query(record, {"columns": ["VARCHAR"], "rows": [["a"], ["b"]]}, {})

    def test_nested_loops_substitute_sql_and_named_connections(self):
        engine = RecordingEngine()
        runner = Runner(engine)
        runner.run(parse("loop i 0 2\nforeach j 7 9\nstatement ok writer\nINSERT INTO t VALUES (${i},${j})\n\nendloop\nendloop\n"))
        self.assertEqual(runner.passed, 4)
        self.assertEqual([r["sql"] for r in engine.requests], [f"INSERT INTO t VALUES ({i},{j})" for i in [0, 1] for j in [7, 9]])
        self.assertTrue(all(r["connection"] == "writer" for r in engine.requests))

    def test_conditions_are_recorded_and_cannot_turn_unsupported_into_success(self):
        engine = RecordingEngine([{"ok": False, "unsupported": True, "message": "unimplemented SQL"}])
        runner = Runner(engine)
        with self.assertRaises(Unsupported):
            runner.run(parse("onlyif sqlite\nstatement ok\nSELECT 1\n\nstatement error\nSELECT unsupported\n"))
        self.assertEqual((runner.skipped, runner.passed), (1, 0))
        self.assertEqual(len(engine.requests), 1)

    def test_unknown_directives_and_malformed_loops_fail(self):
        for source in ["require parquet\n", "mode skip\n", "concurrentloop i 0 3\n", "halt\nstatement ok\nSELECT 1\n"]:
            with self.assertRaises(Unsupported):
                Runner(RecordingEngine()).run(parse(source))
        with self.assertRaises(ValueError):
            Runner(RecordingEngine()).run(parse("loop i 0 3\n"))

    def test_statement_error_must_match_and_queries_validate_column_count(self):
        engine = RecordingEngine([{"ok": False, "message": "Binder Error: missing"}])
        Runner(engine).run(parse("statement error\nSELECT missing\n----\n<REGEX>:Binder Error:.*\n"))
        with self.assertRaises(AssertionError):
            Runner(RecordingEngine()).run(parse("statement error\nSELECT 1\n"))
        with self.assertRaises(AssertionError):
            check_query(Record(1, ("query", "II")), {"columns": ["BIGINT"], "rows": []}, {})

    def test_variables_preserve_both_syntaxes_and_reserved_paths(self):
        engine = RecordingEngine()
        runner = Runner(engine, {"{TEST_DIR}": "/scratch"})
        runner.run(parse("set variable n 7\nstatement ok\nSELECT {n},${n}\n\nset variable n 8\nstatement ok\nSELECT {n},${n}\n"))
        self.assertEqual([r["sql"] for r in engine.requests], ["SELECT 7,7", "SELECT 8,8"])
        with self.assertRaises(Unsupported):
            runner.run(parse("set variable TEST_DIR elsewhere\n"))


class RegressionGateTests(unittest.TestCase):
    def test_any_slowdown_fails_without_allowance_or_compensation(self):
        expected = {"name": "case", "rows": 1, "sum": "7"}
        sample = lambda n: {"rows": 1, "sum": "7", "elapsed_ns": n}
        baseline = [sample(1000000)] * 9
        self.assertTrue(compare(baseline, baseline, expected)["passed"])
        self.assertFalse(compare(baseline, [sample(1000001)] * 9, expected)["passed"])
        with self.assertRaises(ValueError):
            compare(baseline, [{"rows": 1, "sum": "8", "elapsed_ns": 1}] * 9, expected)
        with self.assertRaises(ValueError):
            compare(baseline[:1], baseline[:1], expected)


class SuiteGateTests(unittest.TestCase):
    def test_missing_duplicate_skipped_or_unmapped_cases_never_pass(self):
        cases = [{"id": "a"}, {"id": "b"}]
        results = [{"id": "a", "status": "passed"}, {"id": "b", "status": "passed"}]
        obligations = dict.fromkeys(REQUIRED_SCOPES, "passed")
        self.assertTrue(summarize(cases, cases, results, [], obligations)["full_suite_passed"])
        for selected, outcomes, unported, scopes in [
            (cases[:1], results[:1], [], obligations),
            (cases, results[:1], [], obligations),
            (cases, results + results[:1], [], obligations),
            (cases, [results[0], {"id": "b", "status": "unsupported"}], [], obligations),
            (cases, results, [{"id": "native.cpp:1"}], obligations),
            (cases, results, [], {}),
            (cases, results, [], {**obligations, "platforms": "unverified"}),
        ]:
            with self.subTest(selected=selected, outcomes=outcomes, unported=unported, scopes=scopes):
                self.assertFalse(summarize(cases, selected, outcomes, unported, scopes)["full_suite_passed"])
        self.assertFalse(summarize([], [], [], [], obligations)["full_suite_passed"])


if __name__ == "__main__":
    unittest.main()
