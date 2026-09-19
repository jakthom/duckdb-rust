"""Fail-closed Python SQLLogic proxy gate against both pinned C++ runners."""
import argparse
import copy
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
import platform
import sys
import tempfile
import time

import measure_sqllogic_performance as measure
import run_upstream
import sqllogic


ROOT = Path(__file__).resolve().parents[1]
SCHEMA = "sqllogic-proxy-v2"
ACCEPTANCE_SAMPLES = 21
ACCEPTANCE_WARMUPS = 3
RUNNERS = ("release", "development", "proxy")
PROXY_CONFIGURATION = {
    "threads": 1,
    "worker": "release-attested",
    "mode": "python-proxy",
    "timeout_seconds": 60,
}
EXPECTED_MANIFEST = "benchmark/a2_1_sqllogic_workloads.json"
EXPECTED_MANIFEST_SHA256 = "dbb7c05065fe5a40513b538c8200aef9f280e7bdcd8ee0a535c5d193eac05242"
EXPECTED_WORKLOADS = (
    {
        "id": "a2_1_scalar_comma_loop",
        "path": "test/performance/a2_1_scalar_comma_loop.test",
        "sha256": "e4e9ebe9c15dcab0ae9ce74d2fc4411e94fb6f4e70d8174a9341f7cd6f0f2adb",
        "bytes": 368,
        "proxy_records": 5,
    },
    {
        "id": "a2_1_scalar_comma_loop_large",
        "path": "test/performance/a2_1_scalar_comma_loop_large.test",
        "sha256": "bfc43aff382c8c0ab2f74d067df8a16f9a3ee0ac93579d797ff944d6725288db",
        "bytes": 420,
        "proxy_records": 5000,
    },
    {
        "id": "g01_2c_loop_conditions_concurrent_sessions_lifecycle",
        "path": "test/performance/g01_2c_loop_sessions.test",
        "sha256": "790954ce9e77bb67e7bb5b6e2fe06fcb1ed10a41e4d915d1c2265388e20ea974",
        "bytes": 862,
        "proxy_records": 16,
    },
    {
        "id": "a2_1_readonly_rejections",
        "path": "test/performance/a2_1_readonly_rejections.test",
        "sha256": "e6d07907d8d8d88cfd1ec0ebbf653d1bb9dece1012e2464dbc1154789c7bbd6b",
        "bytes": 400,
        "proxy_records": 4,
    },
    {
        "id": "a2_1_readonly_rejections_large",
        "path": "test/performance/a2_1_readonly_rejections_large.test",
        "sha256": "439bbb9a3bda14709f2af3026b0ea8d26066e817dc68c2f50dba032ce46a248e",
        "bytes": 415,
        "proxy_records": 2002,
    },
)
HELPERS = (
    "measure_sqllogic_proxy.py",
    "measure_sqllogic_performance.py",
    "run_upstream.py",
    "sqllogic.py",
    "source_identity.py",
    "reference_version.py",
    "upstream_suite.py",
)
CAMPAIGN_ARGUMENTS = {
    "test_root",
    "workloads",
    "worker",
    "worker_provenance",
    "release_cpp",
    "development_cpp",
    "release_source",
    "development_source",
    "release_build",
    "development_build",
    "release_cli",
    "development_cli",
}
SUCCESS_REPORT_KEYS = {
    "schema",
    "recorded_at",
    "requested_arguments",
    "samples",
    "warmups",
    "workloads",
    "platform",
    "machine",
    "passed",
    "at_parity_or_better_performance",
    "gate",
    "campaign",
    "references",
    "worker",
    "python",
    "execution_configuration",
    "proxy_configuration",
    "inputs_before",
    "inputs_after",
    "order",
}


def digest(path):
    with Path(path).open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def file_identity(path):
    path = Path(path).resolve(strict=True)
    if not path.is_file():
        raise ValueError(f"input is not a regular file: {path}")
    return {"path": str(path), "sha256": digest(path), "bytes": path.stat().st_size}


def save(report, path):
    Path(path).write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")


def json_safe(value):
    if isinstance(value, float) and not math.isfinite(value):
        return "Infinity" if value > 0 else "-Infinity"
    if isinstance(value, dict):
        return {key: json_safe(item) for key, item in value.items()}
    if isinstance(value, list):
        return [json_safe(item) for item in value]
    return value


def reserve_report(path):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x") as file:
        json.dump({"schema": SCHEMA, "passed": False, "status": "reserved"}, file)
        file.write("\n")


def workload_specs(root):
    root = Path(root).resolve(strict=True)
    result = []
    for expected in EXPECTED_WORKLOADS:
        path = (root / expected["path"]).resolve(strict=True)
        result.append(
            {
                "id": expected["id"],
                "path": expected["path"],
                "kind": "custom_comparable",
                "shared_test_dir": str(root),
                "shared_workload_path": str(path),
                "shared_workload_sha256": expected["sha256"],
                "shared_workload_bytes": expected["bytes"],
                "proxy_records_expected": expected["proxy_records"],
            }
        )
    return result


def validate_workload_population(manifest, test_root):
    root = Path(test_root).resolve(strict=True)
    if root != ROOT:
        raise ValueError(f"proxy acceptance test root must be {ROOT}")
    manifest = Path(manifest).resolve(strict=True)
    expected_manifest = (root / EXPECTED_MANIFEST).resolve(strict=True)
    if manifest != expected_manifest or digest(manifest) != EXPECTED_MANIFEST_SHA256:
        raise ValueError("proxy acceptance requires the frozen five-workload manifest")
    try:
        data = json.loads(manifest.read_text())
    except json.JSONDecodeError as error:
        raise ValueError("proxy workload manifest is not valid JSON") from error
    if set(data) != {"workloads"} or not isinstance(data["workloads"], list):
        raise ValueError("proxy manifest must contain only its workload population")
    declared = data["workloads"]
    expected = workload_specs(root)
    expected_declarations = [
        {"id": workload["id"], "path": workload["path"]}
        for workload in expected
    ]
    if declared != expected_declarations:
        raise ValueError("proxy manifest has missing, extra, or duplicate workloads")
    for workload in expected:
        path = Path(workload["shared_workload_path"])
        if (
            not path.is_file()
            or digest(path) != workload["shared_workload_sha256"]
            or path.stat().st_size != workload["shared_workload_bytes"]
        ):
            raise ValueError("proxy workload identity changed: " + workload["id"])
    return expected


def normalized_campaign(root, manifest, worker, provenance_path, references):
    return {
        "test_root": str(root),
        "workloads": str(manifest),
        "worker": str(worker),
        "worker_provenance": str(provenance_path),
        "release_cpp": references["release"]["unittest"],
        "development_cpp": references["development"]["unittest"],
        "release_source": references["release"]["source_directory"],
        "development_source": references["development"]["source_directory"],
        "release_build": references["release"]["build_directory"],
        "development_build": references["development"]["build_directory"],
        "release_cli": references["release"]["cli"]["path"],
        "development_cli": references["development"]["cli"]["path"],
    }


def snapshot_inputs(campaign, workloads, references, worker, provenance_path, provenance):
    scripts = Path(__file__).resolve().parent
    workload_files = {
        workload["id"]: file_identity(workload["shared_workload_path"])
        for workload in workloads
    }
    reference_files = {}
    for target, identity in references.items():
        reference_files[target] = {
            "unittest": file_identity(identity["unittest"]),
            "cli": file_identity(identity["cli"]["path"]),
            "cmake_cache": file_identity(identity["cmake_cache"]),
        }
    return {
        "python": {**file_identity(sys.executable), "version": sys.version},
        "helpers": {name: file_identity(scripts / name) for name in HELPERS},
        "manifest": file_identity(campaign["workloads"]),
        "workloads": workload_files,
        "worker": file_identity(worker),
        "worker_provenance": {
            **file_identity(provenance_path),
            "attestation": provenance,
        },
        "references": reference_files,
    }


def build_context(values):
    if isinstance(values, dict):
        if set(values) != CAMPAIGN_ARGUMENTS:
            raise ValueError("campaign arguments are missing, extra, or malformed")
        values = argparse.Namespace(**values)
    root = Path(values.test_root).resolve(strict=True)
    manifest = Path(values.workloads).resolve(strict=True)
    workloads = validate_workload_population(manifest, root)
    expected_worker = (ROOT / "target/release/duckdb-rust-test-worker").resolve(strict=True)
    worker = Path(values.worker).resolve(strict=True)
    if worker != expected_worker:
        raise ValueError("proxy acceptance requires the worktree release feedback worker")
    expected_provenance = run_upstream.worker_provenance_path(worker).resolve()
    provenance_path = Path(values.worker_provenance).resolve(strict=True)
    if provenance_path != expected_provenance:
        raise ValueError("proxy acceptance requires the worker's canonical provenance sidecar")
    checked_path, provenance = run_upstream.checked_worker_provenance(worker, provenance_path)
    references = {
        "release": measure.identity(
            "release",
            values.release_cpp,
            values.release_source,
            values.release_build,
            values.release_cli,
            root,
            workloads,
        ),
        "development": measure.identity(
            "development",
            values.development_cpp,
            values.development_source,
            values.development_build,
            values.development_cli,
            root,
            workloads,
        ),
    }
    campaign = normalized_campaign(
        root, manifest, worker, checked_path.resolve(strict=True), references
    )
    inputs = snapshot_inputs(
        campaign, workloads, references, worker, checked_path, provenance
    )
    return {
        "campaign": campaign,
        "workloads": workloads,
        "references": references,
        "worker": {
            "path": str(worker),
            "sha256": digest(worker),
            "provenance_path": str(checked_path.resolve(strict=True)),
            "provenance_sha256": digest(checked_path),
            "provenance": provenance,
        },
        "python": {
            "executable": str(Path(sys.executable).resolve(strict=True)),
            "binary_sha256": digest(sys.executable),
            "version": sys.version,
            "proxy": str(Path(__file__).resolve()),
            "proxy_sha256": digest(__file__),
        },
        "inputs": inputs,
    }


def context_from_report(report):
    campaign = report.get("campaign")
    if not isinstance(campaign, dict):
        raise ValueError("report has no frozen campaign arguments")
    return build_context(campaign)


def campaign_attestation(context):
    return {
        "worker_sha256": context["worker"]["sha256"],
        "source_sha256": context["worker"]["provenance"]["source_sha256"],
        "provenance_sha256": context["worker"]["provenance_sha256"],
    }


def proxy_command(context, relative):
    attestation = campaign_attestation(context)
    return [
        context["python"]["executable"],
        context["python"]["proxy"],
        "--once",
        "--worker",
        context["worker"]["path"],
        "--worker-provenance",
        context["worker"]["provenance_path"],
        "--test-root",
        context["campaign"]["test_root"],
        "--path",
        relative,
        "--timeout",
        str(PROXY_CONFIGURATION["timeout_seconds"]),
        "--campaign-worker-sha256",
        attestation["worker_sha256"],
        "--campaign-source-sha256",
        attestation["source_sha256"],
        "--campaign-provenance-sha256",
        attestation["provenance_sha256"],
    ]


def expected_commands(context, workload):
    root = context["campaign"]["test_root"]
    relative = workload["path"]
    suffix = [
        "--test-dir",
        root,
        relative,
        "--use-colour",
        "no",
        "--durations",
        "no",
        "--single-threaded",
    ]
    return {
        "release": [context["references"]["release"]["unittest"], *suffix],
        "development": [context["references"]["development"]["unittest"], *suffix],
        "proxy": proxy_command(context, relative),
    }


def validate_sample(sample, target, command):
    measure.validate_observation(sample)
    if sample["command"] != command:
        raise ValueError("observation command does not match independently derived command")
    label = "rust" if target == "proxy" else "cpp"
    records = measure.records_from_output(sample["stdout"], label)
    if records != sample["records"]:
        raise ValueError("claimed PASS count does not match runner stdout")


def expected_schedule():
    schedule = {phase: {target: [] for target in RUNNERS} for phase in ("warmup_observations", "observations")}
    sequence = 0
    for round_number in range(ACCEPTANCE_WARMUPS + ACCEPTANCE_SAMPLES):
        phase = "warmup_observations" if round_number < ACCEPTANCE_WARMUPS else "observations"
        names = list(RUNNERS)
        names = names[round_number % len(names):] + names[:round_number % len(names)]
        for target in names:
            schedule[phase][target].append(
                {
                    "round": round_number,
                    "sequence": sequence,
                    "runner": target,
                    "phase": "warmup" if phase == "warmup_observations" else "sample",
                }
            )
            sequence += 1
    return schedule


def validate_report(report, context=None):
    """Revalidate identities, raw evidence, and Gate P from serialized bytes."""
    if not isinstance(report, dict) or report.get("schema") != SCHEMA:
        raise ValueError("invalid proxy evidence schema")
    if any(key in report for key in ("error", "failed_invocation", "failure_phase")):
        raise ValueError("failed or partial campaign cannot validate as acceptance evidence")
    if set(report) != SUCCESS_REPORT_KEYS:
        raise ValueError("successful proxy report has missing or unsupported top-level fields")
    try:
        recorded_at = datetime.fromisoformat(report["recorded_at"])
    except (TypeError, ValueError) as error:
        raise ValueError("proxy report has an invalid recording timestamp") from error
    if recorded_at.tzinfo is None:
        raise ValueError("proxy report recording timestamp must include a timezone")
    if not isinstance(report.get("platform"), str) or not report["platform"]:
        raise ValueError("proxy report has no platform identity")
    if not isinstance(report.get("machine"), str) or not report["machine"]:
        raise ValueError("proxy report has no machine identity")
    if report.get("samples") != ACCEPTANCE_SAMPLES or report.get("warmups") != ACCEPTANCE_WARMUPS:
        raise ValueError("invalid proxy sample or warmup count")
    if report.get("execution_configuration") != measure.SERIAL_CONFIGURATION:
        raise ValueError("invalid matched serial execution configuration")
    if report.get("proxy_configuration") != PROXY_CONFIGURATION:
        raise ValueError("invalid proxy serial configuration")
    if report.get("order") != "paired alternating release, development, proxy":
        raise ValueError("invalid runner order declaration")
    context = context_from_report(report) if context is None else context
    for key in ("campaign", "references", "worker", "python"):
        if report.get(key) != context[key]:
            raise ValueError(f"stale or tampered {key} identity")
    if report.get("requested_arguments") != context["campaign"]:
        raise ValueError("requested worker, provenance, or reference arguments are stale or tampered")
    if report.get("inputs_before") != context["inputs"] or report.get("inputs_after") != context["inputs"]:
        raise ValueError("campaign input files are stale, tampered, or changed during measurement")
    entries = report.get("workloads")
    expected = context["workloads"]
    if not isinstance(entries, list) or len(entries) != len(expected):
        raise ValueError("report has missing, extra, or duplicate workloads")
    identifiers = [entry.get("id") if isinstance(entry, dict) else None for entry in entries]
    if identifiers != [workload["id"] for workload in expected] or len(set(identifiers)) != len(identifiers):
        raise ValueError("report has missing, extra, reordered, or duplicate workload IDs")
    raw = {}
    schedule = expected_schedule()
    for entry, workload in zip(entries, expected):
        expected_keys = {
            *workload,
            "warmup_observations",
            "observations",
            "pass_counts",
        }
        if set(entry) != expected_keys:
            raise ValueError("raw workload contains missing or unsupported fields: " + workload["id"])
        if any(entry.get(key) != value for key, value in workload.items()):
            raise ValueError("raw workload identity changed: " + workload["id"])
        commands = expected_commands(context, workload)
        phases = (("warmup_observations", ACCEPTANCE_WARMUPS), ("observations", ACCEPTANCE_SAMPLES))
        populations = {}
        for phase, count in phases:
            observations = entry.get(phase)
            if not isinstance(observations, dict) or set(observations) != set(RUNNERS):
                raise ValueError("raw workload is missing a runner population")
            for target in RUNNERS:
                samples = observations[target]
                if not isinstance(samples, list) or len(samples) != count:
                    raise ValueError(f"incomplete {phase}: {workload['id']}/{target}")
                for sample, expected_metadata in zip(samples, schedule[phase][target]):
                    validate_sample(sample, target, commands[target])
                    if any(sample.get(key) != value for key, value in expected_metadata.items()):
                        raise ValueError("raw observation violates the matched serial schedule")
                populations.setdefault(target, []).extend(samples)
        pass_counts = {}
        for target, samples in populations.items():
            counts = {sample["records"] for sample in samples}
            if len(counts) != 1:
                raise ValueError("unstable runner PASS count: " + workload["id"] + "/" + target)
            pass_counts[target] = next(iter(counts))
        if pass_counts["proxy"] != workload["proxy_records_expected"]:
            raise ValueError("proxy PASS count changed: " + workload["id"])
        if entry.get("pass_counts") != pass_counts:
            raise ValueError("recorded PASS counts do not match raw observations")
        raw[workload["id"]] = entry["observations"]
    release = {
        "workloads": [
            {**workload, "cpp": raw[workload["id"]]["release"], "rust": raw[workload["id"]]["proxy"]}
            for workload in expected
        ]
    }
    development = {
        "workloads": [
            {**workload, "cpp": raw[workload["id"]]["development"], "rust": raw[workload["id"]]["proxy"]}
            for workload in expected
        ]
    }
    gate = json_safe(measure.gate(release, development, expected))
    if report.get("gate") != gate:
        raise ValueError("stored gate does not match independently recomputed Gate P")
    if report.get("passed") is not gate["passed"]:
        raise ValueError("stored pass verdict does not match recomputed Gate P")
    if report.get("at_parity_or_better_performance") is not gate["at_parity_or_better_performance"]:
        raise ValueError("stored performance verdict does not match recomputed Gate P")
    return gate


def checked_campaign_attestation(worker, worker_provenance, attestation):
    expected_keys = {"worker_sha256", "source_sha256", "provenance_sha256"}
    if not isinstance(attestation, dict) or set(attestation) != expected_keys:
        raise ValueError("campaign worker attestation is incomplete")
    if any(
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
        for value in attestation.values()
    ):
        raise ValueError("campaign worker attestation has a malformed SHA-256")
    worker = Path(worker).resolve(strict=True)
    provenance_path = Path(worker_provenance).resolve(strict=True)
    canonical_path = run_upstream.worker_provenance_path(worker).resolve(strict=True)
    if provenance_path != canonical_path:
        raise ValueError("--once requires the canonical worker provenance sidecar")
    if digest(provenance_path) != attestation["provenance_sha256"]:
        raise ValueError("campaign worker provenance sidecar changed")
    try:
        provenance = json.loads(provenance_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError("campaign worker provenance sidecar is unreadable") from error
    expected = {
        "profile": "release",
        "source_sha256": attestation["source_sha256"],
        "binary_sha256": attestation["worker_sha256"],
    }
    if provenance != expected:
        raise ValueError("campaign worker provenance sidecar is stale or tampered")
    return provenance_path, provenance


def once(worker, worker_provenance, root, relative, timeout=60, attestation=None):
    if timeout != PROXY_CONFIGURATION["timeout_seconds"]:
        raise ValueError("--once requires the frozen 60-second timeout")
    root = Path(root).resolve(strict=True)
    workloads = {
        workload["path"]: workload
        for workload in validate_workload_population(root / EXPECTED_MANIFEST, root)
    }
    if relative not in workloads:
        raise ValueError("--once path is not in the frozen proxy workload population")
    worker = Path(worker).resolve(strict=True)
    expected_worker = (ROOT / "target/release/duckdb-rust-test-worker").resolve(strict=True)
    if worker != expected_worker:
        raise ValueError("--once requires the worktree release feedback worker")
    if attestation is None:
        provenance_path, _ = run_upstream.checked_worker_provenance(
            worker, worker_provenance
        )
    else:
        provenance_path, _ = checked_campaign_attestation(
            worker, worker_provenance, attestation
        )
    if provenance_path.resolve(strict=True) != run_upstream.worker_provenance_path(worker).resolve(strict=True):
        raise ValueError("--once requires the canonical worker provenance sidecar")
    path = (root / relative).resolve(strict=True)
    records = sqllogic.parse(path.read_text())
    deadline = time.monotonic() + timeout
    with tempfile.TemporaryDirectory(prefix="ddb-proxy-measure-") as scratch:
        engine = run_upstream.RustEngine(worker, scratch, deadline)
        try:
            runner = sqllogic.Runner(
                engine,
                {
                    "{TEST_DIR}": scratch,
                    "__TEST_DIR__": scratch,
                    "{WORKING_DIRECTORY}": scratch,
                    "__WORKING_DIRECTORY__": scratch,
                    "{TEST_NAME}": relative,
                    "{BASE_TEST_NAME}": relative.replace("/", "_"),
                    "__SOURCE_DIR__": str(root),
                },
            )
            runner.run(records)
            expected = workloads[relative]["proxy_records_expected"]
            if runner.passed != expected or runner.skipped:
                raise ValueError("proxy workload PASS count differs from frozen untimed expectation")
            return runner.passed
        finally:
            engine.close()


def requested_arguments(args):
    names = sorted(CAMPAIGN_ARGUMENTS)
    return {name: str(getattr(args, name)) for name in names}


def campaign(args):
    reserve_report(args.report)
    report = {
        "schema": SCHEMA,
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "requested_arguments": requested_arguments(args),
        "samples": args.samples,
        "warmups": args.warmups,
        "workloads": [],
        "platform": platform.platform(),
        "machine": platform.machine(),
        "passed": False,
        "at_parity_or_better_performance": False,
        "gate": {
            "workloads": [],
            "passed": False,
            "at_parity_or_better_performance": False,
        },
    }
    phase = "setup"
    context = None
    try:
        if args.samples != ACCEPTANCE_SAMPLES or args.warmups != ACCEPTANCE_WARMUPS:
            raise ValueError("acceptance requires exactly 21 samples and 3 warmups")
        peers = measure.active_peers()
        if peers:
            raise RuntimeError("quiet-host acceptance blocked by active peers: " + "; ".join(peers))
        context = build_context(args)
        report.update(
            {
                "requested_arguments": copy.deepcopy(context["campaign"]),
                "campaign": copy.deepcopy(context["campaign"]),
                "references": copy.deepcopy(context["references"]),
                "worker": copy.deepcopy(context["worker"]),
                "python": copy.deepcopy(context["python"]),
                "execution_configuration": copy.deepcopy(measure.SERIAL_CONFIGURATION),
                "proxy_configuration": copy.deepcopy(PROXY_CONFIGURATION),
                "inputs_before": copy.deepcopy(context["inputs"]),
                "order": "paired alternating release, development, proxy",
            }
        )
        for workload in context["workloads"]:
            commands = expected_commands(context, workload)
            entry = {
                **workload,
                "warmup_observations": {name: [] for name in RUNNERS},
                "observations": {name: [] for name in RUNNERS},
            }
            report["workloads"].append(entry)
            for round_number in range(args.warmups + args.samples):
                phase = f"{workload['id']}/" + ("warmup" if round_number < args.warmups else "sample")
                names = list(RUNNERS)
                names = names[round_number % len(names):] + names[:round_number % len(names)]
                for name in names:
                    sample = measure.run_timed(commands[name], "rust" if name == "proxy" else "cpp")
                    target = "warmup_observations" if round_number < args.warmups else "observations"
                    sample.update(
                        {
                            "round": round_number,
                            "sequence": sum(
                                len(values)
                                for phase_name in ("warmup_observations", "observations")
                                for values in entry[phase_name].values()
                            ),
                            "runner": name,
                            "phase": "warmup" if target == "warmup_observations" else "sample",
                        }
                    )
                    entry[target][name].append(sample)
            entry["pass_counts"] = {
                name: entry["observations"][name][0]["records"]
                for name in RUNNERS
            }
        phase = "input-revalidation"
        final_context = build_context(context["campaign"])
        if final_context != context:
            raise ValueError("campaign inputs changed during measurement")
        report["inputs_after"] = copy.deepcopy(final_context["inputs"])
        phase = "gate"
        raw = {entry["id"]: entry["observations"] for entry in report["workloads"]}
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
        report["gate"] = json_safe(measure.gate(release, development, context["workloads"]))
        report["passed"] = report["gate"]["passed"]
        report["at_parity_or_better_performance"] = report["gate"]["at_parity_or_better_performance"]
        validate_report(report, context)
    except Exception as error:
        report["error"] = str(error)
        report["failure_phase"] = phase
        report["gate"] = {
            "workloads": [],
            "passed": False,
            "at_parity_or_better_performance": False,
            "error": str(error),
        }
        if isinstance(error, measure.SampleFailure):
            report["failed_invocation"] = error.details
    save(report, args.report)
    return report


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--once", action="store_true")
    result.add_argument("--validate", type=Path)
    result.add_argument("--worker", type=Path)
    result.add_argument("--worker-provenance", type=Path)
    result.add_argument("--test-root", type=Path)
    result.add_argument("--path")
    result.add_argument("--timeout", type=int, default=PROXY_CONFIGURATION["timeout_seconds"])
    result.add_argument("--campaign-worker-sha256")
    result.add_argument("--campaign-source-sha256")
    result.add_argument("--campaign-provenance-sha256")
    result.add_argument("--workloads", type=Path)
    result.add_argument("--release-cpp", type=Path)
    result.add_argument("--development-cpp", type=Path)
    result.add_argument("--release-source", type=Path)
    result.add_argument("--development-source", type=Path)
    result.add_argument("--release-build", type=Path)
    result.add_argument("--development-build", type=Path)
    result.add_argument("--release-cli", type=Path)
    result.add_argument("--development-cli", type=Path)
    result.add_argument("--samples", type=int, default=ACCEPTANCE_SAMPLES)
    result.add_argument("--warmups", type=int, default=ACCEPTANCE_WARMUPS)
    result.add_argument("--report", type=Path)
    return result


def main(argv=None):
    argument_parser = parser()
    args = argument_parser.parse_args(argv)
    if args.validate:
        try:
            report = json.loads(args.validate.read_text())
            gate = validate_report(report)
        except (OSError, json.JSONDecodeError, ValueError) as error:
            print(json.dumps({"passed": False, "report": str(args.validate), "error": str(error)}))
            return 1
        print(json.dumps({"passed": gate["passed"], "report": str(args.validate), "error": None}))
        return 0 if gate["passed"] else 1
    if args.once:
        required = ("worker", "worker_provenance", "test_root", "path")
        if any(getattr(args, name) is None for name in required):
            argument_parser.error("--once requires --worker, --worker-provenance, --test-root, and --path")
        attestation_values = {
            "worker_sha256": args.campaign_worker_sha256,
            "source_sha256": args.campaign_source_sha256,
            "provenance_sha256": args.campaign_provenance_sha256,
        }
        present = [value is not None for value in attestation_values.values()]
        if any(present) and not all(present):
            argument_parser.error("campaign worker attestation must be supplied in full")
        count = once(
            args.worker,
            args.worker_provenance,
            args.test_root,
            args.path,
            args.timeout,
            attestation_values if all(present) else None,
        )
        print(f"PASS {args.path} ({count} records)\n{count} records passed; 0 skipped")
        return 0
    required = (*CAMPAIGN_ARGUMENTS, "report")
    if any(getattr(args, name) is None for name in required):
        argument_parser.error("campaign arguments are incomplete")
    result = campaign(args)
    print(json.dumps({"passed": result["passed"], "report": str(args.report), "error": result.get("error")}))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
