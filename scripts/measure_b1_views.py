"""Fail-closed, dual-pin B1 durable process measurement (timing requires --run)."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import time

from native_version_reference import header
from reference_version import ROOT, TARGETS, require_checkout, require_reference
from source_identity import vendored_sources


METRICS = ("wall_ns", "cpu_ns", "max_rss_bytes", "block_input", "block_output")
TARGETS_ORDER = ("release", "development", "rust")
PHASES = ("publish", "reopen_query_drop")
BUILD_COMMAND = (
    "cargo",
    "build",
    "--offline",
    "--release",
    "--no-default-features",
    "--bin",
    "duckdb-rust",
)
EXPECTED = {
    "schema": 1,
    "id": "b1_view_durable_native",
    "rows": 10000,
    "checksum": 49995000,
    "samples": 21,
    "warmups": 3,
    "metrics": [*METRICS, "throughput"],
    "seed_table": "b1_seed",
    "column": "i",
    "configurations": ["checkpoint", "wal"],
    "workloads": ["view_cycle", "direct_table_publication"],
}
HELPERS = (
    "measure_b1_views.py",
    "reference_version.py",
    "native_version_reference.py",
    "source_identity.py",
)
RUST_ATTESTATION_FIELDS = {
    "schema",
    "status",
    "recorded_at",
    "root",
    "git_revision",
    "profile",
    "default_features",
    "build_command",
    "source_before",
    "source_after",
    "toolchain_before",
    "toolchain_after",
    "build_environment_before",
    "build_environment_after",
    "binary_before",
    "binary_after",
}
BUILD_ENVIRONMENT_KEYS = (
    "AR",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
    "CARGO_PROFILE_RELEASE_DEBUG",
    "CARGO_PROFILE_RELEASE_LTO",
    "CARGO_PROFILE_RELEASE_OPT_LEVEL",
    "CARGO_PROFILE_RELEASE_PANIC",
    "CARGO_PROFILE_RELEASE_STRIP",
    "CC",
    "CFLAGS",
    "CXX",
    "CXXFLAGS",
    "DEVELOPER_DIR",
    "MACOSX_DEPLOYMENT_TARGET",
    "RUSTC",
    "RUSTFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "SDKROOT",
    "SOURCE_DATE_EPOCH",
)
BUILD_ENVIRONMENT_PREFIXES = ("CARGO_PROFILE_RELEASE_", "CARGO_TARGET_")
RUST_SERIAL_IMPLEMENTATION = (
    "src/main/connection.rs",
    "src/main/database.rs",
    "src/parallel/mod.rs",
    "tools/shell/main.rs",
)


class SampleFailure(RuntimeError):
    def __init__(self, message, observation):
        super().__init__(message)
        self.observation = observation


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def file_id(path):
    path = Path(path).resolve(strict=True)
    return {"path": str(path), "sha256": digest(path), "bytes": path.stat().st_size}


def workload_manifest(path):
    path = Path(path).resolve(strict=True)
    data = json.loads(path.read_text())
    if data != EXPECTED:
        raise ValueError("B1 durable workload manifest is changed or malformed")
    return {"path": str(path), "sha256": digest(path), "data": data}


def rust_source_paths():
    paths = [
        ROOT / "Cargo.toml",
        ROOT / "Cargo.lock",
        ROOT / ".cargo/config.toml",
        ROOT / "tools/shell/main.rs",
        *(ROOT / "src").rglob("*.rs"),
        *vendored_sources(ROOT),
    ]
    return sorted({path.resolve(strict=True) for path in paths})


def rust_source_identity():
    rows = []
    combined = hashlib.sha256()
    for path in rust_source_paths():
        relative = str(path.relative_to(ROOT))
        sha256 = digest(path)
        rows.append({"path": relative, "sha256": sha256, "bytes": path.stat().st_size})
        combined.update(relative.encode())
        combined.update(b"\0")
        combined.update(bytes.fromhex(sha256))
    return {"sha256": combined.hexdigest(), "count": len(rows), "files": rows}


def toolchain_identity(check_output=subprocess.check_output):
    cargo = shutil.which("cargo")
    rustc = shutil.which("rustc")
    if cargo is None or rustc is None:
        raise FileNotFoundError("cargo and rustc are required")
    return {
        "cargo_path": str(Path(cargo).resolve(strict=True)),
        "cargo": check_output(["cargo", "--version", "--verbose"], text=True).strip(),
        "rustc_path": str(Path(rustc).resolve(strict=True)),
        "rustc": check_output(["rustc", "-Vv"], text=True).strip(),
    }


def git_revision(check_output=subprocess.check_output):
    return check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()


def build_environment():
    keys = set(BUILD_ENVIRONMENT_KEYS)
    keys.update(
        key
        for key in os.environ
        if any(key.startswith(prefix) for prefix in BUILD_ENVIRONMENT_PREFIXES)
    )
    return {key: os.environ.get(key) for key in sorted(keys)}


def serial_configuration():
    return {
        "cpp": {"threads": 1, "sql": "SET threads=1"},
        "rust": {
            "threads": 1,
            "scheduler": "inline",
            "implementation": {
                path: file_id(ROOT / path) for path in RUST_SERIAL_IMPLEMENTATION
            },
        },
    }


def prepare_rust_provenance(path, execute=subprocess.run):
    path = Path(path)
    if path.exists():
        raise FileExistsError("preserve prior Rust attestation: output exists")
    path.parent.mkdir(parents=True, exist_ok=True)
    binary = (ROOT / "target/release/duckdb-rust").resolve()
    source_before = rust_source_identity()
    toolchain_before = toolchain_identity()
    environment_before = build_environment()
    binary_before = file_id(binary) if binary.is_file() else None
    base = {
        "schema": 1,
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "root": str(ROOT.resolve()),
        "git_revision": git_revision(),
        "profile": "release",
        "default_features": False,
        "build_command": [*BUILD_COMMAND],
        "source_before": source_before,
        "toolchain_before": toolchain_before,
        "build_environment_before": environment_before,
        "binary_before": binary_before,
    }
    try:
        execute([*BUILD_COMMAND], cwd=ROOT, check=True)
        source_after = rust_source_identity()
        toolchain_after = toolchain_identity()
        environment_after = build_environment()
        if (
            source_before != source_after
            or toolchain_before != toolchain_after
            or environment_before != environment_after
        ):
            raise RuntimeError(
                "source, toolchain, or build environment changed during release-shell build"
            )
        provenance = {
            **base,
            "status": "complete",
            "source_after": source_after,
            "toolchain_after": toolchain_after,
            "build_environment_after": environment_after,
            "binary_after": file_id(binary),
        }
    except Exception as error:
        failure = {
            **base,
            "status": "failed",
            "error": {"type": type(error).__name__, "message": str(error)},
            "source_after": rust_source_identity(),
            "toolchain_after": toolchain_identity(),
            "build_environment_after": build_environment(),
            "binary_after": file_id(binary) if binary.is_file() else None,
        }
        with path.open("x") as stream:
            json.dump(failure, stream, indent=2)
            stream.write("\n")
        raise
    with path.open("x") as stream:
        json.dump(provenance, stream, indent=2)
        stream.write("\n")
    return provenance


def rust_identity(binary, provenance_path):
    binary = Path(binary).resolve(strict=True)
    canonical = (ROOT / "target/release/duckdb-rust").resolve(strict=True)
    if binary != canonical:
        raise ValueError("Rust timing requires the canonical release shell")
    provenance_path = Path(provenance_path).resolve(strict=True)
    provenance = json.loads(provenance_path.read_text())
    if not isinstance(provenance, dict) or set(provenance) != RUST_ATTESTATION_FIELDS:
        raise ValueError("Rust release-shell attestation fields changed")
    current_source = rust_source_identity()
    current_toolchain = toolchain_identity()
    current_environment = build_environment()
    expected = {
        "schema": 1,
        "status": "complete",
        "root": str(ROOT.resolve()),
        "profile": "release",
        "default_features": False,
        "build_command": [*BUILD_COMMAND],
        "source_before": current_source,
        "source_after": current_source,
        "toolchain_before": current_toolchain,
        "toolchain_after": current_toolchain,
        "build_environment_before": current_environment,
        "build_environment_after": current_environment,
        "binary_after": file_id(binary),
    }
    if (
        not isinstance(provenance.get("recorded_at"), str)
        or not provenance["recorded_at"]
        or not isinstance(provenance.get("git_revision"), str)
        or not provenance["git_revision"]
        or any(provenance.get(key) != value for key, value in expected.items())
    ):
        raise ValueError("Rust release-shell attestation is stale or noncanonical")
    return {
        "binary": file_id(binary),
        "provenance": file_id(provenance_path),
        "attestation": provenance,
    }


def parse_time(stderr):
    usage = re.search(
        r"(?m)^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys\s*$",
        stderr,
    )
    if not usage:
        raise ValueError("/usr/bin/time -l omitted CPU timing")
    result = {
        "cpu_ns": int((float(usage.group(2)) + float(usage.group(3))) * 1_000_000_000)
    }
    labels = (
        ("max_rss_bytes", "maximum resident set size"),
        ("block_input", "block input operations"),
        ("block_output", "block output operations"),
    )
    for key, label in labels:
        found = re.search(rf"(?m)^\s*(\d+)\s+{re.escape(label)}\s*$", stderr)
        if not found:
            raise ValueError(f"/usr/bin/time -l omitted {key}")
        result[key] = int(found.group(1))
    return result


def timed(command_parts, phase, execute=subprocess.run):
    if platform.system() != "Darwin":
        raise RuntimeError("durable acceptance requires macOS /usr/bin/time -l")
    command_parts = [str(part) for part in command_parts]
    started = time.perf_counter_ns()
    result = execute(
        ["/usr/bin/time", "-l", *command_parts], text=True, capture_output=True
    )
    row = {
        "command": command_parts,
        "phase": phase,
        "returncode": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "wall_ns": time.perf_counter_ns() - started,
    }
    if result.returncode:
        raise SampleFailure(f"{phase} CLI failed", row)
    try:
        row.update(parse_time(result.stderr))
    except ValueError as error:
        raise SampleFailure(str(error), row) from error
    if row["wall_ns"] <= 0 or row["cpu_ns"] < 0 or row["max_rss_bytes"] <= 0:
        raise SampleFailure("incomplete timing observation", row)
    return row


def observe(command_parts, label, execute=subprocess.run):
    command_parts = [str(part) for part in command_parts]
    result = execute(command_parts, text=True, capture_output=True)
    return {
        "command": command_parts,
        "label": label,
        "returncode": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
    }


def sqls(workload, mode):
    if workload == "view_cycle":
        create = "CREATE VIEW b1_public AS SELECT i FROM b1_seed"
        drop = "DROP VIEW b1_public"
    elif workload == "direct_table_publication":
        create = "CREATE TABLE b1_public AS SELECT i FROM b1_seed"
        drop = "DROP TABLE b1_public"
    else:
        raise ValueError("unknown durable workload")
    checkpoint = " CHECKPOINT;" if mode == "checkpoint" else ""
    return {
        "publish": (f"BEGIN TRANSACTION; {create}; COMMIT;{checkpoint}"),
        "reopen_query_drop": (
            "SELECT count(*) AS row_count, "
            "coalesce(sum(i),0) AS checksum FROM b1_public; "
            f"BEGIN TRANSACTION; {drop}; COMMIT;"
        ),
    }


def seed_sql():
    return "SELECT count(*) AS row_count, coalesce(sum(i),0) AS checksum FROM b1_seed"


def absent_sql():
    return "SELECT * FROM b1_public"


def command(engine, database, mode, sql, readonly=False):
    database = str(database)
    if re.search(r"(?i)\bset\s+threads\s*=", sql):
        raise ValueError("thread configuration is selected by the adapter")
    if engine["kind"] == "cpp":
        sql = "SET threads=1; " + sql
        prefix = "PRAGMA disable_checkpoint_on_shutdown; " if mode == "wal" else ""
        return [
            engine["binary"],
            database,
            "-json",
            *(["-readonly"] if readonly else []),
            "-c",
            prefix + sql,
        ]
    return [
        engine["binary"],
        database,
        *(["--read-only"] if readonly else ["--durability", mode]),
        "--json",
        "-c",
        sql,
    ]


def json_row(stdout):
    remaining = stdout.strip()
    values = []
    decoder = json.JSONDecoder()
    try:
        while remaining:
            value, consumed = decoder.raw_decode(remaining)
            values.append(value)
            remaining = remaining[consumed:].lstrip()
    except json.JSONDecodeError as error:
        raise ValueError("CLI did not emit JSON") from error
    if not values or any(value != [] for value in values[:-1]):
        raise ValueError("CLI emitted a nonempty setup result")
    data = values[-1]
    wanted = {"row_count", "checksum"}
    if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0], dict):
        raise ValueError("CLI JSON must contain exactly one row")
    if set(data[0]) != wanted:
        raise ValueError("CLI JSON metadata changed")
    row_count = data[0]["row_count"]
    checksum = data[0]["checksum"]
    checksum_matches = (type(checksum) is int and checksum == 49995000) or (
        type(checksum) is str and checksum == "49995000"
    )
    if type(row_count) is not int or row_count != 10000 or not checksum_matches:
        raise ValueError("unexpected checksum JSON")


def absence_error(row):
    if row["returncode"] == 0:
        raise ValueError("dropped publication remained readable")
    pattern = re.compile(
        r"(?is)\b(?:table|view)(?:\s+with\s+name)?\s+"
        r"[\"'`]?((?:main\.)?b1_public)[\"'`]?\s+"
        r"(?:does\s+not\s+exist|not\s+found)\b"
    )
    if not pattern.search(row["stderr"]):
        raise ValueError("absence error did not name missing b1_public")


def artifact_files(database):
    database = Path(database)
    result = {}
    for suffix in ("", ".wal"):
        path = database.with_name(database.name + suffix)
        result[suffix] = file_id(path) if path.exists() else None
    return result


def fail_sample(sample, database, message, detail=None):
    sample["failure"] = {"message": message, "detail": detail}
    try:
        sample["artifacts"] = artifact_files(database)
    except OSError as error:
        sample["artifact_error"] = str(error)
    raise SampleFailure(message, sample)


def one_sample(engine, mode, workload, seed, database):
    seed = Path(seed)
    database = Path(database)
    sample = {
        "seed_sha256": None,
        "seed_copy": None,
        "seed_verification": None,
        "phases": [],
        "absence": None,
        "seed_after": None,
        "wal_after_publish": None,
        "aggregate": None,
        "artifacts": None,
    }
    try:
        sample["seed_sha256"] = digest(seed)
        shutil.copyfile(seed, database)
        sample["seed_copy"] = {
            "sha256": digest(database),
            "bytes": database.stat().st_size,
            "wal_absent": not database.with_name(database.name + ".wal").exists(),
        }
        if sample["seed_copy"]["sha256"] != sample["seed_sha256"]:
            fail_sample(sample, database, "seed copy identity changed")
        if not sample["seed_copy"]["wal_absent"]:
            fail_sample(sample, database, "seed copy unexpectedly has a WAL")

        seed_observation = observe(
            command(engine, database, mode, seed_sql(), readonly=True),
            "seed_verification",
        )
        sample["seed_verification"] = seed_observation
        if seed_observation["returncode"]:
            fail_sample(
                sample, database, "seed verification CLI failed", seed_observation
            )
        try:
            json_row(seed_observation["stdout"])
        except ValueError as error:
            fail_sample(sample, database, str(error), seed_observation)
        if (
            digest(database) != sample["seed_sha256"]
            or database.with_name(database.name + ".wal").exists()
        ):
            fail_sample(
                sample, database, "read-only seed verification changed the seed copy"
            )

        for phase, sql in sqls(workload, mode).items():
            try:
                row = timed(command(engine, database, mode, sql), phase)
            except SampleFailure as error:
                sample["phases"].append(error.observation)
                fail_sample(sample, database, str(error), error.observation)
            sample["phases"].append(row)
            if phase == "publish" and mode == "wal":
                wal = database.with_name(database.name + ".wal")
                if not wal.is_file() or wal.stat().st_size == 0:
                    fail_sample(
                        sample, database, "WAL was not retained before reopen", row
                    )
                sample["wal_after_publish"] = file_id(wal)
            if phase == "reopen_query_drop":
                try:
                    json_row(row["stdout"])
                except ValueError as error:
                    fail_sample(sample, database, str(error), row)

        sample["absence"] = observe(
            command(engine, database, mode, absent_sql(), readonly=True), "absence"
        )
        sample["seed_after"] = observe(
            command(engine, database, mode, seed_sql(), readonly=True), "seed_after"
        )
        absence_error(sample["absence"])
        if sample["seed_after"]["returncode"]:
            raise ValueError("final seed verification CLI failed")
        json_row(sample["seed_after"]["stdout"])

        sample["aggregate"] = {
            metric: (
                max(row[metric] for row in sample["phases"])
                if metric == "max_rss_bytes"
                else sum(row[metric] for row in sample["phases"])
            )
            for metric in METRICS
        }
        sample["artifacts"] = artifact_files(database)
        return sample
    except SampleFailure:
        raise
    except Exception as error:
        fail_sample(sample, database, str(error), {"type": type(error).__name__})


def validate_observation(row, command_parts, label):
    if set(row) != {"command", "label", "returncode", "stdout", "stderr"}:
        raise ValueError(f"{label} observation fields changed")
    if row["command"] != [str(part) for part in command_parts] or row["label"] != label:
        raise ValueError(f"{label} command is tampered")
    if type(row["returncode"]) is not int:
        raise ValueError(f"{label} return code is missing")
    if not isinstance(row["stdout"], str) or not isinstance(row["stderr"], str):
        raise ValueError(f"{label} output is missing")


def validate_timed(row, command_parts, phase):
    wanted_fields = {
        "command",
        "phase",
        "returncode",
        "stdout",
        "stderr",
        *METRICS,
    }
    if set(row) != wanted_fields:
        raise ValueError("timed observation fields changed")
    if row["command"] != [str(part) for part in command_parts] or row["phase"] != phase:
        raise ValueError("timed observation command is tampered")
    if row["returncode"] != 0:
        raise ValueError("timed observation did not succeed")
    if not isinstance(row["stdout"], str) or not isinstance(row["stderr"], str):
        raise ValueError("timed observation output is missing")
    parsed = parse_time(row["stderr"])
    if type(row["wall_ns"]) is not int or row["wall_ns"] <= 0:
        raise ValueError("wall metric is missing")
    for metric in METRICS[1:]:
        if type(row[metric]) is not int or row[metric] < 0:
            raise ValueError("timed metric is missing")
        if row[metric] != parsed[metric]:
            raise ValueError("timed metric is tampered")
    if row["max_rss_bytes"] <= 0:
        raise ValueError("peak RSS is missing")


def validate_artifacts(artifacts, database):
    if not isinstance(artifacts, dict) or set(artifacts) != {"", ".wal"}:
        raise ValueError("artifact evidence changed")
    database = Path(database)
    for suffix, identity in artifacts.items():
        path = database.with_name(database.name + suffix)
        if identity is None:
            if suffix == "":
                raise ValueError("sample database artifact is missing")
            if path.exists():
                raise ValueError("unrecorded sample artifact exists")
        elif identity != file_id(path):
            raise ValueError("sample artifact identity changed")


def validate_sample(sample, engine, mode, workload, database, seed_identity_value):
    wanted_fields = {
        "seed_sha256",
        "seed_copy",
        "seed_verification",
        "phases",
        "absence",
        "seed_after",
        "wal_after_publish",
        "aggregate",
        "artifacts",
    }
    if not isinstance(sample, dict) or set(sample) != wanted_fields:
        raise ValueError("sample fields changed")
    expected_seed_copy = {
        "sha256": seed_identity_value["sha256"],
        "bytes": seed_identity_value["bytes"],
        "wal_absent": True,
    }
    if (
        sample["seed_sha256"] != seed_identity_value["sha256"]
        or sample["seed_copy"] != expected_seed_copy
    ):
        raise ValueError("seed copy identity changed")

    validate_observation(
        sample["seed_verification"],
        command(engine, database, mode, seed_sql(), readonly=True),
        "seed_verification",
    )
    if sample["seed_verification"]["returncode"] != 0:
        raise ValueError("seed verification failed")
    json_row(sample["seed_verification"]["stdout"])

    expected_sql = sqls(workload, mode)
    if not isinstance(sample["phases"], list) or [
        row.get("phase") for row in sample["phases"]
    ] != list(PHASES):
        raise ValueError("missing, duplicate, or reordered timed phases")
    for row in sample["phases"]:
        validate_timed(
            row,
            command(engine, database, mode, expected_sql[row["phase"]]),
            row["phase"],
        )
    json_row(sample["phases"][1]["stdout"])

    validate_observation(
        sample["absence"],
        command(engine, database, mode, absent_sql(), readonly=True),
        "absence",
    )
    absence_error(sample["absence"])
    validate_observation(
        sample["seed_after"],
        command(engine, database, mode, seed_sql(), readonly=True),
        "seed_after",
    )
    if sample["seed_after"]["returncode"] != 0:
        raise ValueError("final seed verification failed")
    json_row(sample["seed_after"]["stdout"])

    if mode == "wal":
        wal = sample["wal_after_publish"]
        expected_path = str(
            Path(database).with_name(Path(database).name + ".wal").resolve()
        )
        if (
            not isinstance(wal, dict)
            or set(wal) != {"path", "sha256", "bytes"}
            or wal["path"] != expected_path
            or not isinstance(wal["sha256"], str)
            or len(wal["sha256"]) != 64
            or type(wal["bytes"]) is not int
            or wal["bytes"] <= 0
        ):
            raise ValueError("published WAL evidence is missing or tampered")
    elif sample["wal_after_publish"] is not None:
        raise ValueError("checkpoint sample unexpectedly records a WAL publication")

    recomputed = {
        metric: (
            max(row[metric] for row in sample["phases"])
            if metric == "max_rss_bytes"
            else sum(row[metric] for row in sample["phases"])
        )
        for metric in METRICS
    }
    if sample["aggregate"] != recomputed:
        raise ValueError("sample aggregate is tampered")
    validate_artifacts(sample["artifacts"], database)


def ratio(value, baseline):
    if value == baseline == 0:
        return 1.0
    if baseline == 0:
        return float("inf")
    return value / baseline


def gate(populations):
    if set(populations) != set(TARGETS_ORDER):
        raise ValueError("both references and Rust populations are required")
    if any(len(rows) != EXPECTED["samples"] for rows in populations.values()):
        raise ValueError("requires exact 21-sample populations")
    medians = {
        target: {
            metric: statistics.median(row["aggregate"][metric] for row in rows)
            for metric in METRICS
        }
        for target, rows in populations.items()
    }
    fastest = {
        metric: min(medians["release"][metric], medians["development"][metric])
        for metric in METRICS
    }
    ratios = {
        metric: ratio(medians["rust"][metric], fastest[metric]) for metric in METRICS
    }
    cpp_throughput = max(
        1 / medians[target]["wall_ns"] for target in ("release", "development")
    )
    rust_throughput = 1 / medians["rust"]["wall_ns"]
    passed = (
        all(value <= 1 for value in ratios.values())
        and rust_throughput >= cpp_throughput
    )
    return {
        "medians": medians,
        "cpp_fastest": fastest,
        "rust_over_fastest": ratios,
        "cpp_throughput": cpp_throughput,
        "rust_throughput": rust_throughput,
        "passed": passed,
    }


def schedule(output, mode, workload):
    output = Path(output)
    rows = []
    for round_number in range(EXPECTED["warmups"] + EXPECTED["samples"]):
        targets = list(TARGETS_ORDER)
        offset = round_number % len(targets)
        for target in targets[offset:] + targets[:offset]:
            database = output / f"{mode}-{workload}-{target}-{round_number}.duckdb"
            rows.append((round_number, target, database))
    return rows


def evaluate(report, context_value):
    expected = {
        (mode, workload)
        for mode in EXPECTED["configurations"]
        for workload in EXPECTED["workloads"]
    }
    results = report.get("results")
    if (
        not isinstance(results, list)
        or len(results) != len(expected)
        or {(item.get("mode"), item.get("workload")) for item in results} != expected
    ):
        raise ValueError("invalid result population")
    gates = []
    for item in results:
        expected_schedule = schedule(
            context_value["output"], item["mode"], item["workload"]
        )
        serialized = [
            {"round": round_number, "target": target, "database": str(database)}
            for round_number, target, database in expected_schedule
        ]
        if item.get("schedule") != serialized:
            raise ValueError("schedule changed")
        for bucket, count in (
            ("warmups", EXPECTED["warmups"]),
            ("observations", EXPECTED["samples"]),
        ):
            if set(item.get(bucket, {})) != set(TARGETS_ORDER) or any(
                len(item[bucket][target]) != count for target in TARGETS_ORDER
            ):
                raise ValueError("missing sample population")
        for round_number, target, database in expected_schedule:
            bucket = "warmups" if round_number < EXPECTED["warmups"] else "observations"
            index = (
                round_number
                if bucket == "warmups"
                else round_number - EXPECTED["warmups"]
            )
            validate_sample(
                item[bucket][target][index],
                context_value["engines"][target],
                item["mode"],
                item["workload"],
                database,
                context_value["inputs"]["seed"],
            )
        gates.append(
            {
                "mode": item["mode"],
                "workload": item["workload"],
                "gate": gate(item["observations"]),
            }
        )
    return {"results": gates, "passed": all(item["gate"]["passed"] for item in gates)}


def replay(report, context_value):
    if report.get("status") != "complete":
        raise ValueError("report is not a complete campaign")
    if report.get("manifest") != context_value["manifest"]:
        raise ValueError("workload manifest identity changed")
    if report.get("inputs_before") != context_value["inputs"]:
        raise ValueError("campaign input identity changed")
    if report.get("inputs_after") != context_value["inputs"]:
        raise ValueError("campaign inputs changed while timing")
    result = evaluate(report, context_value)
    if report.get("gate") != result or report.get("passed") is not result["passed"]:
        raise ValueError("recorded performance result is tampered")
    return result


def cpp_identity(label, source, build, binary):
    source = Path(source).resolve(strict=True)
    build = Path(build).resolve(strict=True)
    cache = build / "CMakeCache.txt"
    if "CMAKE_BUILD_TYPE:STRING=Release" not in cache.read_text(errors="replace"):
        raise ValueError("C++ build is not Release")
    revision = require_checkout(source, label)
    _, cli = require_reference(binary, target=label)
    return {
        "source": str(source),
        "revision": revision,
        "build": file_id(cache),
        "cli": cli,
    }


def seed_identity(seed):
    seed = Path(seed).resolve(strict=True)
    wal = seed.with_name(seed.name + ".wal")
    if wal.exists():
        raise ValueError("seed must not have a WAL sidecar")
    value = header(seed)
    if value["effective"] != 64:
        raise ValueError("seed must use common storage version 64")
    return {**file_id(seed), "header": value, "wal_absent": True}


def campaign_context(arguments):
    spec = workload_manifest(arguments.manifest)
    references = {
        "release": cpp_identity(
            "release",
            arguments.release_source,
            arguments.release_build,
            arguments.release,
        ),
        "development": cpp_identity(
            "development",
            arguments.development_source,
            arguments.development_build,
            arguments.development,
        ),
    }
    rust = rust_identity(arguments.rust, arguments.rust_provenance)
    engines = {
        "release": {"kind": "cpp", "binary": references["release"]["cli"]["path"]},
        "development": {
            "kind": "cpp",
            "binary": references["development"]["cli"]["path"],
        },
        "rust": {"kind": "rust", "binary": rust["binary"]["path"]},
    }
    inputs = {
        "manifest": spec,
        "seed": seed_identity(arguments.seed),
        "execution_configuration": serial_configuration(),
        "helpers": {name: file_id(ROOT / "scripts" / name) for name in HELPERS},
        "references": references,
        "rust": rust,
    }
    return {
        "manifest": spec,
        "inputs": inputs,
        "engines": engines,
        "output": str(Path(arguments.output_dir).resolve()),
    }


def active_peers(check_output=subprocess.check_output):
    output = check_output(["ps", "-axo", "pid=,ppid=,command="], text=True)
    pattern = re.compile(r"\b(cargo|rustc|cmake|ninja|sqllogictest)\b", re.I)
    return [
        line.strip()
        for line in output.splitlines()
        if pattern.search(line) and not line.lstrip().startswith(str(os.getpid()) + " ")
    ]


def save(path, report):
    Path(path).write_text(json.dumps(report, indent=2) + "\n")


def serialized_arguments(arguments):
    names = (
        "manifest",
        "output_dir",
        "seed",
        "rust",
        "rust_provenance",
        "release",
        "development",
        "release_source",
        "release_build",
        "development_source",
        "development_build",
    )
    return {name: str(Path(getattr(arguments, name)).resolve()) for name in names}


def arguments_from_report(report):
    values = report.get("requested_arguments")
    names = {
        "manifest",
        "output_dir",
        "seed",
        "rust",
        "rust_provenance",
        "release",
        "development",
        "release_source",
        "release_build",
        "development_source",
        "development_build",
    }
    if not isinstance(values, dict) or set(values) != names:
        raise ValueError("report lacks exact requested arguments")
    return argparse.Namespace(**{key: Path(value) for key, value in values.items()})


def run_campaign(arguments):
    output = Path(arguments.output_dir)
    if output.exists():
        raise FileExistsError("preserve prior evidence: output exists")
    output.mkdir(parents=True)
    report = {
        "schema": 1,
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "status": "running",
        "passed": False,
        "requested_arguments": serialized_arguments(arguments),
        "manifest": None,
        "inputs_before": None,
        "inputs_after": None,
        "quiet_host": None,
        "results": [],
    }
    report_path = output / "report.json"
    save(report_path, report)
    try:
        context_value = campaign_context(arguments)
        report["manifest"] = context_value["manifest"]
        report["inputs_before"] = context_value["inputs"]
        if not arguments.run:
            report["inputs_after"] = campaign_context(arguments)["inputs"]
            if report["inputs_after"] != report["inputs_before"]:
                raise ValueError("campaign input identity changed during preparation")
            report["status"] = "prepared"
            return report
        if not arguments.quiet_host_confirmed:
            raise RuntimeError("timing requires explicit quiet-host confirmation")
        peers = active_peers()
        report["quiet_host"] = {"confirmed": True, "active_peers": peers}
        if peers:
            raise RuntimeError(
                "quiet-host measurement blocked by active build/test peers"
            )

        for mode in EXPECTED["configurations"]:
            for workload in EXPECTED["workloads"]:
                item = {
                    "mode": mode,
                    "workload": workload,
                    "schedule": [
                        {
                            "round": round_number,
                            "target": target,
                            "database": str(database),
                        }
                        for round_number, target, database in schedule(
                            context_value["output"], mode, workload
                        )
                    ],
                    "warmups": {target: [] for target in TARGETS_ORDER},
                    "observations": {target: [] for target in TARGETS_ORDER},
                }
                report["results"].append(item)
                save(report_path, report)
                for round_number, target, database in schedule(
                    context_value["output"], mode, workload
                ):
                    try:
                        sample = one_sample(
                            context_value["engines"][target],
                            mode,
                            workload,
                            arguments.seed,
                            database,
                        )
                    except SampleFailure as error:
                        item["failed_sample"] = {
                            "round": round_number,
                            "target": target,
                            "observation": error.observation,
                        }
                        raise
                    bucket = (
                        "warmups"
                        if round_number < EXPECTED["warmups"]
                        else "observations"
                    )
                    item[bucket][target].append(sample)
                    save(report_path, report)

        report["inputs_after"] = campaign_context(arguments)["inputs"]
        if report["inputs_after"] != report["inputs_before"]:
            raise ValueError("campaign input identity changed while timing")
        report["status"] = "complete"
        report["gate"] = evaluate(report, context_value)
        report["passed"] = report["gate"]["passed"]
    except Exception as error:
        report["status"] = "failed"
        report["passed"] = False
        report["error"] = str(error)
    finally:
        save(report_path, report)
    return report


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--manifest", type=Path)
    result.add_argument("--output-dir", type=Path)
    result.add_argument("--seed", type=Path)
    result.add_argument("--rust", type=Path)
    result.add_argument("--rust-provenance", type=Path)
    result.add_argument("--release", type=Path)
    result.add_argument("--development", type=Path)
    result.add_argument("--release-source", type=Path)
    result.add_argument("--release-build", type=Path)
    result.add_argument("--development-source", type=Path)
    result.add_argument("--development-build", type=Path)
    result.add_argument("--run", action="store_true")
    result.add_argument("--quiet-host-confirmed", action="store_true")
    result.add_argument("--validate", type=Path)
    result.add_argument("--prepare-rust-provenance", type=Path)
    return result


def selected_action(arguments):
    campaign_flags = (
        "manifest",
        "output_dir",
        "seed",
        "rust",
        "rust_provenance",
        "release",
        "development",
        "release_source",
        "release_build",
        "development_source",
        "development_build",
    )
    if arguments.validate:
        if (
            arguments.prepare_rust_provenance
            or arguments.run
            or arguments.quiet_host_confirmed
        ):
            raise ValueError("--validate cannot be combined with another mode")
        if any(getattr(arguments, name) is not None for name in campaign_flags):
            raise ValueError("--validate does not accept campaign flags")
        return "validate"
    if arguments.prepare_rust_provenance:
        if arguments.run or arguments.quiet_host_confirmed:
            raise ValueError("Rust preparation cannot be timed")
        if any(getattr(arguments, name) is not None for name in campaign_flags):
            raise ValueError("Rust preparation does not accept campaign flags")
        return "prepare"
    required = ("output_dir", "seed", "rust", "rust_provenance")
    if any(getattr(arguments, name) is None for name in required):
        raise ValueError(
            "campaign requires --output-dir, --seed, --rust, and --rust-provenance"
        )
    if arguments.quiet_host_confirmed and not arguments.run:
        raise ValueError("--quiet-host-confirmed requires --run")
    arguments.manifest = (
        arguments.manifest or ROOT / "benchmark/b1_view_durable_workloads.json"
    )
    arguments.release = arguments.release or TARGETS["release"].binary
    arguments.development = (
        arguments.development or ROOT.parent / "duckdb/build/engine-walkthrough/duckdb"
    )
    arguments.release_source = arguments.release_source or TARGETS["release"].source
    arguments.release_build = arguments.release_build or TARGETS["release"].build
    arguments.development_source = (
        arguments.development_source or ROOT / "target/reference-source-development"
    )
    arguments.development_build = (
        arguments.development_build or ROOT.parent / "duckdb/build/engine-walkthrough"
    )
    return "campaign"


def main(argv=None):
    arguments = parser().parse_args(argv)
    try:
        action = selected_action(arguments)
        if action == "prepare":
            provenance = prepare_rust_provenance(arguments.prepare_rust_provenance)
            print(
                json.dumps(
                    {
                        "provenance": str(arguments.prepare_rust_provenance),
                        "binary": provenance["binary_after"],
                    }
                )
            )
            return 0
        if action == "validate":
            report = json.loads(arguments.validate.read_text())
            context_value = campaign_context(arguments_from_report(report))
            result = replay(report, context_value)
            print(json.dumps(result))
            return 0 if result["passed"] else 1
        report = run_campaign(arguments)
        print(
            json.dumps(
                {
                    "status": report["status"],
                    "passed": report["passed"],
                    "report": str(Path(arguments.output_dir) / "report.json"),
                    "error": report.get("error"),
                }
            )
        )
        return 0 if report["status"] == "prepared" or report["passed"] else 1
    except Exception as error:
        print(json.dumps({"status": "failed", "passed": False, "error": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
