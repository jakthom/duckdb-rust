import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import measure_b1_views as measure


def phase(name, value=100):
    return {"command": ["duckdb", "file.duckdb", "-json", "-c", "SELECT 1"], "phase": name,
            "returncode": 0, "stdout": "[]", "stderr": "resource", "wall_ns": value,
            "cpu_ns": value, "max_rss_bytes": value, "block_input": 1, "block_output": 1}


def sample(value=100):
    phases = [phase(name, value) for name in measure.PHASES]
    return {"phases": phases,
            "aggregate": {"wall_ns": value * 4, "cpu_ns": value * 4, "max_rss_bytes": value,
                          "block_input": 4, "block_output": 4},
            "artifact_before": {"seed_sha256": "a", "sizes": {"": 1, ".wal": 0}},
            "artifact_after": {"sizes": {"": 2, ".wal": 0}, "delta_bytes": {"": 1, ".wal": 0}}}


class DurableAdapterTests(unittest.TestCase):
    def populations(self, rust=90):
        return {"release": [sample() for _ in range(21)], "development": [sample(95) for _ in range(21)],
                "rust": [sample(rust) for _ in range(21)]}

    def report(self):
        rows = []
        for mode in measure.EXPECTED["configurations"]:
            for workload in measure.EXPECTED["workloads"]:
                def contextual(target, value):
                    engine = {"kind": "rust" if target == "rust" else "cpp", "binary": target}
                    phases = []
                    for name, sql in measure.sqls(workload, mode).items():
                        stdout = "[]"
                        if name == "reopen_query": stdout = '[{"row_count":10000,"checksum":49995000}]'
                        if name == "readonly_verify": stdout = '[{"absent":0}]'
                        phases.append({**phase(name, value), "command": measure.command(engine, "file.duckdb", mode, sql, name == "readonly_verify"), "stdout": stdout})
                    return {"phases": phases, "aggregate": {"wall_ns": value * 4, "cpu_ns": value * 4, "max_rss_bytes": value, "block_input": 4, "block_output": 4}, "artifact_before": {"seed_sha256": "a", "sizes": {"": 1, ".wal": 0}}, "artifact_after": {"sizes": {"": 2, ".wal": 0}, "delta_bytes": {"": 1, ".wal": 0}}}
                rows.append({"mode": mode, "workload": workload,
                             "warmups": {name: [contextual(name, 100) for _ in range(3)] for name in ("release", "development", "rust")},
                             "observations": {"release": [contextual("release", 100) for _ in range(21)], "development": [contextual("development", 95) for _ in range(21)], "rust": [contextual("rust", 90) for _ in range(21)]}})
        return {"manifest": {"data": copy.deepcopy(measure.EXPECTED)}, "samples": 21, "warmups": 3, "results": rows}

    def test_gate_requires_every_metric_and_throughput(self):
        self.assertTrue(measure.gate(self.populations())["passed"])
        bad = self.populations()
        for row in bad["rust"]:
            for item in row["phases"]: item["block_output"] = 2
            row["aggregate"]["block_output"] = 8
        self.assertFalse(measure.gate(bad)["passed"])
        slow = self.populations(101)
        self.assertFalse(measure.gate(slow)["passed"])

    def test_gate_rejects_zero_missing_duplicate_and_tampered_populations(self):
        rows = self.populations(); rows.pop("rust")
        with self.assertRaises(ValueError): measure.gate(rows)
        rows = self.populations(); rows["rust"] = rows["rust"][:-1]
        with self.assertRaises(ValueError): measure.gate(rows)
        rows = self.populations(); rows["rust"][0]["aggregate"]["cpu_ns"] += 1
        with self.assertRaises(ValueError): measure.gate(rows)
        rows = self.populations(); rows["rust"][0]["phases"][0]["wall_ns"] = 0
        with self.assertRaises(ValueError): measure.gate(rows)

    def test_replay_rejects_manifest_result_and_population_tampering(self):
        report = self.report(); self.assertTrue(measure.replay(report)["passed"])
        report["results"].pop()
        with self.assertRaises(ValueError): measure.replay(report)
        report = self.report(); report["manifest"]["data"]["rows"] = 1
        with self.assertRaises(ValueError): measure.replay(report)
        report = self.report(); report["results"][0]["observations"]["rust"].pop()
        with self.assertRaises(ValueError): measure.replay(report)
        report = self.report(); report["results"][0]["observations"]["rust"][0]["phases"][0]["command"].append("--threads=4")
        with self.assertRaises(ValueError): measure.replay(report)

    def test_commands_are_symmetric_and_wal_disables_cpp_shutdown_checkpoint(self):
        cpp = {"kind": "cpp", "binary": "cpp"}; rust = {"kind": "rust", "binary": "rust"}
        self.assertIn("PRAGMA disable_checkpoint_on_shutdown", measure.command(cpp, "db", "wal", "SELECT 1")[-1])
        self.assertNotIn("PRAGMA", measure.command(cpp, "db", "checkpoint", "SELECT 1")[-1])
        self.assertIn("-readonly", measure.command(cpp, "db", "wal", "SELECT 1", True))
        self.assertIn("--durability", measure.command(rust, "db", "wal", "SELECT 1"))
        self.assertIn("--read-only", measure.command(rust, "db", "wal", "SELECT 1", True))
        self.assertNotIn("--durability", measure.command(rust, "db", "wal", "SELECT 1", True))
        self.assertEqual(list(measure.sqls("view_cycle", "checkpoint")), list(measure.PHASES))

    def test_json_verdict_rejects_non_numeric_metadata_and_extra_rows(self):
        measure.json_row('[{"row_count": 10000, "checksum": 49995000}]', {"row_count": 10000, "checksum": 49995000})
        for text in ('not json', '[{"row_count":"10000","checksum":49995000}]', '[{"row_count":10000,"checksum":49995000,"x":1}]', '[]'):
            with self.subTest(text=text):
                with self.assertRaises(ValueError): measure.json_row(text, {"row_count": 10000, "checksum": 49995000})

    def test_time_parser_and_failure_are_fail_closed(self):
        text = "0.01 real 0.00 user 0.01 sys\n9 maximum resident set size\n2 block input operations\n3 block output operations\n"
        self.assertEqual(measure.parse_time(text)["cpu_ns"], 10_000_000)
        with self.assertRaises(ValueError): measure.parse_time("0.01 real 0.00 user 0.01 sys\n")
        with patch.object(measure.platform, "system", return_value="Darwin"):
            with self.assertRaises(measure.SampleFailure):
                measure.timed(["bad"], "publish", execute=lambda *a, **k: subprocess.CompletedProcess(a[0], 1, "", "bad"))

    def test_output_directory_is_overwrite_protected(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "old"; output.mkdir()
            args = type("Args", (), {"output_dir": output})()
            with self.assertRaises(FileExistsError): measure.run_campaign(args)

    def test_manifest_is_exact_fixed_population(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "workloads.json"; path.write_text(json.dumps(measure.EXPECTED))
            self.assertEqual(measure.manifest(path)["data"], measure.EXPECTED)
            changed = copy.deepcopy(measure.EXPECTED); changed["samples"] = 9; path.write_text(json.dumps(changed))
            with self.assertRaises(ValueError): measure.manifest(path)


if __name__ == "__main__": unittest.main()
