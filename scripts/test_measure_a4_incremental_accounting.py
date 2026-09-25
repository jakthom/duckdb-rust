import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from measure_a4_incremental_accounting import _canonical_from_cpp, cpp_classifications, digest, gate, relations, require_identity, run_timed, validate_output
from generate_a4_incremental_accounting_fixture import write
from summarize_upstream import classify_incremental
import test_summarize_upstream as summary_tests


class A4MeasurementTests(unittest.TestCase):
    def test_process_sample_combines_clock_with_cpu_rss_and_zero_io(self):
        import subprocess
        stderr = "0.01 real 0.01 user 0.00 sys\n100 maximum resident set size\n0 block input operations\n0 block output operations\n"
        completed = subprocess.CompletedProcess([], 0, json.dumps({"rows": 4, "output_sha256": "output"}), stderr)
        with tempfile.TemporaryDirectory() as temporary, patch("measure_a4_incremental_accounting.platform.system", return_value="Darwin"), patch("measure_a4_incremental_accounting.subprocess.run", return_value=completed), patch("measure_a4_incremental_accounting.time.perf_counter_ns", side_effect=[100, 1100]):
            sample = run_timed(["worker"], Path(temporary) / "sample")
            self.assertEqual(sample["wall_ns"], 1000)
            self.assertEqual(sample["cpu_ns"], 10_000_000)
            self.assertEqual(sample["max_rss_bytes"], 100)
            self.assertEqual((sample["block_input"], sample["block_output"]), (0, 0))
            self.assertEqual(sample["rows_per_second"], 4_000_000)

    def reports(self, *, config="ordinary", stale=False):
        helper = summary_tests.SummaryTests()
        return (helper.incremental_report([{"path": "a.test", "status": "passed"}], config=config, stale=stale),
                helper.incremental_report([{"path": "a.test", "status": "failed"}], config=config))

    def test_small_and_large_fixtures_have_declared_population_and_immutable_hashes(self):
        with tempfile.TemporaryDirectory() as temporary:
            for count in (2, 10001):
                root = Path(temporary) / str(count)
                write(root, count)
                identities = json.loads((root / "fixture-identities.json").read_text())
                baseline, current = [json.loads((root / (name + ".json")).read_text()) for name in ("baseline", "current")]
                self.assertEqual(len(baseline["populations"]["development"]["results"]), count)
                self.assertEqual(len(current["populations"]["development"]["results"]), count - 1)
                for name, identity in identities.items():
                    self.assertEqual(require_identity(root / name, identity["sha256"]), identity["sha256"])
                result = classify_incremental(baseline, current)
                self.assertEqual((len(result["rows"]), result["fresh_failures"], result["lost_passes"]), (count * 2, 1, 2))
                self.assertEqual(result["counts"]["stale"], count)
                relations(baseline, current, root / "relations")

    def test_measure_rejects_fixture_or_output_mutation(self):
        baseline, current = self.reports()
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "fixture.json"; path.write_text(json.dumps(baseline)); expected = digest(path)
            path.write_text(json.dumps(current))
            with self.assertRaises(ValueError): require_identity(path, expected)
            with self.assertRaises(ValueError): validate_output({"output_sha256": expected}, path)

    def test_cpp_evidence_retains_stale_predecessor_without_claiming_fresh_failure(self):
        baseline, current = self.reports(stale=True)
        rows = classify_incremental(baseline, current)["rows"]
        key = (rows[0]["path"], rows[0]["pin"], rows[0]["runtime_configuration_sha256"])
        self.assertEqual(_canonical_from_cpp(baseline, current, {key: "stale"}), rows)
        self.assertFalse(rows[0]["fresh_failure"])
        self.assertEqual(rows[0]["stale_reasons"], ["stale_source"])

    def test_cpp_classifier_requires_pinned_attestation_and_rows(self):
        baseline, current = self.reports()
        with patch("measure_a4_incremental_accounting.subprocess.run") as run, tempfile.TemporaryDirectory() as temporary:
            run.return_value.stdout = "READY\tdeadbeef00\t1.0\nnot-a-row\n"
            with self.assertRaises(ValueError): cpp_classifications(Path("worker"), baseline, current, Path(temporary) / "one", "deadbeef00")
        with patch("measure_a4_incremental_accounting.subprocess.run") as run, tempfile.TemporaryDirectory() as temporary:
            run.return_value.stdout = "READY\tdeadbeef\t1.0\na.test\tdevelopment\tx\tfresh_failure\n"
            with self.assertRaises(ValueError): cpp_classifications(Path("worker"), baseline, current, Path(temporary) / "two", "other")

    def test_cpp_and_python_complete_output_match_without_fallback(self):
        baseline, current = self.reports(config="ordinary")
        expected = classify_incremental(baseline, current)["rows"]
        key = (expected[0]["path"], expected[0]["pin"], expected[0]["runtime_configuration_sha256"])
        self.assertEqual(_canonical_from_cpp(baseline, current, {key: "fresh_failure"}), expected)

    def test_independent_resource_and_throughput_gates_cannot_offset_regressions(self):
        from measure_sqllogic_performance import METRICS
        sample = {metric: 10 for metric in METRICS}
        sample.update(wall_ns=10, rows_per_second=100)
        samples = {name: [dict(sample) for _ in range(21)] for name in ("python", "release", "development")}
        self.assertTrue(gate(samples)["passed"])
        metric = "cpu_ns"
        for item in samples["python"]:
            item[metric] = 11
            item["wall_ns"] = 9
        self.assertFalse(gate(samples)["passed"])
        for item in samples["python"]:
            item[metric] = 10
            item["rows_per_second"] = 99
        self.assertFalse(gate(samples)["checks"]["throughput"])


if __name__ == "__main__": unittest.main()
