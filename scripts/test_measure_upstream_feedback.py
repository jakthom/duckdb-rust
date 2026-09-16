import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import measure_upstream_feedback as feedback
import measure_sqllogic_performance as measure


class FeedbackPerformanceTests(unittest.TestCase):
    def test_manifest_requires_the_same_safe_source_path_in_both_pins(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "workloads.json"
            manifest.write_text(json.dumps({"workloads": [{"id": "one", "path": "test/a.test"}]}))
            with patch.object(feedback, "TARGETS", {"release": type("T", (), {"source": Path(directory)})(),
                                                      "development": type("T", (), {"source": Path(directory)})()}):
                (Path(directory) / "test").mkdir(); (Path(directory) / "test/a.test").write_text("ok")
                self.assertEqual(feedback.validate_manifest(manifest)[0]["source_sha256"].keys(), {"release", "development"})
            manifest.write_text(json.dumps({"workloads": [{"id": "one", "path": "../escape.test"}]}))
            with self.assertRaises(ValueError): feedback.validate_manifest(manifest)

    def test_rust_report_requires_exact_selected_passed_case(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "result.json"
            report.write_text(json.dumps({"worker_profile": "release", "stale_source": False,
                                          "populations": {"development": {"sql_files_selected": 1,
                                          "selected": [{"id": "test/a.test", "kind": "sqllogictest", "path": "test/a.test", "line": 1}],
                                          "results": [{"status": "passed", "passed_records": 3}]}}}))
            self.assertEqual(feedback.upstream_verdict(report, "development", "test/a.test"), 3)
            report.write_text(report.read_text().replace('"passed"', '"failed"'))
            with self.assertRaises(ValueError): feedback.upstream_verdict(report, "development", "test/a.test")

    def test_timed_failure_keeps_raw_process_diagnostic(self):
        details = {"command": ["runner", "case"], "returncode": 1, "stdout": "wrong",
                   "stderr": "resource evidence"}
        self.assertEqual(feedback.failure_diagnostic(measure.SampleFailure("failed", details)), details)
        self.assertIsNone(feedback.failure_diagnostic(ValueError("other failure")))


if __name__ == "__main__": unittest.main()
