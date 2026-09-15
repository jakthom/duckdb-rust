import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import measure_sqllogic_performance as measure


def sample(**overrides):
    value = {"wall_ns": 100, "cpu_ns": 100, "max_rss_bytes": 100, "block_input": 1, "block_output": 1, "records": 2}
    value.update(overrides)
    return value


class PerformanceGateTests(unittest.TestCase):
    def reports(self):
        workloads = [{"id": "a", "path": "a.test", "sha256": "x", "bytes": 1}]
        entry = {**workloads[0], "cpp": [sample() for _ in range(9)], "rust": [sample(wall_ns=90, cpu_ns=90, max_rss_bytes=90) for _ in range(9)], "cpp_records": 2, "rust_records": 2}
        return workloads, {"workloads": [copy.deepcopy(entry)]}, {"workloads": [copy.deepcopy(entry)]}

    def test_selects_faster_reference_and_passes_every_metric(self):
        workloads, release, development = self.reports()
        release["workloads"][0]["rust"] = [sample(wall_ns=70, cpu_ns=70, max_rss_bytes=70) for _ in range(9)]
        development["workloads"][0]["cpp"] = [sample(wall_ns=80, cpu_ns=80, max_rss_bytes=80) for _ in range(9)]
        development["workloads"][0]["rust"] = [sample(wall_ns=70, cpu_ns=70, max_rss_bytes=70) for _ in range(9)]
        result = measure.gate(release, development, workloads)
        self.assertTrue(result["passed"])
        self.assertEqual(result["workloads"][0]["cpp_fastest_medians"]["wall_ns"], 80)

    def test_any_metric_regression_fails(self):
        workloads, release, development = self.reports()
        release["workloads"][0]["rust"] = [sample(block_output=2) for _ in range(9)]
        self.assertFalse(measure.gate(release, development, workloads)["passed"])

    def test_missing_or_duplicate_workload_rejected(self):
        workloads, release, development = self.reports()
        with self.assertRaises(ValueError):
            measure.gate(release, development, workloads + workloads)
        release["workloads"].append(copy.deepcopy(release["workloads"][0]))
        with self.assertRaises(ValueError):
            measure.gate(release, development, workloads)

    def test_incomplete_sample_rejected(self):
        workloads, release, development = self.reports()
        release["workloads"][0]["rust"] = [sample()] * 8
        with self.assertRaises(ValueError):
            measure.gate(release, development, workloads)

    def test_manifest_rejects_bad_and_duplicate_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "a.test").write_text("statement ok\nSELECT 1\n")
            manifest = root / "workloads.json"
            manifest.write_text(json.dumps({"workloads": [{"id": "a", "path": "a.test"}, {"id": "a", "path": "a.test"}]}))
            with self.assertRaises(ValueError):
                measure.validate_manifest(manifest, root)
            manifest.write_text(json.dumps({"workloads": [{"id": "a", "path": "../nope.test"}]}))
            with self.assertRaises(ValueError):
                measure.validate_manifest(manifest, root)

    def test_wrong_marker_nonzero_and_zero_record_fail(self):
        with self.assertRaises(ValueError):
            measure.records_from_output("PASS a.test (0 records)\n0 records passed; 0 skipped\n", "rust")
        with patch.object(measure.platform, "system", return_value="Darwin"):
            with self.assertRaises(RuntimeError):
                measure.run_timed(["x"], "cpp", execute=lambda *args, **kwargs: subprocess.CompletedProcess(args[0], 2, "", "bad"))

    def test_macos_resource_parser_requires_every_field(self):
        stderr = "0.01 real 0.00 user 0.01 sys\n10 maximum resident set size\n0 block input operations\n0 block output operations\n"
        self.assertEqual(measure.parse_time(stderr)["cpu_ns"], 10_000_000)
        with self.assertRaises(ValueError):
            measure.parse_time("0.01 real 0.00 user 0.01 sys\n")

    def test_quiet_host_ignores_measurement_process_and_its_shell(self):
        processes = (
            "10 20 python measure.py --release-cpp unittest --rust sqllogictest\n"
            "20 1 shell python measure.py --release-cpp unittest\n"
            "30 1 cargo build --release\n"
        )
        with patch.object(measure.os, "getpid", return_value=10), \
             patch.object(measure.os, "getppid", return_value=20):
            self.assertEqual(
                measure.active_peers(check_output=lambda *args, **kwargs: processes),
                ["cargo build --release"],
            )

    def test_identity_failure_and_overwrite_rejected(self):
        with patch.object(measure, "require_checkout", side_effect=ValueError("wrong pin")):
            with self.assertRaises(ValueError):
                measure.identity("release", __file__, Path("."), Path("."), Path("x"))
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "old.json"
            report.write_text("old")
            args = type("Args", (), {"report": report})()
            with self.assertRaises(FileExistsError):
                measure.run_campaign(args)


if __name__ == "__main__":
    unittest.main()
