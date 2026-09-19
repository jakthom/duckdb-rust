import copy
from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import measure_sqllogic_proxy as proxy


def observation(command, target, relative, records, metric=100):
    if target == "proxy":
        stdout = f"PASS {relative} ({records} records)\n{records} records passed; 0 skipped\n"
    else:
        stdout = f"All tests passed ({records} assertions in 1 test case)\n"
    return {
        "command": list(command),
        "returncode": 0,
        "stdout": stdout,
        "stderr": "",
        "wall_ns": metric,
        "cpu_ns": metric,
        "max_rss_bytes": metric,
        "block_input": 1,
        "block_output": 1,
        "records": records,
    }


class ProxyEvidenceTests(unittest.TestCase):
    def context(self):
        root = "/frozen/root"
        workloads = []
        for item in proxy.EXPECTED_WORKLOADS:
            workloads.append(
                {
                    "id": item["id"],
                    "path": item["path"],
                    "kind": "custom_comparable",
                    "shared_test_dir": root,
                    "shared_workload_path": root + "/" + item["path"],
                    "shared_workload_sha256": item["sha256"],
                    "shared_workload_bytes": item["bytes"],
                    "proxy_records_expected": item["proxy_records"],
                }
            )
        campaign = {
            "test_root": root,
            "workloads": root + "/" + proxy.EXPECTED_MANIFEST,
            "worker": root + "/target/release/duckdb-rust-test-worker",
            "worker_provenance": root + "/target/release/duckdb-rust-test-worker.provenance.json",
            "release_cpp": "/release/test/unittest",
            "development_cpp": "/development/test/unittest",
            "release_source": "/release/source",
            "development_source": "/development/source",
            "release_build": "/release",
            "development_build": "/development",
            "release_cli": "/release/duckdb",
            "development_cli": "/development/duckdb",
        }
        return {
            "campaign": campaign,
            "workloads": workloads,
            "references": {
                "release": {"unittest": campaign["release_cpp"], "target": "release"},
                "development": {"unittest": campaign["development_cpp"], "target": "development"},
            },
            "worker": {
                "path": campaign["worker"],
                "sha256": "1" * 64,
                "provenance_path": campaign["worker_provenance"],
                "provenance_sha256": "2" * 64,
                "provenance": {"profile": "release", "source_sha256": "3" * 64, "binary_sha256": "1" * 64},
            },
            "python": {
                "executable": "/python",
                "binary_sha256": "4" * 64,
                "version": "test",
                "proxy": "/scripts/measure_sqllogic_proxy.py",
                "proxy_sha256": "5" * 64,
            },
            "inputs": {"frozen": {"path": "/input", "sha256": "6" * 64, "bytes": 1}},
        }

    def report(self, proxy_metric=90):
        context = self.context()
        entries = []
        for index, workload in enumerate(context["workloads"]):
            commands = proxy.expected_commands(context, workload)
            counts = {
                "release": 100 + index,
                "development": 200 + index,
                "proxy": workload["proxy_records_expected"],
            }
            warmups = {}
            measured = {}
            for target in proxy.RUNNERS:
                warmups[target] = []
                measured[target] = []
            schedule = proxy.expected_schedule()
            for phase, populations in (("warmup_observations", warmups), ("observations", measured)):
                for target in proxy.RUNNERS:
                    metric = proxy_metric if target == "proxy" else 100
                    for metadata in schedule[phase][target]:
                        sample = observation(
                            commands[target], target, workload["path"], counts[target], metric
                        )
                        sample.update(metadata)
                        populations[target].append(sample)
            entries.append(
                {
                    **workload,
                    "warmup_observations": warmups,
                    "observations": measured,
                    "pass_counts": counts,
                }
            )
        report = {
            "schema": proxy.SCHEMA,
            "recorded_at": "2026-09-19T00:00:00+00:00",
            "platform": "test-platform",
            "machine": "test-machine",
            "samples": proxy.ACCEPTANCE_SAMPLES,
            "warmups": proxy.ACCEPTANCE_WARMUPS,
            "campaign": copy.deepcopy(context["campaign"]),
            "requested_arguments": copy.deepcopy(context["campaign"]),
            "references": copy.deepcopy(context["references"]),
            "worker": copy.deepcopy(context["worker"]),
            "python": copy.deepcopy(context["python"]),
            "execution_configuration": copy.deepcopy(proxy.measure.SERIAL_CONFIGURATION),
            "proxy_configuration": copy.deepcopy(proxy.PROXY_CONFIGURATION),
            "inputs_before": copy.deepcopy(context["inputs"]),
            "inputs_after": copy.deepcopy(context["inputs"]),
            "order": "paired alternating release, development, proxy",
            "workloads": entries,
        }
        raw = {entry["id"]: entry["observations"] for entry in entries}
        release = {
            "workloads": [
                {**workload, "cpp": raw[workload["id"]]["release"], "rust": raw[workload["id"]]["proxy"]}
                for workload in context["workloads"]
            ]
        }
        development = {
            "workloads": [
                {**workload, "cpp": raw[workload["id"]]["development"], "rust": raw[workload["id"]]["proxy"]}
                for workload in context["workloads"]
            ]
        }
        report["gate"] = proxy.json_safe(
            proxy.measure.gate(release, development, context["workloads"])
        )
        report["passed"] = report["gate"]["passed"]
        report["at_parity_or_better_performance"] = report["gate"]["at_parity_or_better_performance"]
        return context, report

    def test_valid_report_recomputes_gate_without_equal_runner_count_units(self):
        context, report = self.report()
        gate = proxy.validate_report(report, context)
        self.assertTrue(gate["passed"])
        entry = report["workloads"][0]
        self.assertNotEqual(entry["pass_counts"]["release"], entry["pass_counts"]["proxy"])
        self.assertEqual(json.loads(json.dumps(report)), report)

    def test_commands_are_independently_derived_and_require_worker_attestation(self):
        context, report = self.report()
        for phase in ("warmup_observations", "observations"):
            for sample in report["workloads"][0][phase]["release"]:
                sample["command"] = ["mutated", "--serial"]
        with self.assertRaisesRegex(ValueError, "independently derived"):
            proxy.validate_report(report, context)
        context, report = self.report()
        command = report["workloads"][0]["observations"]["proxy"][0]["command"]
        self.assertEqual(
            command[command.index("--campaign-worker-sha256") + 1],
            context["worker"]["sha256"],
        )
        self.assertEqual(
            command[command.index("--campaign-source-sha256") + 1],
            context["worker"]["provenance"]["source_sha256"],
        )
        self.assertEqual(
            command[command.index("--campaign-provenance-sha256") + 1],
            context["worker"]["provenance_sha256"],
        )
        index = command.index("--worker-provenance")
        command[index:index + 2] = []
        with self.assertRaisesRegex(ValueError, "independently derived"):
            proxy.validate_report(report, context)

        context, report = self.report()
        command = report["workloads"][0]["observations"]["proxy"][0]["command"]
        index = command.index("--campaign-source-sha256") + 1
        command[index] = "0" * 64
        with self.assertRaisesRegex(ValueError, "independently derived"):
            proxy.validate_report(report, context)

    def test_campaign_attestation_checks_only_canonical_sidecar_and_exact_triple(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"
            worker.write_bytes(b"worker bytes are prechecked outside timing")
            provenance_path = Path(str(worker) + ".provenance.json")
            attestation = {
                "worker_sha256": "1" * 64,
                "source_sha256": "2" * 64,
                "provenance_sha256": "",
            }
            provenance = {
                "profile": "release",
                "source_sha256": attestation["source_sha256"],
                "binary_sha256": attestation["worker_sha256"],
            }
            provenance_path.write_text(json.dumps(provenance, sort_keys=True) + "\n")
            attestation["provenance_sha256"] = proxy.digest(provenance_path)
            with patch.object(
                proxy.run_upstream,
                "checked_worker_provenance",
                side_effect=AssertionError("full source/binary hashing entered timed child"),
            ):
                checked_path, checked = proxy.checked_campaign_attestation(
                    worker, provenance_path, attestation
                )
            self.assertEqual(checked_path, provenance_path.resolve())
            self.assertEqual(checked, provenance)

            mutations = (
                {**attestation, "worker_sha256": "0" * 64},
                {**attestation, "source_sha256": "0" * 64},
                {**attestation, "provenance_sha256": "0" * 64},
                {"worker_sha256": attestation["worker_sha256"]},
                {**attestation, "worker_sha256": "not-a-sha"},
            )
            for mutated in mutations:
                with self.subTest(mutated=mutated), self.assertRaises(ValueError):
                    proxy.checked_campaign_attestation(
                        worker, provenance_path, mutated
                    )

            other = Path(directory) / "other.json"
            other.write_bytes(provenance_path.read_bytes())
            with self.assertRaisesRegex(ValueError, "canonical"):
                proxy.checked_campaign_attestation(worker, other, attestation)

    def test_standalone_once_retains_full_worker_attestation(self):
        with patch.object(proxy, "validate_workload_population", return_value=[]), patch.object(
            proxy.run_upstream, "checked_worker_provenance"
        ) as checked:
            with self.assertRaisesRegex(ValueError, "population"):
                proxy.once(
                    proxy.ROOT / "target/release/duckdb-rust-test-worker",
                    proxy.ROOT / "target/release/duckdb-rust-test-worker.provenance.json",
                    proxy.ROOT,
                    "not-present.test",
                )
            checked.assert_not_called()

        workload = proxy.EXPECTED_WORKLOADS[0]
        specs = [{"path": workload["path"], "proxy_records_expected": workload["proxy_records"]}]
        with patch.object(proxy, "validate_workload_population", return_value=specs), patch.object(
            proxy.run_upstream,
            "checked_worker_provenance",
            side_effect=RuntimeError("full attestation reached"),
        ) as checked:
            with self.assertRaisesRegex(RuntimeError, "full attestation reached"):
                proxy.once(
                    proxy.ROOT / "target/release/duckdb-rust-test-worker",
                    proxy.ROOT / "target/release/duckdb-rust-test-worker.provenance.json",
                    proxy.ROOT,
                    workload["path"],
                )
            checked.assert_called_once()

    def test_once_rejects_partial_campaign_attestation_before_execution(self):
        arguments = [
            "--once",
            "--worker",
            str(proxy.ROOT / "target/release/duckdb-rust-test-worker"),
            "--worker-provenance",
            str(proxy.ROOT / "target/release/duckdb-rust-test-worker.provenance.json"),
            "--test-root",
            str(proxy.ROOT),
            "--path",
            proxy.EXPECTED_WORKLOADS[0]["path"],
            "--campaign-worker-sha256",
            "1" * 64,
        ]
        with self.assertRaises(SystemExit), redirect_stdout(
            io.StringIO()
        ), redirect_stderr(io.StringIO()):
            proxy.main(arguments)

    def test_missing_duplicate_reordered_and_changed_workloads_fail(self):
        for mutate in (
            lambda workloads: workloads.pop(),
            lambda workloads: workloads.append(copy.deepcopy(workloads[0])),
            lambda workloads: workloads.reverse(),
            lambda workloads: workloads[0].update(path="other.test"),
        ):
            context, report = self.report()
            mutate(report["workloads"])
            with self.subTest(mutate=mutate), self.assertRaisesRegex(ValueError, "workload"):
                proxy.validate_report(report, context)

    def test_manifest_and_workload_hashes_are_frozen(self):
        self.assertEqual(
            set(proxy.HELPERS),
            {
                "measure_sqllogic_proxy.py",
                "measure_sqllogic_performance.py",
                "run_upstream.py",
                "sqllogic.py",
                "source_identity.py",
                "reference_version.py",
                "upstream_suite.py",
            },
        )
        workloads = proxy.validate_workload_population(
            proxy.ROOT / proxy.EXPECTED_MANIFEST, proxy.ROOT
        )
        self.assertEqual([item["proxy_records_expected"] for item in workloads], [5, 5000, 16, 4, 2002])
        with patch.object(proxy, "digest", return_value="0" * 64):
            with self.assertRaisesRegex(ValueError, "frozen five-workload"):
                proxy.validate_workload_population(proxy.ROOT / proxy.EXPECTED_MANIFEST, proxy.ROOT)
        with tempfile.TemporaryDirectory() as directory:
            copied = Path(directory) / "workloads.json"
            copied.write_bytes((proxy.ROOT / proxy.EXPECTED_MANIFEST).read_bytes())
            with self.assertRaisesRegex(ValueError, "frozen five-workload"):
                proxy.validate_workload_population(copied, proxy.ROOT)

    def test_actual_input_snapshots_and_all_top_level_identities_are_required(self):
        mutations = (
            lambda report: report["inputs_before"]["frozen"].update(sha256="0" * 64),
            lambda report: report["inputs_after"]["frozen"].update(bytes=2),
            lambda report: report["worker"].update(sha256="0" * 64),
            lambda report: report["python"].update(binary_sha256="0" * 64),
            lambda report: report["references"]["release"].update(target="other"),
            lambda report: report["campaign"].update(worker="/other-worker"),
            lambda report: report["requested_arguments"].update(worker_provenance="/other-provenance"),
        )
        for mutate in mutations:
            context, report = self.report()
            mutate(report)
            with self.subTest(mutate=mutate), self.assertRaisesRegex(ValueError, "identity|input|arguments"):
                proxy.validate_report(report, context)

    def test_stdout_markers_are_reparsed_and_claims_must_match(self):
        context, report = self.report()
        sample = report["workloads"][0]["observations"]["proxy"][0]
        sample["stdout"] = "PASS path (6 records)\n6 records passed; 0 skipped\n"
        with self.assertRaisesRegex(ValueError, "stdout"):
            proxy.validate_report(report, context)
        context, report = self.report()
        sample = report["workloads"][0]["warmup_observations"]["release"][0]
        sample["stdout"] = "No tests ran\n"
        with self.assertRaises(ValueError):
            proxy.validate_report(report, context)

    def test_warmups_and_samples_are_complete_stable_and_nonzero(self):
        mutations = (
            lambda entry: entry["warmup_observations"]["proxy"].pop(),
            lambda entry: entry["observations"]["development"].pop(),
            lambda entry: entry["warmup_observations"]["proxy"][0].update(records=0),
            lambda entry: entry["observations"]["release"][1].update(records=999),
            lambda entry: entry["pass_counts"].update(proxy=999),
        )
        for mutate in mutations:
            context, report = self.report()
            mutate(report["workloads"][0])
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                proxy.validate_report(report, context)

    def test_proxy_expected_counts_are_independent_of_cpp_assertion_counts(self):
        context, report = self.report()
        entry = report["workloads"][1]
        for phase in ("warmup_observations", "observations"):
            for sample in entry[phase]["proxy"]:
                sample["records"] = 4999
                sample["stdout"] = (
                    f"PASS {entry['path']} (4999 records)\n4999 records passed; 0 skipped\n"
                )
        entry["pass_counts"]["proxy"] = 4999
        with self.assertRaisesRegex(ValueError, "proxy PASS count changed"):
            proxy.validate_report(report, context)

    def test_configuration_errors_and_failed_reports_never_validate(self):
        mutations = (
            lambda report: report.update(samples=9),
            lambda report: report.update(warmups=2),
            lambda report: report["execution_configuration"]["cpp"].update(threads=2),
            lambda report: report.update(proxy_configuration={}),
            lambda report: report.update(error="failed"),
            lambda report: report.update(error=""),
            lambda report: report.update(failed_invocation={"command": ["x"]}),
            lambda report: report.update(failed_invocation={}),
            lambda report: report.update(failure_phase="sample"),
            lambda report: report.update(failure_phase=""),
            lambda report: report.update(unexpected="field"),
        )
        for mutate in mutations:
            context, report = self.report()
            mutate(report)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                proxy.validate_report(report, context)

    def test_matched_serial_schedule_is_replayed(self):
        context, report = self.report()
        sample = report["workloads"][0]["observations"]["proxy"][0]
        sample["sequence"] += 1
        with self.assertRaisesRegex(ValueError, "serial schedule"):
            proxy.validate_report(report, context)

    def test_unexpected_self_declared_commands_and_nonfinite_gate_are_fail_closed(self):
        context, report = self.report()
        report["workloads"][0]["commands"] = {"release": ["mutated"]}
        with self.assertRaisesRegex(ValueError, "unsupported fields"):
            proxy.validate_report(report, context)

        context, report = self.report()
        for entry in report["workloads"]:
            for phase in ("warmup_observations", "observations"):
                for sample in entry[phase]["release"] + entry[phase]["development"]:
                    sample["block_input"] = 0
                for sample in entry[phase]["proxy"]:
                    sample["block_input"] = 1
        raw = {entry["id"]: entry["observations"] for entry in report["workloads"]}
        release = {"workloads": [
            {**workload, "cpp": raw[workload["id"]]["release"], "rust": raw[workload["id"]]["proxy"]}
            for workload in context["workloads"]
        ]}
        development = {"workloads": [
            {**workload, "cpp": raw[workload["id"]]["development"], "rust": raw[workload["id"]]["proxy"]}
            for workload in context["workloads"]
        ]}
        report["gate"] = proxy.json_safe(
            proxy.measure.gate(release, development, context["workloads"])
        )
        report["passed"] = False
        report["at_parity_or_better_performance"] = False
        self.assertFalse(proxy.validate_report(report, context)["passed"])
        json.dumps(report, allow_nan=False)

    def test_gate_and_verdict_are_recomputed_for_pass_and_failure(self):
        context, report = self.report()
        report["gate"]["workloads"][0]["passed"] = False
        with self.assertRaisesRegex(ValueError, "recomputed"):
            proxy.validate_report(report, context)
        context, failed = self.report(proxy_metric=110)
        gate = proxy.validate_report(failed, context)
        self.assertFalse(gate["passed"])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "failed.json"
            path.write_text(json.dumps(failed))
            with patch.object(proxy, "context_from_report", return_value=context):
                with redirect_stdout(io.StringIO()):
                    self.assertEqual(proxy.main(["--validate", str(path)]), 1)

    def test_validate_cli_rejects_malformed_and_stale_reports(self):
        context, report = self.report()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            path.write_text("not json")
            with redirect_stdout(io.StringIO()):
                self.assertEqual(proxy.main(["--validate", str(path)]), 1)
            path.write_text(json.dumps(report))
            stale = copy.deepcopy(context)
            stale["inputs"] = {"changed": True}
            with patch.object(proxy, "context_from_report", return_value=stale):
                with redirect_stdout(io.StringIO()):
                    self.assertEqual(proxy.main(["--validate", str(path)]), 1)

    def campaign_args(self, report):
        values = {
            name: Path("/requested") / name
            for name in proxy.CAMPAIGN_ARGUMENTS
        }
        values.update(
            {
                "report": report,
                "samples": proxy.ACCEPTANCE_SAMPLES,
                "warmups": proxy.ACCEPTANCE_WARMUPS,
            }
        )
        return type("Args", (), values)()

    def test_campaign_serializes_paths_warmups_and_full_observations(self):
        context = self.context()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            args = self.campaign_args(path)

            def timed(command, label):
                command = list(command)
                relative = (
                    command[command.index("--path") + 1]
                    if "--once" in command
                    else command[command.index("--test-dir") + 2]
                )
                workload = next(item for item in context["workloads"] if item["path"] == relative)
                target = "proxy" if "--once" in command else (
                    "release" if command[0] == context["references"]["release"]["unittest"] else "development"
                )
                records = workload["proxy_records_expected"] if target == "proxy" else (100 if target == "release" else 200)
                metric = 90 if target == "proxy" else 100
                return observation(command, target, relative, records, metric)

            with patch.object(proxy.measure, "active_peers", return_value=[]), patch.object(
                proxy, "build_context", side_effect=[context, context]
            ) as context_builder, patch.object(
                proxy.measure, "run_timed", side_effect=timed
            ):
                result = proxy.campaign(args)
            disk = json.loads(path.read_text())
            self.assertTrue(result["passed"], result.get("error"))
            self.assertEqual(disk, result)
            self.assertEqual(
                len(disk["workloads"][0]["warmup_observations"]["proxy"]),
                proxy.ACCEPTANCE_WARMUPS,
            )
            self.assertEqual(
                len(disk["workloads"][0]["observations"]["proxy"]),
                proxy.ACCEPTANCE_SAMPLES,
            )
            self.assertEqual(context_builder.call_count, 2)
            self.assertTrue(proxy.validate_report(disk, context)["passed"])

    def test_changed_or_missing_post_campaign_attestation_cannot_pass(self):
        context = self.context()
        changed = copy.deepcopy(context)
        changed["worker"]["sha256"] = "0" * 64
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "changed.json"
            args = self.campaign_args(path)

            def timed(command, label):
                relative = (
                    command[command.index("--path") + 1]
                    if "--once" in command
                    else command[command.index("--test-dir") + 2]
                )
                workload = next(
                    item for item in context["workloads"] if item["path"] == relative
                )
                target = "proxy" if "--once" in command else "release"
                records = (
                    workload["proxy_records_expected"] if target == "proxy" else 100
                )
                return observation(command, target, relative, records)

            with patch.object(
                proxy.measure, "active_peers", return_value=[]
            ), patch.object(
                proxy, "build_context", side_effect=[context, changed]
            ) as context_builder, patch.object(
                proxy.measure, "run_timed", side_effect=timed
            ):
                result = proxy.campaign(args)
            self.assertEqual(context_builder.call_count, 2)
            self.assertFalse(result["passed"])
            self.assertEqual(result["failure_phase"], "input-revalidation")
            self.assertIn("changed during measurement", result["error"])
            with self.assertRaisesRegex(ValueError, "failed or partial"):
                proxy.validate_report(result, context)

    def test_partial_invocation_and_setup_failures_are_persisted(self):
        context = self.context()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "partial.json"
            args = self.campaign_args(path)
            commands = proxy.expected_commands(context, context["workloads"][0])
            calls = 0

            def timed(command, label):
                nonlocal calls
                calls += 1
                if calls == 5:
                    raise proxy.measure.SampleFailure("boom", {"command": list(command), "returncode": 2})
                target = "proxy" if "--once" in command else (
                    "release" if command[0] == commands["release"][0] else "development"
                )
                records = 5 if target == "proxy" else 100
                return observation(command, target, context["workloads"][0]["path"], records)

            with patch.object(proxy.measure, "active_peers", return_value=[]), patch.object(
                proxy, "build_context", return_value=context
            ), patch.object(proxy.measure, "run_timed", side_effect=timed):
                result = proxy.campaign(args)
            disk = json.loads(path.read_text())
            self.assertFalse(result["passed"])
            self.assertEqual(disk["failed_invocation"]["returncode"], 2)
            self.assertEqual(sum(len(values) for values in disk["workloads"][0]["warmup_observations"].values()), 4)
            with self.assertRaisesRegex(ValueError, "failed or partial"):
                proxy.validate_report(disk, context)

            setup_path = Path(directory) / "setup.json"
            setup_args = self.campaign_args(setup_path)
            with patch.object(proxy.measure, "active_peers", return_value=[]), patch.object(
                proxy, "build_context", side_effect=ValueError("bad identity")
            ):
                setup = proxy.campaign(setup_args)
            self.assertEqual(setup["failure_phase"], "setup")
            self.assertIn("bad identity", json.loads(setup_path.read_text())["error"])

    def test_existing_report_is_refused_before_setup_or_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "existing.json"
            path.write_text("preserve")
            args = self.campaign_args(path)
            with patch.object(proxy.measure, "active_peers") as peers:
                with self.assertRaises(FileExistsError):
                    proxy.campaign(args)
                peers.assert_not_called()
            self.assertEqual(path.read_text(), "preserve")


if __name__ == "__main__":
    unittest.main()
