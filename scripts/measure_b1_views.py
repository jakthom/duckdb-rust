"""Fail-closed B1 durable native-view process measurement (run only with --run)."""
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

from reference_version import ROOT, TARGETS, require_checkout, require_reference

METRICS = ("wall_ns", "cpu_ns", "max_rss_bytes", "block_input", "block_output")
PHASES = ("publish", "reopen_query", "drop", "readonly_verify")
EXPECTED = {"schema": 1, "id": "b1_view_durable_native", "rows": 10000,
            "checksum": 49995000, "samples": 21, "warmups": 3,
            "metrics": [*METRICS, "throughput"], "seed_table": "b1_seed", "column": "i",
            "configurations": ["checkpoint", "wal"],
            "workloads": ["view_cycle", "direct_table_publication"]}


class SampleFailure(RuntimeError):
    def __init__(self, message, observation):
        super().__init__(message)
        self.observation = observation


def digest(path):
    with Path(path).open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def manifest(path):
    path = Path(path).resolve(strict=True)
    data = json.loads(path.read_text())
    if data != EXPECTED:
        raise ValueError("B1 durable workload manifest is changed or malformed")
    return {"path": str(path), "sha256": digest(path), "data": data}


def parse_time(stderr):
    usage = re.search(r"(?m)^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys\s*$", stderr)
    if not usage:
        raise ValueError("/usr/bin/time -l omitted CPU timing")
    result = {"cpu_ns": int((float(usage.group(2)) + float(usage.group(3))) * 1_000_000_000)}
    for key, label in (("max_rss_bytes", "maximum resident set size"),
                       ("block_input", "block input operations"),
                       ("block_output", "block output operations")):
        found = re.search(rf"(?m)^\s*(\d+)\s+{re.escape(label)}\s*$", stderr)
        if not found:
            raise ValueError("/usr/bin/time -l omitted " + key)
        result[key] = int(found.group(1))
    return result


def timed(command, phase, execute=subprocess.run):
    if platform.system() != "Darwin":
        raise RuntimeError("durable acceptance requires macOS /usr/bin/time -l")
    command = [str(part) for part in command]
    started = time.perf_counter_ns()
    result = execute(["/usr/bin/time", "-l", *command], text=True, capture_output=True)
    observation = {"command": command, "phase": phase, "returncode": result.returncode,
                   "stdout": result.stdout, "stderr": result.stderr,
                   "wall_ns": time.perf_counter_ns() - started}
    if result.returncode:
        raise SampleFailure(phase + " CLI failed", observation)
    try:
        observation.update(parse_time(result.stderr))
    except ValueError as error:
        raise SampleFailure(str(error), observation) from error
    if observation["wall_ns"] <= 0 or any(observation[key] < 0 for key in METRICS):
        raise SampleFailure("incomplete timing observation", observation)
    return observation


def cpp_sql(mode, sql):
    return ("PRAGMA disable_checkpoint_on_shutdown; " if mode == "wal" else "") + sql


def command(engine, database, mode, sql, readonly=False):
    if engine["kind"] == "cpp":
        return [engine["binary"], database, "-json", *( ["-readonly"] if readonly else []), "-c", cpp_sql(mode, sql)]
    if readonly:
        return [engine["binary"], database, "--read-only", "--json", "-c", sql]
    return [engine["binary"], database, "--durability", mode, "--json", "-c", sql]


def sqls(workload, mode):
    if workload == "view_cycle":
        create, query, drop, absent = ("CREATE OR REPLACE VIEW b1_public AS SELECT i FROM b1_seed",
            "SELECT count(*) AS row_count, coalesce(sum(i), 0) AS checksum FROM b1_public",
            "DROP VIEW b1_public", "SELECT count(*) AS absent FROM information_schema.views WHERE table_name = 'b1_public'")
    elif workload == "direct_table_publication":
        create, query, drop, absent = ("CREATE OR REPLACE TABLE b1_public AS SELECT i FROM b1_seed",
            "SELECT count(*) AS row_count, coalesce(sum(i), 0) AS checksum FROM b1_public",
            "DROP TABLE b1_public", "SELECT count(*) AS absent FROM information_schema.tables WHERE table_name = 'b1_public'")
    else:
        raise ValueError("unknown durable workload")
    return {"publish": "BEGIN TRANSACTION; " + create + "; COMMIT;" + (" CHECKPOINT;" if mode == "checkpoint" else ""), "reopen_query": query,
            "drop": "BEGIN TRANSACTION; " + drop + "; COMMIT;", "readonly_verify": absent}


def json_row(stdout, expected):
    try:
        value = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise ValueError("CLI did not emit JSON") from error
    if not isinstance(value, list) or len(value) != 1 or not isinstance(value[0], dict):
        raise ValueError("CLI JSON must contain exactly one row")
    row = value[0]
    if set(row) != set(expected):
        raise ValueError("CLI JSON metadata changed")
    for key, wanted in expected.items():
        if isinstance(row[key], bool) or row[key] != wanted:
            raise ValueError("CLI JSON value changed: " + key)


def untimed(command, expected):
    result = subprocess.run([str(part) for part in command], text=True, capture_output=True)
    if result.returncode:
        raise SampleFailure("untimed verification CLI failed", {"command": command, "returncode": result.returncode,
                                                                  "stdout": result.stdout, "stderr": result.stderr})
    json_row(result.stdout, expected)


def seed_command(engine, database):
    return command(engine, database, "checkpoint", "SELECT count(*) AS row_count, coalesce(sum(i), 0) AS checksum FROM b1_seed")


def artifact_sizes(database):
    return {suffix: (database.with_name(database.name + suffix).stat().st_size if database.with_name(database.name + suffix).exists() else 0)
            for suffix in ("", ".wal")}


def one_sample(engine, mode, workload, seed, database):
    shutil.copyfile(seed, database)
    before = {"seed_sha256": digest(database), "sizes": artifact_sizes(database)}
    untimed(seed_command(engine, database), {"row_count": 10000, "checksum": 49995000})
    observations = []
    for phase, sql in sqls(workload, mode).items():
        observation = timed(command(engine, database, mode, sql, phase == "readonly_verify"), phase)
        observations.append(observation)
        if phase == "publish" and mode == "wal" and not database.with_name(database.name + ".wal").is_file():
            raise SampleFailure("WAL configuration did not retain a WAL before reopen", observation)
        if phase == "reopen_query":
            json_row(observation["stdout"], {"row_count": 10000, "checksum": 49995000})
        if phase == "readonly_verify":
            json_row(observation["stdout"], {"absent": 0})
    aggregate = {metric: (max(row[metric] for row in observations) if metric == "max_rss_bytes" else sum(row[metric] for row in observations)) for metric in METRICS}
    after_sizes = artifact_sizes(database)
    return {"phases": observations, "aggregate": aggregate, "artifact_before": before,
            "artifact_after": {"sizes": after_sizes,
                               "delta_bytes": {key: after_sizes[key] - before["sizes"][key] for key in after_sizes}}}


def validate_sample(sample):
    if not isinstance(sample, dict) or set(sample) != {"phases", "aggregate", "artifact_before", "artifact_after"}:
        raise ValueError("sample is incomplete")
    phases = sample["phases"]
    if not isinstance(phases, list) or [row.get("phase") for row in phases] != list(PHASES):
        raise ValueError("sample has missing, duplicate, or reordered phases")
    for row in phases:
        if row.get("returncode") != 0 or not isinstance(row.get("command"), list) or not row["command"]:
            raise ValueError("sample lacks successful CLI evidence")
        if not isinstance(row.get("stdout"), str) or not isinstance(row.get("stderr"), str):
            raise ValueError("sample lacks CLI output")
        if any(not isinstance(row.get(metric), int) or row[metric] < 0 for metric in METRICS) or row["wall_ns"] <= 0:
            raise ValueError("sample has incomplete metrics")
    aggregate = sample["aggregate"]
    recomputed = {metric: (max(row[metric] for row in phases) if metric == "max_rss_bytes" else sum(row[metric] for row in phases)) for metric in METRICS}
    if aggregate != recomputed:
        raise ValueError("sample aggregate is tampered or incomplete")
    before, after = sample["artifact_before"], sample["artifact_after"]
    if set(before) != {"seed_sha256", "sizes"} or set(after) != {"sizes", "delta_bytes"} or set(before["sizes"]) != {"", ".wal"} or set(after["sizes"]) != {"", ".wal"}:
        raise ValueError("sample lacks artifact-size evidence")
    if after["delta_bytes"] != {key: after["sizes"][key] - before["sizes"][key] for key in before["sizes"]}:
        raise ValueError("artifact byte delta is tampered")


def validate_commands(sample, mode, workload, target):
    """Derive every accepted CLI invocation; serialized commands are not trusted."""
    binary = sample["phases"][0]["command"][0]
    engine = {"kind": "rust" if target == "rust" else "cpp", "binary": binary}
    expected_sql = sqls(workload, mode)
    for row in sample["phases"]:
        actual = row["command"]
        if len(actual) < 2:
            raise ValueError("CLI command lacks a database path")
        wanted = command(engine, actual[1], mode, expected_sql[row["phase"]], row["phase"] == "readonly_verify")
        if actual != wanted:
            raise ValueError("CLI command replay differs from durable contract")
    json_row(sample["phases"][1]["stdout"], {"row_count": 10000, "checksum": 49995000})
    json_row(sample["phases"][3]["stdout"], {"absent": 0})


def gate(populations, samples=21):
    if set(populations) != {"release", "development", "rust"}:
        raise ValueError("both references and Rust populations are required")
    if any(len(rows) != samples for rows in populations.values()):
        raise ValueError("requires exactly 21 retained samples per population")
    for rows in populations.values():
        for row in rows:
            validate_sample(row)
    medians = {name: {metric: statistics.median(row["aggregate"][metric] for row in rows) for metric in METRICS}
               for name, rows in populations.items()}
    fastest = {metric: min(medians["release"][metric], medians["development"][metric]) for metric in METRICS}
    def ratio(value, baseline): return 1.0 if value == baseline == 0 else float("inf") if baseline == 0 else value / baseline
    ratios = {metric: ratio(medians["rust"][metric], fastest[metric]) for metric in METRICS}
    cpp_throughput = max(1 / medians[name]["wall_ns"] for name in ("release", "development"))
    rust_throughput = 1 / medians["rust"]["wall_ns"]
    passed = all(value <= 1 for value in ratios.values()) and rust_throughput >= cpp_throughput
    return {"medians": medians, "cpp_fastest": fastest, "rust_over_fastest": ratios,
            "cpp_throughput": cpp_throughput, "rust_throughput": rust_throughput,
            "passed": passed, "at_parity_or_better_performance": passed}


def replay(report):
    spec = report.get("manifest", {}).get("data")
    if spec != EXPECTED or report.get("samples") != 21 or report.get("warmups") != 3:
        raise ValueError("report manifest or population configuration changed")
    results = report.get("results")
    expected = {(mode, workload) for mode in EXPECTED["configurations"] for workload in EXPECTED["workloads"]}
    if not isinstance(results, list) or {(item.get("mode"), item.get("workload")) for item in results} != expected or len(results) != len(expected):
        raise ValueError("report has missing, extra, or duplicate configurations")
    gates = []
    for item in results:
        if set(item.get("observations", {})) != {"release", "development", "rust"} or set(item.get("warmups", {})) != {"release", "development", "rust"}:
            raise ValueError("configuration lacks a population")
        for target in ("release", "development", "rust"):
            warmups = item["warmups"][target]
            if not isinstance(warmups, list) or len(warmups) != 3:
                raise ValueError("configuration has missing warmups")
            for sample in [*warmups, *item["observations"][target]]:
                validate_sample(sample)
                validate_commands(sample, item["mode"], item["workload"], target)
        gates.append({"mode": item["mode"], "workload": item["workload"], "gate": gate(item["observations"])})
    return {"results": gates, "passed": all(item["gate"]["passed"] for item in gates)}


def active_peers():
    output = subprocess.check_output(["ps", "-axo", "pid=,ppid=,command="], text=True)
    return [line.strip() for line in output.splitlines() if re.search(r"\b(cargo|rustc|cmake|ninja|unittest|sqllogictest)\b", line, re.I) and str(os.getpid()) not in line]


def cpp_identity(label, source, build, binary):
    source, build = Path(source).resolve(strict=True), Path(build).resolve(strict=True)
    if "CMAKE_BUILD_TYPE:STRING=Release" not in (build / "CMakeCache.txt").read_text(errors="replace"):
        raise ValueError("C++ build must be an explicit Release build")
    revision = require_checkout(source, label)
    _, cli = require_reference(binary, target=label)
    return {"source": str(source), "revision": revision, "build": str(build), "build_sha256": digest(build / "CMakeCache.txt"), "cli": cli}


def rust_source_digest():
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "tools/shell/main.rs", *(ROOT / "src").rglob("*.rs")]
    result = hashlib.sha256()
    for path in sorted(paths):
        result.update(str(path.relative_to(ROOT)).encode() + b"\0")
        result.update(path.read_bytes())
    return result.hexdigest()


def run_campaign(args):
    if args.output_dir.exists():
        raise FileExistsError("preserve prior evidence: output directory already exists")
    spec = manifest(args.manifest)
    args.output_dir.mkdir(parents=True)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "manifest": spec, "samples": 21, "warmups": 3,
              "platform": platform.platform(), "machine": platform.machine(), "passed": False,
              "at_parity_or_better_performance": False}
    try:
        report["references"] = {"release": cpp_identity("release", args.release_source, args.release_build, args.release),
                                "development": cpp_identity("development", args.development_source, args.development_build, args.development)}
        rust = Path(args.rust).resolve(strict=True)
        report["rust"] = {"binary": str(rust), "binary_sha256": digest(rust), "git_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "source_sha256": rust_source_digest()}
        report["helpers"] = {name: digest(ROOT / "scripts" / name) for name in ("measure_b1_views.py", "reference_version.py")}
        seed = Path(args.seed).resolve(strict=True)
        report["seed"] = {"path": str(seed), "sha256": digest(seed), "sizes": artifact_sizes(seed)}
        if not args.run:
            report["prepared"] = True
            return report
        if active_peers():
            raise RuntimeError("quiet-host measurement blocked by active build/test peers")
        engines = {"release": {"kind": "cpp", "binary": str(args.release)}, "development": {"kind": "cpp", "binary": str(args.development)}, "rust": {"kind": "rust", "binary": str(rust)}}
        report["results"] = []
        for mode in EXPECTED["configurations"]:
            for workload in EXPECTED["workloads"]:
                observations, warmups = ({name: [] for name in engines}, {name: [] for name in engines})
                for round_number in range(24):
                    names = list(engines); names = names[round_number % 3:] + names[:round_number % 3]
                    for name in names:
                        db = args.output_dir / f"{mode}-{workload}-{name}-{round_number}.duckdb"
                        sample = one_sample(engines[name], mode, workload, seed, db)
                        (observations if round_number >= 3 else warmups)[name].append(sample)
                report["results"].append({"mode": mode, "workload": workload, "warmups": warmups, "observations": observations})
        report["gate"] = replay(report); report["passed"] = report["gate"]["passed"]
        report["at_parity_or_better_performance"] = report["passed"]
        after = {"rust_binary_sha256": digest(rust), "rust_source_sha256": rust_source_digest(),
                 "helpers": {name: digest(ROOT / "scripts" / name) for name in ("measure_b1_views.py", "reference_version.py")},
                 "manifest_sha256": digest(args.manifest)}
        before = {"rust_binary_sha256": report["rust"]["binary_sha256"], "rust_source_sha256": report["rust"]["source_sha256"],
                  "helpers": report["helpers"], "manifest_sha256": spec["sha256"]}
        if before != after: raise ValueError("source, binary, helper, or workload identity changed during campaign")
        report["identity_after"] = after
    except Exception as error:
        report["error"] = str(error)
        if isinstance(error, SampleFailure): report["failed_run"] = error.observation
    finally:
        (args.output_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=ROOT / "benchmark/b1_view_durable_workloads.json")
    parser.add_argument("--output-dir", type=Path, required=True); parser.add_argument("--seed", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True); parser.add_argument("--release", type=Path, default=TARGETS["release"].binary)
    parser.add_argument("--development", type=Path, default=ROOT.parent / "duckdb/build/engine-walkthrough/duckdb")
    parser.add_argument("--release-source", type=Path, default=TARGETS["release"].source); parser.add_argument("--release-build", type=Path, default=TARGETS["release"].build)
    parser.add_argument("--development-source", type=Path, default=ROOT / "target/reference-source-development")
    parser.add_argument("--development-build", type=Path, default=ROOT.parent / "duckdb/build/engine-walkthrough")
    parser.add_argument("--run", action="store_true", help="execute the 3 warmup + 21 sample campaign")
    args = parser.parse_args(); report = run_campaign(args)
    print(json.dumps({"prepared": report.get("prepared", False), "passed": report["passed"], "report": str(args.output_dir / "report.json"), "error": report.get("error")}))
    raise SystemExit(0 if report.get("prepared") or report["passed"] else 1)


if __name__ == "__main__": main()
