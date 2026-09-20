"""Mutation contracts for H1 reports and the fastest-reference gate."""
import importlib.util
import json
import tempfile
import unittest
from unittest import mock
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("h1", Path(__file__).with_name("measure_h1_filesystem.py"))
h1 = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(h1)

def observation(elapsed=10, wall=11):
    return {"ok": True, "returncode": 0, "wall_ns": wall, "cpu_ns": 2, "max_rss_bytes": 3, "block_input": 4, "block_output": 5, "payload": {"operation": "sequential-read", "bytes": 64, "elapsed_ns": elapsed, "source_id": "abcdef0"}}
def report(target, rust_elapsed=10):
    cpp = [observation() for _ in range(h1.SAMPLES)]; rust = [observation(elapsed=rust_elapsed) for _ in range(h1.SAMPLES)]
    row = {"name": "read", "operation": "sequential-read", "cpp": {"warmups": [observation() for _ in range(h1.WARMUPS)], "samples": cpp}, "rust": {"warmups": [observation() for _ in range(h1.WARMUPS)], "samples": rust}}
    row["summary"] = {"cpp": h1.summarize(cpp), "rust": h1.summarize(rust)}
    receipt = {"schema": "h1-prepared-v4", "target": target, "cpp_revision": "abcdef0123456789", "rust_worker": {"path": "/worker", "sha256": "worker"}, "rust_inputs": {"a": {"sha256": "a"}}, "rust_source_digest_after_build": "source", "adapter": {"sha256": "adapter"}, "workloads": {"sha256": "workloads"}}
    return {"schema": "h1-process-v4", "prepared": receipt, "warmups": h1.WARMUPS, "samples": h1.SAMPLES, "metrics": h1.METRICS, "source_digest_after_measure": "source", "workloads": [row], "passed": True}
class GateMutations(unittest.TestCase):
    def test_publication_fixture_requires_a_real_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "0"
            expected = h1.fixture(path, 64, "publication")
            names = {item.name for item in path.parent.iterdir()}
            self.assertNotEqual(path.read_bytes(), expected)
            with self.assertRaisesRegex(ValueError, "result mismatch"):
                h1.validate_file_effect(path, expected, "publication", names)
            path.write_bytes(expected)
            h1.validate_file_effect(path, expected, "publication", names)
    def test_cleanup_requires_original_bytes_and_no_staged_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "0"
            expected = h1.fixture(path, 64, "publication-cleanup")
            names = {item.name for item in path.parent.iterdir()}
            h1.validate_file_effect(path, expected, "publication-cleanup", names)
            path.with_suffix(".h1-stage").write_bytes(expected)
            with self.assertRaisesRegex(ValueError, "staged file"):
                h1.validate_file_effect(path, expected, "publication-cleanup", names)
    def test_cycle_pattern_keeps_original_bytes(self):
        for size in (1, 255, 256, 257, 1000):
            self.assertEqual(h1.pattern(size), bytes((index * 31 + 7) & 255 for index in range(size)))
    def gate(self, release, development):
        with tempfile.TemporaryDirectory() as directory:
            paths = [Path(directory) / name for name in ("release.json", "development.json")]
            paths[0].write_text(json.dumps(release)); paths[1].write_text(json.dumps(development))
            with mock.patch.object(h1, "verify_receipt"), mock.patch.object(h1, "validate_workloads", return_value={"bytes":64,"operations":[{"name":"read","operation":"sequential-read"}]}):
                return h1.gate(type("Args", (), {"reports": paths, "report": Path(directory) / "gate.json"})())
    def test_zero_reference_resource_does_not_hide_rust_cost(self):
        self.assertEqual(h1.ratio(0, 0), 1); self.assertEqual(h1.ratio(1, 0), float("inf"))
    def test_gate_rejects_empty_population(self):
        bad = report("release"); bad["workloads"] = []
        with self.assertRaises(ValueError): self.gate(bad, report("development"))
    def test_gate_rejects_fabricated_summary(self):
        bad = report("release"); bad["workloads"][0]["summary"]["rust"]["inner_latency_ns"] = 0
        with self.assertRaises(ValueError): self.gate(bad, report("development"))
    def test_gate_rejects_changed_rust_input_identity(self):
        changed = report("development"); changed["prepared"]["rust_inputs"] = {"a": {"sha256": "different"}}
        with self.assertRaises(ValueError): self.gate(report("release"), changed)
    def test_gate_rejects_self_consistent_wrong_payload_size(self):
        bad = report("development")
        for engine in ("rust", "cpp"):
            for phase in ("warmups", "samples"):
                for sample in bad["workloads"][0][engine][phase]: sample["payload"]["bytes"] = 1
        with self.assertRaises(ValueError): self.gate(report("release"), bad)
    def test_gate_rejects_unknown_operation(self):
        bad = report("development"); bad["workloads"][0]["operation"] = "different-read"
        with self.assertRaises(ValueError): self.gate(report("release"), bad)
    def test_empty_runtime_source_id_is_rejected(self):
        sample = observation(); sample["payload"]["source_id"] = ""
        with self.assertRaises(ValueError): h1.validate_observation(sample, {"operation":"sequential-read"},64,"cpp",{"cpp_revision":"abcdef0123456789"})
    def test_gate_checks_each_rust_population_against_fastest_pin(self):
        with self.assertRaises(SystemExit) as outcome: self.gate(report("release"), report("development", rust_elapsed=20))
        self.assertEqual(outcome.exception.code, 1)
if __name__ == "__main__": unittest.main()
