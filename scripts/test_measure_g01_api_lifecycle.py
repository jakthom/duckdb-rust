import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import measure_g01_api_lifecycle as measure


def sample(value=2, **overrides):
    result = {
        "command": ["candidate"],
        "returncode": 0,
        "stdout": measure.MARKER + "\n",
        "stderr": "time evidence",
        "wall_ns": value,
        "cpu_ns": value,
        "max_rss_bytes": value,
        "block_input": 0,
        "block_output": 0,
    }
    result.update(overrides)
    return result


class GateTests(unittest.TestCase):
    def reports(self, rust_value=2):
        cpp = {"release": [sample(2)] * 9, "development": [sample(3)] * 9}
        rust = {
            "release": [sample(rust_value)] * 9,
            "development": [sample(rust_value)] * 9,
        }
        return cpp, rust

    def test_marker_and_faster_reference_gate(self):
        cpp, rust = self.reports()
        result = measure.gate(cpp, rust)
        self.assertTrue(result["passed"])
        self.assertTrue(result["at_parity_or_better_performance"])

    def test_requires_complete_raw_nine_sample_populations(self):
        cpp, rust = self.reports()
        rust["release"] = rust["release"][:8]
        with self.assertRaisesRegex(ValueError, "exactly 9"):
            measure.gate(cpp, rust)
        cpp, rust = self.reports()
        rust["release"][0] = {"wall_ns": 1}
        with self.assertRaisesRegex(ValueError, "incomplete"):
            measure.gate(cpp, rust)

    def test_metric_and_throughput_regression_fail(self):
        cpp, rust = self.reports(4)
        result = measure.gate(cpp, rust)
        self.assertFalse(result["passed"])
        self.assertFalse(result["at_parity_or_better_performance"])

    def test_manifest_is_bound_to_mapping(self):
        path = Path(__file__).parents[1] / "test/performance/g01_api_lifecycle_manifest.json"
        self.assertEqual(measure.manifest(path)["data"]["samples"], 9)
        with tempfile.TemporaryDirectory() as directory:
            changed = Path(directory) / "manifest.json"
            data = json.loads(path.read_text())
            data["source_ids"]["development"] = "wrong"
            changed.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "changed lifecycle"):
                measure.manifest(changed)

    def test_timed_rejects_wrong_marker_and_incomplete_metrics(self):
        bad = subprocess.CompletedProcess(["candidate"], 0, "wrong\n", "")
        with patch.object(measure.platform, "system", return_value="Darwin"):
            with self.assertRaises(measure.SampleFailure) as error:
                measure.timed(["candidate"], execute=lambda *args, **kwargs: bad)
        self.assertEqual(error.exception.observation["returncode"], 0)
        incomplete = subprocess.CompletedProcess(
            ["candidate"], 0, measure.MARKER + "\n", "0.01 real 0.00 user 0.00 sys\n"
        )
        with patch.object(measure.platform, "system", return_value="Darwin"):
            with self.assertRaisesRegex(measure.SampleFailure, "missing max_rss"):
                measure.timed(["candidate"], execute=lambda *args, **kwargs: incomplete)

    def test_quiet_host_ignores_measurement_ancestors(self):
        processes = (
            "10 20 python measure.py\n"
            "20 1 shell python measure.py\n"
            "30 1 cargo build --release\n"
        )
        with patch.object(measure.os, "getpid", return_value=10), \
             patch.object(measure.os, "getppid", return_value=20):
            self.assertEqual(
                measure.active_peers(check_output=lambda *args, **kwargs: processes),
                ["cargo build --release"],
            )


if __name__ == "__main__":
    unittest.main()
