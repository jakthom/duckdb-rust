"""Tests of the test oracles: deliberate wrong results must be detected."""
import unittest
from pathlib import Path
import tempfile

from compare_native import compare
from run_upstream import REQUIRED_SCOPES, summarize
from sqllogic import Record, Runner, Unsupported, check_query, hash_values, parse
from sql_reference import verify_corpus


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

    def test_exact_numeric_fallback_uses_returned_type_without_tolerance(self):
        for kind, actual, expected in [
                ('DOUBLE', '42', '42.000000'), ('FLOAT', '-0.0', '0'),
                ('INTEGER', '42', '4.2e1'), ('UTINYINT', '255', '255.000'),
                ('UHUGEINT', str(2**128-1), str(2**128-1)+'.0'),
                ('HUGEINT', str(-2**127), str(-2**127)+'.000'),
                ('DECIMAL(38,0)', '9'*38, '9'*38+'.0'),
                ('DECIMAL(4,4)', '.1250', '0.125'),
                ('DOUBLE', 'nan', 'NAN'), ('FLOAT', '-inf', '-infinity')]:
            # Even a T header does not override a returned numeric logical type.
            check_query(Record(1, ('query', 'T'), expected=(expected,)),
                        {'columns': [kind], 'rows': [[actual]]}, {})
        for kind, actual, expected in [
                ('VARCHAR', '42', '42.000000'), ('BOOLEAN', '1', '1.0'),
                ('BLOB', '42', '42.0'), ('DOUBLE', '42.1235', '42.12345'),
                ('INTEGER', '42', '43.0'), ('UTINYINT', '256', '256.0'),
                ('INTEGER', '1.2', '1.20'), ('DOUBLE', 'NULL', '0'),
                ('UTINYINT', '0', '-0.0'), ('UHUGEINT', '0.00', '-0'),
                ('', '42', '42.0'), ('UNKNOWN', '42', '42.0'),
                ('DOUBLE', 'nan', 'inf'), ('DOUBLE', 'inf', '-infinity'),
                ('DECIMAL(4,2)', '999', '999.0'),
                ('DECIMAL(4,2)', '.123', '0.1230'),
                ('DECIMAL(0,0)', '0', '0.0'),
                ('INTEGER', '1e100000', '10e99999')]:
            with self.assertRaises(AssertionError, msg=(kind, actual, expected)):
                check_query(Record(1, ('query', 'R'), expected=(expected,)),
                            {'columns': [kind], 'rows': [[actual]]}, {})

    def test_numeric_fallback_preserves_text_regex_hash_and_column_association(self):
        check_query(Record(1, ('query', 'II'), expected=('42.000\t42',)),
                    {'columns': ['DOUBLE', 'VARCHAR'], 'rows': [['42', '42']]}, {})
        for record, response in [
                (Record(1, ('query', 'RI'), expected=('42\t42.000',)),
                 {'columns': ['DOUBLE', 'VARCHAR'], 'rows': [['42', '42']]}),
                (Record(1, ('query', 'R'), expected=(r'<REGEX>:42\.0',)),
                 {'columns': ['DOUBLE'], 'rows': [['42']]}),
                (Record(1, ('query', 'R'), expected=(hash_values(['42.0']),)),
                 {'columns': ['DOUBLE'], 'rows': [['42']]}),
                (Record(1, ('query', 'RR', 'valuesort'), expected=('1.0', '2.0')),
                 {'columns': ['DOUBLE', 'VARCHAR'], 'rows': [['2', '1']]})]:
            with self.assertRaises(AssertionError):
                check_query(record, response, {})
        check_query(Record(1, ('query', 'RR', 'valuesort'), expected=('1.0', '2.0')),
                    {'columns': ['DOUBLE', 'DOUBLE'], 'rows': [['2', '1']]}, {})

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

    def test_recognized_not_implemented_rejection_keeps_exact_error_assertion(self):
        source = parse("statement error\nSELECT incompatible\n----\n"
                       "Not implemented Error: recognized rejection\n")
        response = {"ok": False, "unsupported": False,
                    "message": "Not implemented Error: recognized rejection"}
        runner = Runner(RecordingEngine([response]))
        runner.run(source)
        self.assertEqual(runner.passed, 1)
        for message in ["Not implemented: recognized rejection",
                        "Not Implemented Error: recognized rejection",
                        "Not implemented Error: a different rejection"]:
            with self.assertRaises(AssertionError):
                Runner(RecordingEngine([{**response, "message": message}])).run(source)
        for message in [response["message"], "Not implemented: missing capability"]:
            runner = Runner(RecordingEngine([{**response, "unsupported": True,
                                             "message": message}]))
            with self.assertRaises(Unsupported):
                runner.run(source)
            self.assertEqual(runner.passed, 0)

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


class ReferenceCorpusTests(unittest.TestCase):
    def verify(self, source, command):
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory) / "corpus.test"
            corpus.write_text(source)
            return verify_corpus(corpus, [("rust", "a"), ("cpp", "b")], command)

    def test_both_engines_must_satisfy_values_cardinality_and_width(self):
        source = "query I\nSELECT 7\n----\n7\n"
        calls = []
        def command(engine, path, sql, **options):
            calls.append((engine, path))
            return [{"c0": 7}]
        self.assertEqual(len(self.verify(source, command)), 1)
        self.assertEqual(calls, [("rust", "a"), ("cpp", "b")])
        for wrong in [[], [{"c0": 8}], [{"c0": 7}, {"c0": 7}], [{"c0": 7, "extra": 8}]]:
            with self.assertRaises(AssertionError):
                self.verify(source, lambda engine, *args, **kwargs: [{"c0": 7}] if engine == "rust" else wrong)

    def test_tabs_hash_marks_and_null_values_are_preserved(self):
        self.verify("query T\nSELECT '#a'\n----\n#a\n", lambda *args, **kwargs: [{"c0": "#a"}])
        self.verify("query T\nSELECT 'a\tb'\n----\na\tb\n", lambda *args, **kwargs: [{"c0": "a\tb"}])
        self.verify("query TI\nSELECT NULL,1\n----\nNULL\t1\n", lambda *args, **kwargs: [{"c0": None, "c1": 1}])

    def test_missing_errors_and_unsupported_controls_fail(self):
        with self.assertRaises(AssertionError):
            self.verify("statement error\nSELECT missing\n", lambda *args, **kwargs: "")
        for source in ["require parquet\n", "query I custom_mode\nSELECT 7\n----\n7\n"]:
            with self.assertRaises(AssertionError):
                self.verify(source, lambda *args, **kwargs: [{"c0": 7}])

    def test_continuation_records_failures_and_still_fails_the_corpus(self):
        with tempfile.TemporaryDirectory() as directory:
            corpus = Path(directory) / "corpus.test"
            corpus.write_text("query I\nSELECT 1\n----\n1\n\nquery I\nSELECT 7\n----\n7\n")
            for fail_fast, statuses in [(True, [False]), (False, [False, True])]:
                outcomes = []
                with self.assertRaises(AssertionError):
                    verify_corpus(corpus, [("cpp", "a")],
                                  lambda *args, **kwargs: [{"c0": 7}],
                                  outcomes, fail_fast=fail_fast)
                self.assertEqual([r["passed"] for r in outcomes], statuses)
                self.assertIn("cpp", outcomes[0]["error"]["message"])


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
