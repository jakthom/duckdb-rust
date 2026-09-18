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
    value = {"command": ["runner", "a.test"], "returncode": 0, "stdout": "PASS", "stderr": "",
             "wall_ns": 100, "cpu_ns": 100, "max_rss_bytes": 100,
             "block_input": 1, "block_output": 1, "records": 2}
    value.update(overrides)
    return value


class PerformanceGateTests(unittest.TestCase):
    def reports(self):
        workloads = [{"id": "a", "path": "a.test", "kind": "custom_comparable",
                      "shared_test_dir": "/shared",
                      "shared_workload_path": "/shared/a.test",
                      "shared_workload_sha256": "x", "shared_workload_bytes": 1}]
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

    def test_serialized_raw_evidence_recomputes_gate(self):
        workloads, release, development = self.reports()
        report = {"samples": 9, "execution_configuration": measure.SERIAL_CONFIGURATION, "workloads": [{**workloads[0], "observations": {
            "release": release["workloads"][0]["cpp"],
            "development": development["workloads"][0]["cpp"],
            "rust": release["workloads"][0]["rust"],
        }}]}
        for target in ("release", "development"):
            for observation in report["workloads"][0]["observations"][target]:
                observation["command"].append("--single-threaded")
        serialized = json.loads(json.dumps(report))
        result = measure.gate_report(serialized, workloads)
        self.assertTrue(result["passed"])
        self.assertIn("stdout", serialized["workloads"][0]["observations"]["rust"][0])
        self.assertEqual(result, measure.gate_report(serialized, workloads))

    def test_raw_summary_or_partial_observation_fails_closed(self):
        workloads, release, development = self.reports()
        report = {"samples": 9, "execution_configuration": measure.SERIAL_CONFIGURATION, "workloads": [{**workloads[0], "observations": {
            "release": release["workloads"][0]["cpp"],
            "development": development["workloads"][0]["cpp"],
            "rust": release["workloads"][0]["rust"],
        }}]}
        report["workloads"][0]["observations"]["rust"][0].pop("stdout")
        with self.assertRaises(ValueError):
            measure.gate_report(report, workloads)

    def test_campaign_serializes_observations_without_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workload = root / "a.test"
            workload.write_text("query I\nSELECT 1\n----\n1\n")
            manifest = root / "workloads.json"
            manifest.write_text(json.dumps({"workloads": [{"id": "a", "path": "a.test"}]}))
            for name in ("release", "development", "rust"):
                (root / name).write_text(name)
            args = type("Args", (), {
                "report": root / "report.json", "samples": 9, "warmups": 3,
                "workloads": manifest, "test_root": root, "release_cpp": root / "release",
                "development_cpp": root / "development", "rust": root / "rust",
                "release_source": root, "development_source": root, "release_build": root,
                "development_build": root, "release_cli": root / "release", "development_cli": root / "development",
            })()
            def timed(command, label):
                value = sample(command=[str(part) for part in command])
                if command[0] == args.rust:
                    value.update(wall_ns=90, cpu_ns=90, max_rss_bytes=90)
                return value
            with patch.object(measure, "active_peers", return_value=[]), \
                 patch.object(measure, "identity", side_effect=lambda target, *unused: {"target": target}), \
                 patch.object(measure, "source_digest", return_value="source"), \
                 patch.object(measure, "run_timed", side_effect=timed):
                report = measure.run_campaign(args)
            disk = json.loads(args.report.read_text())
            self.assertTrue(report["passed"])
            self.assertEqual(report["gate"], measure.gate_report(disk, measure.validate_manifest(manifest, root)))
            self.assertEqual(set(disk["workloads"][0]["observations"]), {"release", "development", "rust"})
            self.assertNotEqual(disk["workloads"], disk["gate"]["workloads"])
            observations = disk["workloads"][0]["observations"]
            shared_root = str(root.resolve())
            self.assertEqual(observations["release"][0]["command"][1:4],
                             ["--test-dir", shared_root, "a.test"])
            self.assertEqual(observations["development"][0]["command"][1:4],
                             ["--test-dir", shared_root, "a.test"])
            self.assertEqual(observations["rust"][0]["command"][1:], [shared_root, "a.test"])

    def test_serial_cpp_flag_and_metadata_fail_closed(self):
        workloads, release, development = self.reports()
        report = {"samples": 9, "execution_configuration": measure.SERIAL_CONFIGURATION,
                  "workloads": [{**workloads[0], "observations": {
                      "release": release["workloads"][0]["cpp"],
                      "development": development["workloads"][0]["cpp"],
                      "rust": release["workloads"][0]["rust"],
                  }}]}
        for target in ("release", "development"):
            for observation in report["workloads"][0]["observations"][target]:
                observation["command"].append("--single-threaded")
        self.assertTrue(measure.gate_report(report, workloads)["passed"])
        report["workloads"][0]["observations"]["release"][0]["command"].remove("--single-threaded")
        with self.assertRaisesRegex(ValueError, "thread controls"):
            measure.gate_report(report, workloads)
        report.pop("execution_configuration")
        with self.assertRaisesRegex(ValueError, "serial execution"):
            measure.gate_report(report, workloads)

    def test_serial_replay_rejects_extra_and_equals_thread_controls(self):
        workloads, release, development = self.reports()
        def report():
            result = {"samples": 9, "execution_configuration": measure.SERIAL_CONFIGURATION,
                      "workloads": [{**workloads[0], "observations": {
                          "release": copy.deepcopy(release["workloads"][0]["cpp"]),
                          "development": copy.deepcopy(development["workloads"][0]["cpp"]),
                          "rust": copy.deepcopy(release["workloads"][0]["rust"]),
                      }}]}
            for target in ("release", "development"):
                for observation in result["workloads"][0]["observations"][target]:
                    observation["command"].append("--single-threaded")
            return result
        for target, flag in (("release", "--threads"), ("development", "--threads=4"),
                             ("rust", "--threads"), ("rust", "--single-threaded")):
            with self.subTest(target=target, flag=flag):
                value = report()
                observation = value["workloads"][0]["observations"][target][0]
                observation["command"].extend([flag, "4"] if flag == "--threads" else [flag])
                with self.assertRaisesRegex(ValueError, "thread controls"):
                    measure.gate_report(value, workloads)

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

    def test_manifest_rejects_unhashed_fixture_resolution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "workloads.json"
            manifest.write_text(json.dumps({"workloads": [{"id": "a", "path": "a.test"}]}))
            for directive in ("include another.test", "load {TEST_DIR}/db.duckdb",
                              "unzip test/data/archive.gz", "<FILE>:expected.csv"):
                (root / "a.test").write_text(directive + "\n")
                with self.subTest(directive=directive), self.assertRaises(ValueError):
                    measure.validate_manifest(manifest, root)

    def test_wrong_marker_nonzero_and_zero_record_fail(self):
        with self.assertRaises(ValueError):
            measure.records_from_output("PASS a.test (0 records)\n0 records passed; 0 skipped\n", "rust")
        with self.assertRaises(ValueError):
            measure.records_from_output("No tests ran\n", "cpp")
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
                measure.identity("release", __file__, Path("."), Path("."), Path("x"), Path("."), [])
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "old.json"
            report.write_text("old")
            args = type("Args", (), {"report": report})()
            with self.assertRaises(FileExistsError):
                measure.run_campaign(args)

    def test_identity_records_pin_build_test_directory_and_shared_workload(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            build = root / "build"
            test_root = root / "tests"
            source.mkdir()
            build.mkdir()
            test_root.mkdir()
            (build / "test").mkdir()
            cpp = build / "test/unittest"
            cli = build / "duckdb"
            cpp.write_text("cpp")
            cli.write_text("cli")
            (build / "CMakeCache.txt").write_text("CMAKE_BUILD_TYPE:STRING=Release\n")
            workloads = [{"id": "g11", "path": "g11.test", "sha256": "abc", "bytes": 7}]
            with patch.object(measure, "require_checkout", return_value="revision"), \
                 patch.object(measure, "require_reference", return_value=(cli, {"target": "release"})):
                result = measure.identity("release", cpp, source, build, cli, test_root, workloads)
            self.assertEqual(result["source_directory"], str(source.resolve()))
            self.assertEqual(result["build_directory"], str(build.resolve()))
            self.assertEqual(result["test_directory"], str(test_root.resolve()))
            self.assertEqual(result["shared_workloads"], workloads)
            foreign = root / "foreign-unittest"
            foreign.write_text("foreign")
            with patch.object(measure, "require_checkout", return_value="revision"), \
                 patch.object(measure, "require_reference", return_value=(cli, {"target": "release"})):
                with self.assertRaisesRegex(ValueError, "pinned build"):
                    measure.identity("release", foreign, source, build, cli, test_root, workloads)


if __name__ == "__main__":
    unittest.main()
