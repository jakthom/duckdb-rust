import argparse
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import measure_b1_views as m


STDERR = (
    "0.01 real 0.01 user 0.00 sys\n"
    "9 maximum resident set size\n"
    "0 block input operations\n"
    "3 block output operations\n"
)
CHECKSUM = '[{"row_count":10000,"checksum":49995000}]'
ABSENT = "Catalog Error: Table with name b1_public does not exist!"


def engine(target):
    return {"kind": "rust" if target == "rust" else "cpp", "binary": f"/{target}"}


def untimed(command, label, returncode=0, stdout=CHECKSUM, stderr=""):
    return {
        "command": [str(part) for part in command],
        "label": label,
        "returncode": returncode,
        "stdout": stdout,
        "stderr": stderr,
    }


def timed(command, phase, value=100, stdout="[]"):
    return {
        "command": [str(part) for part in command],
        "phase": phase,
        "returncode": 0,
        "stdout": stdout,
        "stderr": STDERR,
        "wall_ns": value,
        "cpu_ns": 10_000_000,
        "max_rss_bytes": 9,
        "block_input": 0,
        "block_output": 3,
    }


def sample(target, mode, workload, database, value=100):
    selected = engine(target)
    sql = m.sqls(workload, mode)
    phases = [
        timed(m.command(selected, database, mode, sql["publish"]), "publish", value),
        timed(
            m.command(selected, database, mode, sql["reopen_query_drop"]),
            "reopen_query_drop",
            value,
            CHECKSUM,
        ),
    ]
    return {
        "seed_sha256": "seed",
        "seed_copy": {"sha256": "seed", "bytes": 4, "wal_absent": True},
        "seed_verification": untimed(
            m.command(selected, database, mode, m.seed_sql(), readonly=True),
            "seed_verification",
        ),
        "phases": phases,
        "absence": untimed(
            m.command(selected, database, mode, m.absent_sql(), readonly=True),
            "absence",
            returncode=1,
            stdout="",
            stderr=ABSENT,
        ),
        "seed_after": untimed(
            m.command(selected, database, mode, m.seed_sql(), readonly=True),
            "seed_after",
        ),
        "aggregate": {
            "wall_ns": value * 2,
            "cpu_ns": 20_000_000,
            "max_rss_bytes": 9,
            "block_input": 0,
            "block_output": 6,
        },
        "artifacts": {
            "": {"path": str(database), "sha256": "db", "bytes": 1},
            ".wal": None,
        },
    }


def context(output):
    return {
        "manifest": {"manifest": 1},
        "inputs": {"seed": {"sha256": "seed", "bytes": 4}},
        "engines": {target: engine(target) for target in m.TARGETS_ORDER},
        "output": str(output),
    }


def complete_report(output, rust_value=90):
    selected_context = context(output)
    results = []
    for mode in m.EXPECTED["configurations"]:
        for workload in m.EXPECTED["workloads"]:
            warmups = {target: [] for target in m.TARGETS_ORDER}
            observations = {target: [] for target in m.TARGETS_ORDER}
            schedule = []
            for round_number, target, database in m.schedule(output, mode, workload):
                value = rust_value if target == "rust" else 100
                row = sample(target, mode, workload, database, value)
                bucket = (
                    warmups if round_number < m.EXPECTED["warmups"] else observations
                )
                bucket[target].append(row)
                schedule.append(
                    {"round": round_number, "target": target, "database": str(database)}
                )
            results.append(
                {
                    "mode": mode,
                    "workload": workload,
                    "schedule": schedule,
                    "warmups": warmups,
                    "observations": observations,
                }
            )
    report = {
        "status": "complete",
        "passed": False,
        "manifest": selected_context["manifest"],
        "inputs_before": selected_context["inputs"],
        "inputs_after": selected_context["inputs"],
        "results": results,
    }
    with patch.object(m, "validate_artifacts"):
        report["gate"] = m.evaluate(report, selected_context)
    report["passed"] = report["gate"]["passed"]
    return report, selected_context


def campaign_args(output, run=False):
    return argparse.Namespace(
        manifest=Path("/manifest"),
        output_dir=Path(output),
        seed=Path("/seed"),
        rust=Path("/rust"),
        rust_provenance=Path("/rust.provenance.json"),
        release=Path("/release"),
        development=Path("/development"),
        release_source=Path("/release-source"),
        release_build=Path("/release-build"),
        development_source=Path("/development-source"),
        development_build=Path("/development-build"),
        run=run,
        quiet_host_confirmed=run,
    )


class DurableViewMeasurementTests(unittest.TestCase):
    def test_manifest_is_exact(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            path.write_text(json.dumps(m.EXPECTED))
            self.assertEqual(m.workload_manifest(path)["data"], m.EXPECTED)
            changed = copy.deepcopy(m.EXPECTED)
            changed["samples"] = 20
            path.write_text(json.dumps(changed))
            with self.assertRaises(ValueError):
                m.workload_manifest(path)

    def test_parse_time_requires_each_metric_and_preserves_real_zero_io(self):
        self.assertEqual(
            m.parse_time(STDERR),
            {
                "cpu_ns": 10_000_000,
                "max_rss_bytes": 9,
                "block_input": 0,
                "block_output": 3,
            },
        )
        for missing in (
            "real",
            "maximum resident set size",
            "block input operations",
            "block output operations",
        ):
            with self.subTest(missing=missing), self.assertRaises(ValueError):
                m.parse_time(
                    "\n".join(
                        line for line in STDERR.splitlines() if missing not in line
                    )
                )

    def test_timed_failure_and_missing_metrics_preserve_raw_observation(self):
        failed = subprocess.CompletedProcess(["x"], 2, "out", "err")
        with patch.object(m.platform, "system", return_value="Darwin"):
            with self.assertRaises(m.SampleFailure) as caught:
                m.timed(["x"], "publish", execute=lambda *args, **kwargs: failed)
        self.assertEqual(caught.exception.observation["returncode"], 2)
        incomplete = subprocess.CompletedProcess(
            ["x"], 0, "out", "0 real 0 user 0 sys\n"
        )
        with patch.object(m.platform, "system", return_value="Darwin"):
            with self.assertRaises(m.SampleFailure) as caught:
                m.timed(["x"], "publish", execute=lambda *args, **kwargs: incomplete)
        self.assertEqual(caught.exception.observation["stderr"], incomplete.stderr)

    def test_sql_and_commands_fix_two_phases_threads_and_fresh_create(self):
        for workload in m.EXPECTED["workloads"]:
            for mode in m.EXPECTED["configurations"]:
                sql = m.sqls(workload, mode)
                self.assertEqual(tuple(sql), m.PHASES)
                self.assertTrue(all("SET threads=1" in value for value in sql.values()))
                self.assertNotIn("OR REPLACE", sql["publish"])
                self.assertEqual("CHECKPOINT" in sql["publish"], mode == "checkpoint")
        cpp = m.command(engine("release"), "/db", "wal", "SELECT 1", readonly=True)
        rust = m.command(engine("rust"), "/db", "wal", "SELECT 1", readonly=True)
        self.assertIn("-readonly", cpp)
        self.assertIn("PRAGMA disable_checkpoint_on_shutdown", cpp[-1])
        self.assertIn("--read-only", rust)
        self.assertNotIn("--durability", rust)

    def test_json_result_rejects_shape_type_and_value_changes(self):
        m.json_row(CHECKSUM)
        bad = (
            "[]",
            '[{"row_count":10000}]',
            '[{"row_count":"10000","checksum":49995000}]',
            '[{"row_count":true,"checksum":49995000}]',
            '[{"row_count":10000,"checksum":1}]',
        )
        for value in bad:
            with self.subTest(value=value), self.assertRaises(ValueError):
                m.json_row(value)

    def test_absence_must_name_b1_public_in_the_missing_object_clause(self):
        m.absence_error(untimed([], "absence", 1, "", ABSENT))
        m.absence_error(
            untimed(
                [],
                "absence",
                1,
                "",
                "Catalog error: table main.b1_public does not exist",
            )
        )
        bad = (
            untimed([], "absence", 0, "", ""),
            untimed([], "absence", 1, "", "disk full"),
            untimed(
                [],
                "absence",
                1,
                "",
                "Table wrong_name does not exist; query mentioned b1_public",
            ),
        )
        for value in bad:
            with self.assertRaises(ValueError):
                m.absence_error(value)

    def test_one_sample_records_original_seed_and_all_untimed_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            seed = Path(directory) / "seed.duckdb"
            database = Path(directory) / "sample.duckdb"
            seed.write_bytes(b"seed")
            selected = engine("rust")
            observations = [
                untimed(
                    m.command(
                        selected, database, "checkpoint", m.seed_sql(), readonly=True
                    ),
                    "seed_verification",
                ),
                untimed(
                    m.command(
                        selected, database, "checkpoint", m.absent_sql(), readonly=True
                    ),
                    "absence",
                    1,
                    "",
                    ABSENT,
                ),
                untimed(
                    m.command(
                        selected, database, "checkpoint", m.seed_sql(), readonly=True
                    ),
                    "seed_after",
                ),
            ]

            def timed_phase(command, phase):
                if phase == "reopen_query_drop":
                    database.write_bytes(b"mutated")
                return timed(
                    command,
                    phase,
                    stdout=CHECKSUM if phase == "reopen_query_drop" else "[]",
                )

            with (
                patch.object(m, "observe", side_effect=observations),
                patch.object(m, "timed", side_effect=timed_phase),
            ):
                value = m.one_sample(
                    selected, "checkpoint", "view_cycle", seed, database
                )
            self.assertEqual(value["seed_sha256"], m.digest(seed))
            self.assertNotEqual(value["seed_sha256"], m.digest(database))
            self.assertIn("--read-only", value["seed_verification"]["command"])
            self.assertEqual(value["absence"]["label"], "absence")
            self.assertEqual(value["seed_after"]["label"], "seed_after")
            self.assertEqual([row["phase"] for row in value["phases"]], list(m.PHASES))

    def test_one_sample_preserves_failed_timed_phase_and_prior_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            seed = Path(directory) / "seed"
            database = Path(directory) / "db"
            seed.write_bytes(b"seed")
            selected = engine("rust")
            seed_check = untimed(
                m.command(
                    selected, database, "checkpoint", m.seed_sql(), readonly=True
                ),
                "seed_verification",
            )
            failed = timed([], "publish")
            failed["returncode"] = 2
            with (
                patch.object(m, "observe", return_value=seed_check),
                patch.object(
                    m,
                    "timed",
                    side_effect=m.SampleFailure("publish CLI failed", failed),
                ),
            ):
                with self.assertRaises(m.SampleFailure) as caught:
                    m.one_sample(selected, "checkpoint", "view_cycle", seed, database)
            partial = caught.exception.observation
            self.assertEqual(partial["seed_verification"], seed_check)
            self.assertEqual(partial["phases"], [failed])
            self.assertEqual(partial["failure"]["detail"], failed)

    def test_one_sample_wraps_pre_phase_io_failure_as_partial_sample(self):
        with tempfile.TemporaryDirectory() as directory:
            seed = Path(directory) / "seed"
            database = Path(directory) / "db"
            seed.write_bytes(b"seed")
            with patch.object(m, "digest", side_effect=OSError("unreadable")):
                with self.assertRaises(m.SampleFailure) as caught:
                    m.one_sample(
                        engine("rust"), "checkpoint", "view_cycle", seed, database
                    )
            partial = caught.exception.observation
            self.assertIsNone(partial["seed_sha256"])
            self.assertEqual(partial["phases"], [])
            self.assertEqual(partial["failure"]["detail"], {"type": "OSError"})

    def test_one_sample_preserves_both_phases_on_checksum_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            seed = Path(directory) / "seed"
            database = Path(directory) / "db"
            seed.write_bytes(b"seed")
            selected = engine("rust")
            seed_check = untimed(
                m.command(
                    selected, database, "checkpoint", m.seed_sql(), readonly=True
                ),
                "seed_verification",
            )
            rows = [timed([], "publish"), timed([], "reopen_query_drop", stdout="[]")]
            with (
                patch.object(m, "observe", return_value=seed_check),
                patch.object(m, "timed", side_effect=rows),
            ):
                with self.assertRaises(m.SampleFailure) as caught:
                    m.one_sample(selected, "checkpoint", "view_cycle", seed, database)
            self.assertEqual(caught.exception.observation["phases"], rows)

    def test_one_sample_preserves_absence_and_positive_seed_on_absence_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            seed = Path(directory) / "seed"
            database = Path(directory) / "db"
            seed.write_bytes(b"seed")
            selected = engine("rust")
            observations = [
                untimed([], "seed_verification"),
                untimed(
                    [], "absence", 1, "", "Table wrong_name does not exist; b1_public"
                ),
                untimed([], "seed_after"),
            ]
            rows = [
                timed([], "publish"),
                timed([], "reopen_query_drop", stdout=CHECKSUM),
            ]
            with (
                patch.object(m, "observe", side_effect=observations),
                patch.object(m, "timed", side_effect=rows),
            ):
                with self.assertRaises(m.SampleFailure) as caught:
                    m.one_sample(selected, "checkpoint", "view_cycle", seed, database)
            partial = caught.exception.observation
            self.assertIsNotNone(partial["absence"])
            self.assertIsNotNone(partial["seed_after"])
            self.assertEqual(partial["phases"], rows)

    def test_artifact_identity_is_required_and_replayed(self):
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "db"
            database.write_bytes(b"one")
            identity = m.artifact_files(database)
            m.validate_artifacts(identity, database)
            database.write_bytes(b"two")
            with self.assertRaises(ValueError):
                m.validate_artifacts(identity, database)
            with self.assertRaises(ValueError):
                m.validate_artifacts({"": None, ".wal": None}, database)

    def test_validate_sample_rejects_command_output_metric_seed_and_aggregate_tampering(
        self,
    ):
        database = Path("/out/db")
        base = sample("rust", "checkpoint", "view_cycle", database)
        mutations = (
            lambda value: value.__setitem__("seed_sha256", "wrong"),
            lambda value: value["seed_copy"].__setitem__("wal_absent", False),
            lambda value: value["phases"][0]["command"].__setitem__(0, "/wrong"),
            lambda value: value["phases"][0].__setitem__("cpu_ns", 0),
            lambda value: value["phases"][0].__setitem__("wall_ns", True),
            lambda value: value["phases"][1].__setitem__("stdout", "[]"),
            lambda value: value["absence"].__setitem__(
                "stderr", "Table other does not exist"
            ),
            lambda value: value["seed_after"].__setitem__("returncode", True),
            lambda value: value["seed_after"].__setitem__("returncode", 1),
            lambda value: value["aggregate"].__setitem__("wall_ns", 1),
        )
        with patch.object(m, "validate_artifacts"):
            seed_identity = {"sha256": "seed", "bytes": 4}
            m.validate_sample(
                base,
                engine("rust"),
                "checkpoint",
                "view_cycle",
                database,
                seed_identity,
            )
            for mutate in mutations:
                changed = copy.deepcopy(base)
                mutate(changed)
                with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                    m.validate_sample(
                        changed,
                        engine("rust"),
                        "checkpoint",
                        "view_cycle",
                        database,
                        seed_identity,
                    )

    def test_schedule_has_exact_rotated_warmup_and_observation_populations(self):
        rows = m.schedule("/absolute", "wal", "view_cycle")
        self.assertEqual(len(rows), 72)
        self.assertEqual(
            [target for _, target, _ in rows[:6]],
            [
                "release",
                "development",
                "rust",
                "development",
                "rust",
                "release",
            ],
        )
        for target in m.TARGETS_ORDER:
            self.assertEqual(
                sum(
                    1
                    for round_number, name, _ in rows
                    if name == target and round_number < 3
                ),
                3,
            )
            self.assertEqual(
                sum(
                    1
                    for round_number, name, _ in rows
                    if name == target and round_number >= 3
                ),
                21,
            )
        self.assertTrue(all(database.is_absolute() for _, _, database in rows))

    def test_replay_rejects_population_round_path_binary_metric_and_identity_mutations(
        self,
    ):
        report, selected_context = complete_report("/out")
        with patch.object(m, "validate_artifacts"):
            self.assertTrue(m.replay(report, selected_context)["passed"])
            mutations = (
                lambda value: value["results"].pop(),
                lambda value: value["results"][0]["observations"]["rust"].pop(),
                lambda value: value["results"][0]["schedule"][0].__setitem__(
                    "round", 2
                ),
                lambda value: value["results"][0]["schedule"][0].__setitem__(
                    "database", "/other"
                ),
                lambda value: value["results"][0]["observations"]["rust"][0]["phases"][
                    0
                ]["command"].__setitem__(0, "/evil"),
                lambda value: value["results"][0]["observations"]["rust"][0]["phases"][
                    0
                ].__setitem__("max_rss_bytes", 10),
                lambda value: value.__setitem__("inputs_after", {}),
                lambda value: value.__setitem__("gate", {}),
            )
            for mutate in mutations:
                changed = copy.deepcopy(report)
                mutate(changed)
                with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                    m.replay(changed, selected_context)

    def test_replay_accepts_structurally_valid_complete_performance_failure(self):
        report, selected_context = complete_report("/out", rust_value=110)
        self.assertFalse(report["passed"])
        with patch.object(m, "validate_artifacts"):
            result = m.replay(report, selected_context)
        self.assertFalse(result["passed"])

    def test_gate_uses_each_metric_and_zero_baseline_fails_positive_rust(self):
        populations = {
            target: [
                {"aggregate": {metric: 0 for metric in m.METRICS}}
                for _ in range(m.EXPECTED["samples"])
            ]
            for target in m.TARGETS_ORDER
        }
        for row in populations["rust"]:
            row["aggregate"]["block_input"] = 1
            row["aggregate"]["wall_ns"] = 1
        for target in ("release", "development"):
            for row in populations[target]:
                row["aggregate"]["wall_ns"] = 1
        result = m.gate(populations)
        self.assertFalse(result["passed"])
        self.assertEqual(result["rust_over_fastest"]["block_input"], float("inf"))

    def test_prepare_rust_provenance_builds_exact_command_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "target/release/duckdb-rust"
            provenance = root / "attestation.json"
            source = {"sha256": "source", "count": 1, "files": []}
            toolchain = {"cargo": "cargo", "rustc": "rustc"}
            calls = []

            def execute(command, cwd, check):
                calls.append((command, cwd, check))
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b"release")

            with (
                patch.object(m, "ROOT", root),
                patch.object(m, "rust_source_identity", return_value=source),
                patch.object(m, "toolchain_identity", return_value=toolchain),
                patch.object(m, "git_revision", return_value="revision"),
            ):
                value = m.prepare_rust_provenance(provenance, execute=execute)
                self.assertEqual(calls, [([*m.BUILD_COMMAND], root, True)])
                self.assertEqual(value["binary_after"], m.file_id(binary))
                self.assertEqual(json.loads(provenance.read_text()), value)
                with self.assertRaises(FileExistsError):
                    m.prepare_rust_provenance(provenance, execute=execute)

    def test_rust_identity_rejects_stale_profile_source_binary_and_noncanonical_path(
        self,
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "target/release/duckdb-rust"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"release")
            source = {"sha256": "source", "count": 1, "files": []}
            toolchain = {"cargo": "cargo", "rustc": "rustc"}
            base = {
                "schema": 1,
                "recorded_at": "now",
                "root": str(root.resolve()),
                "git_revision": "revision",
                "profile": "release",
                "default_features": False,
                "build_command": [*m.BUILD_COMMAND],
                "source_before": source,
                "source_after": source,
                "toolchain_before": toolchain,
                "toolchain_after": toolchain,
                "binary_before": None,
                "binary_after": m.file_id(binary),
            }
            provenance = root / "attestation.json"
            with (
                patch.object(m, "ROOT", root),
                patch.object(m, "rust_source_identity", return_value=source),
                patch.object(m, "toolchain_identity", return_value=toolchain),
                patch.object(m, "git_revision", return_value="revision"),
            ):
                provenance.write_text(json.dumps(base))
                m.rust_identity(binary, provenance)
                for mutate in (
                    lambda value: value.__setitem__("schema", 2),
                    lambda value: value.__setitem__("git_revision", "other"),
                    lambda value: value.__setitem__("profile", "debug"),
                    lambda value: value.__setitem__("source_after", {}),
                    lambda value: value["binary_after"].__setitem__("sha256", "bad"),
                    lambda value: value.__setitem__("toolchain_after", {}),
                ):
                    changed = copy.deepcopy(base)
                    mutate(changed)
                    provenance.write_text(json.dumps(changed))
                    with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                        m.rust_identity(binary, provenance)
                other = root / "other"
                other.write_bytes(b"release")
                provenance.write_text(json.dumps(base))
                with self.assertRaises(ValueError):
                    m.rust_identity(other, provenance)

    def test_selected_action_makes_validate_and_prepare_flags_conditional(self):
        validate = m.parser().parse_args(["--validate", "report.json"])
        self.assertEqual(m.selected_action(validate), "validate")
        with self.assertRaises(ValueError):
            m.selected_action(
                m.parser().parse_args(["--validate", "report.json", "--seed", "seed"])
            )
        prepare = m.parser().parse_args(["--prepare-rust-provenance", "rust.json"])
        self.assertEqual(m.selected_action(prepare), "prepare")
        with self.assertRaises(ValueError):
            m.selected_action(
                m.parser().parse_args(
                    ["--prepare-rust-provenance", "rust.json", "--run"]
                )
            )
        with self.assertRaises(ValueError):
            m.selected_action(m.parser().parse_args([]))
        quiet_without_run = m.parser().parse_args(
            [
                "--output-dir",
                "out",
                "--seed",
                "seed",
                "--rust",
                "rust",
                "--rust-provenance",
                "rust.json",
                "--quiet-host-confirmed",
            ]
        )
        with self.assertRaises(ValueError):
            m.selected_action(quiet_without_run)

    def test_campaign_arguments_are_absolute_and_exact_on_replay(self):
        arguments = campaign_args("relative-output")
        serialized = m.serialized_arguments(arguments)
        self.assertTrue(all(Path(value).is_absolute() for value in serialized.values()))
        restored = m.arguments_from_report({"requested_arguments": serialized})
        self.assertEqual(m.serialized_arguments(restored), serialized)
        changed = dict(serialized)
        changed["unused"] = "/unused"
        with self.assertRaises(ValueError):
            m.arguments_from_report({"requested_arguments": changed})

    def test_campaign_failure_retains_failed_sample_and_prior_result_structure(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            arguments = campaign_args(output, run=True)
            selected_context = context(output.resolve())
            partial = {"phases": [{"phase": "publish"}], "failure": {"message": "bad"}}
            with (
                patch.object(m, "campaign_context", return_value=selected_context),
                patch.object(m, "active_peers", return_value=[]),
                patch.object(
                    m, "one_sample", side_effect=m.SampleFailure("bad", partial)
                ),
            ):
                report = m.run_campaign(arguments)
            self.assertEqual(report["status"], "failed")
            self.assertFalse(report["passed"])
            self.assertEqual(
                report["results"][0]["failed_sample"]["observation"], partial
            )
            self.assertEqual(json.loads((output / "report.json").read_text()), report)

    def test_prepared_campaign_checks_identity_twice_without_running_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            arguments = campaign_args(output)
            selected_context = context(output.resolve())
            with (
                patch.object(
                    m, "campaign_context", return_value=selected_context
                ) as checked,
                patch.object(m, "one_sample") as run,
            ):
                report = m.run_campaign(arguments)
            self.assertEqual(report["status"], "prepared")
            self.assertEqual(checked.call_count, 2)
            run.assert_not_called()

    def test_seed_identity_requires_storage64_and_no_wal(self):
        with tempfile.TemporaryDirectory() as directory:
            seed = Path(directory) / "seed.duckdb"
            seed.write_bytes(b"seed")
            with patch.object(m, "header", return_value={"effective": 64}):
                self.assertTrue(m.seed_identity(seed)["wal_absent"])
            seed.with_name(seed.name + ".wal").write_bytes(b"wal")
            with self.assertRaises(ValueError):
                m.seed_identity(seed)
            seed.with_name(seed.name + ".wal").unlink()
            with (
                patch.object(m, "header", return_value={"effective": 69}),
                self.assertRaises(ValueError),
            ):
                m.seed_identity(seed)

    def test_validate_mode_needs_only_report_and_returns_gate_status(self):
        with tempfile.TemporaryDirectory() as directory:
            report_path = Path(directory) / "report.json"
            arguments = campaign_args("/out")
            report_path.write_text(
                json.dumps({"requested_arguments": m.serialized_arguments(arguments)})
            )
            with (
                patch.object(m, "campaign_context", return_value={"context": True}),
                patch.object(m, "replay", return_value={"passed": True}),
            ):
                self.assertEqual(m.main(["--validate", str(report_path)]), 0)
            with (
                patch.object(m, "campaign_context", return_value={"context": True}),
                patch.object(m, "replay", return_value={"passed": False}),
            ):
                self.assertEqual(m.main(["--validate", str(report_path)]), 1)


if __name__ == "__main__":
    unittest.main()
