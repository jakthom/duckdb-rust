"""Focused reporting and retry-selection tests for the G01 SQL campaign."""
import tempfile
import unittest
from pathlib import Path
import stat

import sqllogic
from run_upstream import failure_class, run_case, selected_entries, summarize


class RunUpstreamTests(unittest.TestCase):
    def test_failure_classes_do_not_turn_oracle_or_engine_blocks_into_passes(self):
        self.assertEqual(failure_class(sqllogic.Unsupported("test directive require: parquet")), "harness_directive_or_oracle")
        self.assertEqual(failure_class(sqllogic.Unsupported("unimplemented SQL function"), True), "engine_unsupported")
        self.assertEqual(failure_class(AssertionError("wrong error")), "assertion_or_error_mismatch")
        self.assertEqual(failure_class(TimeoutError("deadline")), "timeout")
        self.assertEqual(failure_class(RuntimeError("worker exited: signal 11")), "crash")
        self.assertEqual(failure_class(UnicodeDecodeError("utf8", b"", 0, 1, "bad"), phase="parse"), "harness_parse")
        self.assertEqual(failure_class(BrokenPipeError()), "crash")

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

    def test_retry_selection_uses_only_prior_timeouts(self):
        sql = [{"id": "a", "path": "a.test"}, {"id": "b", "path": "b.test"}]
        with tempfile.TemporaryDirectory() as d:
            report = Path(d) / "prior.json"
            report.write_text('{"populations":{"development":{"results":[{"path":"b.test","failure_class":"timeout"}]}}}')
            self.assertEqual(selected_entries(sql, [], None, report, "development"), [sql[1]])

    def test_path_list_rejects_unknown_ids(self):
        with tempfile.TemporaryDirectory() as d:
            paths = Path(d) / "retry.txt"; paths.write_text("missing.test\n")
            with self.assertRaises(ValueError): selected_entries([{"id": "a", "path": "a.test"}], [], paths)

    def test_run_case_counts_sent_sql_and_source_reach_honestly(self):
        with tempfile.TemporaryDirectory() as d:
            source, worker = Path(d) / "source", Path(d) / "worker"
            source.mkdir(); (source / "fail.test").write_text("statement ok\nFAIL\n\nstatement ok\nTAIL\n")
            (source / "loop.test").write_text("loop x 0 2\nstatement ok\nOK\n\nendloop\n")
            (source / "restart.test").write_text("restart\nstatement ok\nOK\n")
            worker.write_text("#!/usr/bin/env python3\nimport json,sys\nfor line in sys.stdin:\n r=json.loads(line); ok='FAIL' not in r.get('sql',''); print(json.dumps({'ok':ok,'message':'failure'}),flush=True)\n")
            worker.chmod(worker.stat().st_mode | stat.S_IXUSR)
            failed = run_case(worker, source, {"id":"f","path":"fail.test"}, 2)
            self.assertEqual((failed["attempted_records"], failed["worker_requests"], failed["unreached_source_records"]), (1, 1, 1))
            loop = run_case(worker, source, {"id":"l","path":"loop.test"}, 2)
            self.assertIsNone(loop["unreached_source_records"])
            restarted = run_case(worker, source, {"id":"r","path":"restart.test"}, 2)
            self.assertEqual((restarted["attempted_records"], restarted["worker_requests"]), (1, 2))


if __name__ == "__main__":
    unittest.main()
