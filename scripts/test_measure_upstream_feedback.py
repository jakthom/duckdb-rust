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
            workload = {"id": "one", "path": "test/a.test"}
            report.write_text(json.dumps({"kind": "duckdb-rust-selected-feedback", "version": 1, "status": "passed", "target": "development", "revision": "r", "workload_id": "one", "sample_id": "sample", "path": "test/a.test", "declarations": 3,
                                          "passed_records": 3, "skipped_records": 0, "generated_records": 0, "source_sha256": "source", "suite_sha256": "suite", "token_sha256": "token", "runner_binary_sha256": "runner"}))
            self.assertEqual(feedback.upstream_verdict(report, "development", "r", workload, "sample", "source", "suite", "token", "runner"), 3)
            report.write_text(report.read_text().replace('"passed"', '"failed"'))
            with self.assertRaises(ValueError): feedback.upstream_verdict(report, "development", "r", workload, "sample", "source", "suite", "token", "runner")

    def test_rust_report_rejects_tampered_cache_or_wrong_selection(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "result.json"
            report.write_text(json.dumps({"version": 1, "status": "passed", "path": "test/other.test"}))
            with self.assertRaises(ValueError):
                feedback.upstream_verdict(report, "release", "r", {"id": "one", "path": "test/a.test"}, "sample", "source", "suite", "token", "runner")

    def test_timed_failure_keeps_raw_process_diagnostic(self):
        details = {"command": ["runner", "case"], "returncode": 1, "stdout": "wrong",
                   "stderr": "resource evidence"}
        self.assertEqual(feedback.failure_diagnostic(measure.SampleFailure("failed", details)), details)
        self.assertIsNone(feedback.failure_diagnostic(ValueError("other failure")))

    def test_snapshot_rehashes_every_selected_cache_entry(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); workloads = root / "workloads.json"; workloads.write_text("{}")
            runner = root / "runner"; runner.write_bytes(b"runner")
            sidecar = root / "runner.provenance.json"; sidecar.write_text("{}")
            prepared = {}
            for workload in ("one", "two"):
                source = root / workload / "source"; source.mkdir(parents=True)
                path = "test/a.test"; file = source / path; file.parent.mkdir(); file.write_text(workload)
                suite = source.parent / "suite.json"; suite.write_text(workload)
                prepared[(workload, "release")] = (source, suite, feedback.digest(file), feedback.digest(suite), {"id": workload}, path)
            args = type("Args", (), {"workloads": workloads, "rust_provenance": sidecar})()
            with patch.object(feedback, "checked_worker_provenance", return_value=(sidecar, {"profile": "release"})):
                snapshot = feedback.campaign_snapshot(args, runner, {}, prepared)
                self.assertEqual(set(snapshot["caches"]), {"one/release", "two/release"})
                (root / "two/source/test/a.test").write_text("drift")
                with self.assertRaises(ValueError):
                    feedback.campaign_snapshot(args, runner, {}, prepared)


if __name__ == "__main__": unittest.main()
