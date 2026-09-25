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

    def test_campaign_populates_per_workload_prepared_caches_before_timing_rust(self):
        """Exercise the main campaign loop that owns the per-workload cache map."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_root = root / "source"
            source_root.mkdir()
            rust = root / "target/release/sqllogictest"
            rust.parent.mkdir(parents=True)
            rust.write_text("runner")
            sidecar = Path(str(rust) + ".provenance.json")
            sidecar.write_text("{}")
            scripts = root / "scripts"
            scripts.mkdir()
            for name in ("measure_upstream_feedback.py", "run_upstream.py", "upstream_suite.py"):
                (scripts / name).write_text(name)
            report = root / "report.json"
            workloads = root / "workloads.json"
            workloads.write_text("{}")
            binaries, builds = {}, {}
            for target in ("release", "development"):
                build = root / target / "build"
                binary = build / "test/unittest"
                binary.parent.mkdir(parents=True)
                binary.write_text(target)
                (build / "CMakeCache.txt").write_text(
                    "CMAKE_BUILD_TYPE:STRING=Release\n"
                    f"CMAKE_HOME_DIRECTORY:INTERNAL={source_root}\n"
                )
                binaries[target], builds[target] = binary, build
            args = type("Args", (), {
                "workloads": workloads, "rust": rust, "rust_provenance": sidecar,
                "release_cpp": binaries["release"], "development_cpp": binaries["development"],
                "release_build": builds["release"], "development_build": builds["development"],
                "suite_cache": root / "cache", "report": report, "samples": 9, "warmups": 3,
                "timeout": 1,
            })()
            target_config = {target: type("Target", (), {"source": source_root, "binary": rust,
                                                            "build": builds[target]})()
                             for target in ("release", "development")}
            cache_root = root / "selected"
            def populate(target, paths, cache):
                selected = cache_root / target / "source"
                selected.mkdir(parents=True, exist_ok=True)
                (selected / "test").mkdir(exist_ok=True)
                (selected / paths[0]).write_text(target)
                (selected.parent / "suite.json").write_text(target)
                return selected, {}, {"revision": target + "-revision"}
            timed_rust = []
            def record_rust(*call_args):
                timed_rust.append(call_args)
                return {"wall_ns": 1}
            with patch.object(feedback.argparse.ArgumentParser, "parse_args", return_value=args), \
                 patch.object(feedback, "ROOT", root), \
                 patch.object(feedback, "TARGETS", target_config), \
                 patch.object(feedback, "validate_manifest", return_value=[{"id": "one", "path": "test/a.test"}]), \
                 patch.object(feedback.measure, "active_peers", return_value=[]), \
                 patch.object(feedback, "checked_worker_provenance", return_value=(sidecar, {"binary_sha256": "runner", "source_sha256": "source"})), \
                 patch.object(feedback, "require_checkout", side_effect=lambda source, target: target + "-revision"), \
                 patch.object(feedback, "require_reference", return_value=(rust, "cli")), \
                 patch.object(feedback, "selected_feedback_population", side_effect=populate), \
                 patch.object(feedback, "campaign_snapshot", return_value={"snapshot": "stable"}), \
                 patch.object(feedback.measure, "run_timed", return_value={"wall_ns": 1}), \
                 patch.object(feedback, "timed_rust", side_effect=record_rust), \
                 patch.object(feedback, "gate", return_value={"passed": True}):
                with self.assertRaises(SystemExit) as exit:
                    feedback.main()
            self.assertEqual(exit.exception.code, 0)
            self.assertEqual(len(timed_rust), 24)  # two targets across 3 warmups + 9 samples
            self.assertTrue(report.is_file())


if __name__ == "__main__": unittest.main()
