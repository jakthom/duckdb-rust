"""Focused reporting and retry-selection tests for the G01 SQL campaign."""
import tempfile
import unittest
from pathlib import Path

import sqllogic
from run_upstream import failure_class, selected_entries, summarize


class RunUpstreamTests(unittest.TestCase):
    def test_failure_classes_do_not_turn_oracle_or_engine_blocks_into_passes(self):
        self.assertEqual(failure_class(sqllogic.Unsupported("test directive require: parquet")), "harness_directive_or_oracle")
        self.assertEqual(failure_class(sqllogic.Unsupported("unimplemented SQL function"), True), "engine_unsupported")
        self.assertEqual(failure_class(AssertionError("wrong error")), "assertion_or_error_mismatch")
        self.assertEqual(failure_class(TimeoutError("deadline")), "timeout")
        self.assertEqual(failure_class(RuntimeError("worker exited: signal 11")), "crash")

    def test_selection_and_coverage_reject_missing_or_failed_ids(self):
        sql = [{"id": "a", "path": "a.test"}, {"id": "b", "path": "b.test"}]
        with tempfile.TemporaryDirectory() as d:
            paths = Path(d) / "retry.txt"; paths.write_text("b.test\n")
            self.assertEqual(selected_entries(sql, [], paths), [sql[1]])
        report = summarize(sql, sql, [{"id": "a", "status": "passed"}], [], {})
        self.assertFalse(report["sql_selection_passed"])
        self.assertFalse(report["sql_suite_passed"])
        report = summarize(sql, sql, [{"id": "a", "status": "passed"}, {"id": "b", "status": "failed", "failure_class": "engine_unsupported"}], [], {})
        self.assertFalse(report["sql_selection_passed"])
        self.assertEqual(report["failure_classes"], {"engine_unsupported": 1})


if __name__ == "__main__":
    unittest.main()
