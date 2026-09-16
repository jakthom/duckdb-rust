"""Fail-closed dual-pin performance acceptance for native SQLLogic runners.

The input manifest is JSON ``{"workloads": [{"id": "...", "path": "..."}]}``.
Paths are relative to ``--test-root`` and must name unchanged SQLLogic files.
This adapter intentionally measures process-level runner costs: it is for runner,
parser, fixture, and oracle changes, rather than engine microbenchmarks.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import time

from reference_version import ROOT, TARGETS, require_checkout, require_reference
from source_identity import vendored_sources


METRICS = ("wall_ns", "cpu_ns", "max_rss_bytes", "block_input", "block_output")


class SampleFailure(RuntimeError):
    def __init__(self, message, details):
        super().__init__(message)
        self.details = details


def digest(path):
    with Path(path).open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def source_digest():
    files = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", *vendored_sources(ROOT),
             *(ROOT / "src").rglob("*.rs"), *(ROOT / "test").rglob("*.rs")]
    result = hashlib.sha256()
    for path in sorted(set(files)):
        result.update(str(path.relative_to(ROOT)).encode() + b"\0")
        result.update(path.read_bytes())
    return result.hexdigest()


def release_cache(build):
    cache = Path(build) / "CMakeCache.txt"
    if not cache.is_file() or "CMAKE_BUILD_TYPE:STRING=Release" not in cache.read_text(errors="replace"):
        raise ValueError("C++ baseline must have an explicit CMake Release configuration")
    return cache


def parse_time(stderr):
    """Parse macOS ``/usr/bin/time -l`` output; absence is not comparable."""
    import re
    labels = {
        "maximum resident set size": ("max_rss_bytes", 1),
        "block input operations": ("block_input", 1),
        "block output operations": ("block_output", 1),
    }
    values = {}
    usage = re.search(r"(?m)^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys\s*$", stderr)
    if usage:
        values["cpu_user"] = int(float(usage.group(2)) * 1_000_000_000)
        values["cpu_system"] = int(float(usage.group(3)) * 1_000_000_000)
    for line in stderr.splitlines():
        line = line.strip()
        for label, (key, scale) in labels.items():
            if line.endswith(label):
                raw = line[: -len(label)].strip().split()[0]
                values[key] = int(float(raw) * scale)
    if set(values) != {"cpu_user", "cpu_system", *(key for key, _ in labels.values())}:
        raise ValueError("/usr/bin/time -l did not provide CPU, RSS, and block-I/O metrics")
    # Darwin reports maximum resident set size in bytes.
    values["cpu_ns"] = values.pop("cpu_user") + values.pop("cpu_system")
    return values


def records_from_output(stdout, label):
    """Only accept a successful, nonempty native-runner verdict."""
    import re
    if label == "feedback":
        # The upstream feedback driver writes its assertion evidence to the
        # report named in the command.  Keep stdout machine-readable too, but
        # defer the exact selected-result validation to that retained report.
        if not re.search(r'(?m)^\{"outcomes":', stdout):
            raise ValueError("feedback runner did not emit its report marker")
        return 1
    if label == "rust":
        match = re.search(r"(?m)^PASS .+ \(([1-9][0-9]*) records\)$", stdout)
        summary = re.search(r"(?m)^([1-9][0-9]*) records passed; 0 skipped$", stdout)
        if not match or not summary or match.group(1) != summary.group(1):
            raise ValueError("Rust runner did not emit the expected nonzero PASS marker")
        return int(match.group(1))
    # Catch's success output is stable across the two pinned runners.  The exact
    # path has already been supplied as the only test specification.
    match = re.search(r"All tests passed \(([1-9][0-9]*) assertion", stdout)
    if not match:
        raise ValueError("C++ runner did not emit the expected nonzero PASS marker")
    return int(match.group(1))


def run_timed(command, label, execute=subprocess.run):
    if platform.system() != "Darwin":
        raise RuntimeError("acceptance measurements require macOS /usr/bin/time -l")
    started = time.perf_counter_ns()
    result = execute(["/usr/bin/time", "-l", *map(str, command)], text=True,
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    wall_ns = time.perf_counter_ns() - started
    if result.returncode:
        raise SampleFailure(f"{label} exited {result.returncode}", {"command": [str(part) for part in command],
                            "returncode": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
    details = {"command": [str(part) for part in command], "returncode": result.returncode,
               "stdout": result.stdout, "stderr": result.stderr, "wall_ns": wall_ns}
    try:
        values = parse_time(result.stderr)
        values.update(details)
        values["records"] = records_from_output(result.stdout, label)
    except ValueError as error:
        raise SampleFailure(str(error), details) from error
    if values["wall_ns"] <= 0 or any(not isinstance(values[key], int) or values[key] < 0 for key in METRICS):
        raise ValueError("incomplete or nonpositive timing sample")
    return values


def validate_manifest(path, test_root):
    data = json.loads(Path(path).read_text())
    workloads = data.get("workloads")
    if not isinstance(workloads, list) or not workloads:
        raise ValueError("workload manifest must contain at least one workload")
    seen, prepared = set(), []
    root = Path(test_root).resolve(strict=True)
    for item in workloads:
        if set(item) != {"id", "path"} or not isinstance(item["id"], str) or not item["id"]:
            raise ValueError("each workload must have only nonempty id and path")
        if item["id"] in seen:
            raise ValueError("duplicate workload id: " + item["id"])
        seen.add(item["id"])
        candidate = (root / item["path"]).resolve()
        if not candidate.is_relative_to(root) or not candidate.is_file() or not candidate.name.endswith((".test", ".test_slow", ".test_coverage")):
            raise ValueError("invalid SQLLogic workload path: " + item["path"])
        prepared.append({**item, "sha256": digest(candidate), "bytes": candidate.stat().st_size})
    return prepared


def active_peers(check_output=subprocess.check_output):
    output = check_output(["ps", "-axo", "pid=,ppid=,command="], text=True)
    needles = ("cargo ", "rustc", "cmake", "ninja", "unittest", "sqllogictest")
    processes = {}
    for line in output.splitlines():
        parts = line.strip().split(maxsplit=2)
        if len(parts) == 3:
            processes[int(parts[0])] = (int(parts[1]), parts[2])
    ancestors = {os.getpid()}
    parent = os.getppid()
    while parent in processes and parent not in ancestors:
        ancestors.add(parent)
        parent = processes[parent][0]
    return [command for pid, (_, command) in processes.items()
            if pid not in ancestors and any(needle in command.lower() for needle in needles)]


def median(entries, metric):
    samples = [sample[metric] for sample in entries]
    if len(samples) < 9 or any(not isinstance(value, int) or value < 0 for value in samples):
        raise ValueError("incomplete samples for " + metric)
    return statistics.median(samples)


def gate(release, development, workloads):
    """Apply every metric independently to every workload and retained Rust run."""
    reports = {"release": release, "development": development}
    expected = {item["id"]: item for item in workloads}
    if len(expected) != len(workloads):
        raise ValueError("duplicate workload id in acceptance manifest")
    outcome = []
    for target, report in reports.items():
        entries = report["workloads"]
        if {entry["id"] for entry in entries} != set(expected) or len(entries) != len(expected):
            raise ValueError("missing, extra, or duplicate workload result in " + target)
    for identifier, spec in expected.items():
        entries = {target: next(entry for entry in report["workloads"] if entry["id"] == identifier)
                   for target, report in reports.items()}
        if any({key: entry[key] for key in spec} != spec for entry in entries.values()):
            raise ValueError("workload identity changed: " + identifier)
        cpp = {target: {metric: median(entries[target]["cpp"], metric) for metric in METRICS}
               for target in reports}
        rust = {target: {metric: median(entries[target]["rust"], metric) for metric in METRICS}
                for target in reports}
        baseline = {metric: min(cpp["release"][metric], cpp["development"][metric]) for metric in METRICS}
        # Records are fixed per workload; it is still retained explicitly so a
        # changed runner cannot hide a zero/partial run behind a low wall clock.
        # C++ reports Catch assertions while Rust reports SQLLogic records; those
        # counts are distinct units.  Each timed invocation is one validated,
        # equivalent workload, so invocation throughput is the comparable unit.
        throughput = {target: 1 / rust[target]["wall_ns"] for target in reports}
        cpp_throughput = {target: 1 / cpp[target]["wall_ns"] for target in reports}
        best_throughput = max(cpp_throughput.values())
        def ratio(value, reference):
            return 1.0 if value == reference == 0 else float("inf") if reference == 0 else value / reference
        ratios = {target: {metric: ratio(rust[target][metric], baseline[metric]) for metric in METRICS} for target in reports}
        passed = all(ratio <= 1 for values in ratios.values() for ratio in values.values()) and all(value >= best_throughput for value in throughput.values())
        outcome.append({"id": identifier, "cpp_fastest_medians": baseline, "rust_medians": rust,
                        "rust_over_fastest": ratios, "cpp_throughput": cpp_throughput,
                        "rust_throughput": throughput, "passed": passed})
    return {"workloads": outcome, "passed": all(item["passed"] for item in outcome),
            "at_parity_or_better_performance": all(item["passed"] for item in outcome)}


def validate_observation(sample):
    """Reject a summary or partial sample before it can enter Gate P.

    The report is evidence, rather than merely input to a one-shot gate.  Keep
    enough of each invocation to audit its runner verdict and to recompute the
    gate after JSON serialization.
    """
    required = {"command", "returncode", "stdout", "stderr", "records", *METRICS}
    if not isinstance(sample, dict) or not required <= set(sample):
        raise ValueError("raw observation is missing command, verdict, or metric evidence")
    if (not isinstance(sample["command"], list) or not sample["command"]
            or any(not isinstance(part, str) or not part for part in sample["command"])):
        raise ValueError("raw observation has an invalid command")
    if sample["returncode"] != 0 or not isinstance(sample["stdout"], str) or not isinstance(sample["stderr"], str):
        raise ValueError("raw observation has no successful runner verdict")
    if not isinstance(sample["records"], int) or sample["records"] <= 0:
        raise ValueError("raw observation has an invalid PASS count")
    if (not isinstance(sample["wall_ns"], int) or sample["wall_ns"] <= 0
            or any(not isinstance(sample[key], int) or sample[key] < 0 for key in METRICS if key != "wall_ns")):
        raise ValueError("raw observation has incomplete timing metrics")


def gate_report(report, workloads):
    """Recompute Gate P from a serialized campaign report; fail closed."""
    sample_count = report.get("samples")
    if not isinstance(sample_count, int) or sample_count < 9 or sample_count % 2 != 1:
        raise ValueError("raw report has an invalid acceptance sample count")
    entries = report.get("workloads")
    if not isinstance(entries, list) or len(entries) != len(workloads):
        raise ValueError("raw report has missing, extra, or duplicate workloads")
    expected = {item["id"]: item for item in workloads}
    raw = {}
    for entry in entries:
        identifier = entry.get("id") if isinstance(entry, dict) else None
        if identifier not in expected or identifier in raw:
            raise ValueError("raw report has missing, extra, or duplicate workloads")
        if any(entry.get(key) != value for key, value in expected[identifier].items()):
            raise ValueError("raw workload identity changed: " + identifier)
        observations = entry.get("observations")
        if not isinstance(observations, dict) or set(observations) != {"release", "development", "rust"}:
            raise ValueError("raw workload is missing a runner population")
        for target, samples in observations.items():
            if not isinstance(samples, list) or len(samples) != sample_count:
                raise ValueError("raw workload has incomplete samples: " + identifier + "/" + target)
            for sample in samples:
                validate_observation(sample)
            if len({sample["records"] for sample in samples}) != 1:
                raise ValueError("inconsistent SQLLogic PASS count: " + identifier + "/" + target)
        raw[identifier] = observations
    release = {"workloads": [{**item, "cpp": raw[item["id"]]["release"], "rust": raw[item["id"]]["rust"]}
                            for item in workloads]}
    development = {"workloads": [{**item, "cpp": raw[item["id"]]["development"], "rust": raw[item["id"]]["rust"]}
                                for item in workloads]}
    return gate(release, development, workloads)


def identity(target, cpp, source, build, cli, test_root, workloads):
    source = Path(source).resolve(strict=True)
    build = Path(build).resolve(strict=True)
    test_root = Path(test_root).resolve(strict=True)
    revision = require_checkout(source, target)
    _, cli_identity = require_reference(cli, target=target)
    cpp = Path(cpp).resolve(strict=True)
    if not cpp.is_file():
        raise ValueError("C++ unittest is not a file")
    cache = release_cache(build)
    return {"target": target, "revision": revision, "source_directory": str(source),
            "build_directory": str(build), "test_directory": str(test_root),
            "shared_workloads": workloads, "cli": cli_identity,
            "unittest": str(cpp), "unittest_sha256": digest(cpp),
            "cmake_cache": str(cache), "cmake_cache_sha256": digest(cache)}


def run_campaign(args):
    if args.report.exists():
        raise FileExistsError("preserve prior evidence: choose a new report path")
    if args.samples < 9 or args.samples % 2 != 1 or args.warmups != 3:
        raise ValueError("use exactly three warmups and an odd sample count of at least nine")
    peers = active_peers()
    if peers:
        raise RuntimeError("quiet-host acceptance measurement blocked by active peers: " + "; ".join(peers))
    workloads = validate_manifest(args.workloads, args.test_root)
    root = Path(args.test_root).resolve(strict=True)
    references = {
        "release": identity("release", args.release_cpp, args.release_source, args.release_build,
                            args.release_cli, root, workloads),
        "development": identity("development", args.development_cpp, args.development_source,
                                args.development_build, args.development_cli, root, workloads),
    }
    rust = Path(args.rust).resolve(strict=True)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "samples": args.samples, "warmups": args.warmups,
              "order": "paired alternating release, development, rust", "test_root": str(root),
              "workloads_manifest": str(Path(args.workloads).resolve()),
              "workloads_manifest_sha256": digest(args.workloads), "workloads": [], "references": references,
              "rust_binary": str(rust), "rust_binary_sha256": digest(rust), "rust_source_sha256": source_digest(),
              "platform": platform.platform(), "machine": platform.machine(), "passed": False,
              "at_parity_or_better_performance": False}
    try:
        for workload in workloads:
            relative = workload["path"]
            commands = {
                "release": [args.release_cpp, "--test-dir", root, relative, "--use-colour", "no", "--durations", "no"],
                "development": [args.development_cpp, "--test-dir", root, relative, "--use-colour", "no", "--durations", "no"],
                "rust": [rust, root, relative],
            }
            samples = {name: [] for name in commands}
            for round_number in range(args.warmups + args.samples):
                names = ["release", "development", "rust"]
                names = names[round_number % len(names):] + names[:round_number % len(names)]
                for name in names:
                    sample = run_timed(commands[name], "rust" if name == "rust" else "cpp")
                    if round_number >= args.warmups:
                        samples[name].append(sample)
            cpp_records = {name: {sample["records"] for sample in samples[name]} for name in ("release", "development")}
            rust_records = {sample["records"] for sample in samples["rust"]}
            if any(len(records) != 1 for records in cpp_records.values()) or len(rust_records) != 1:
                raise ValueError("inconsistent SQLLogic PASS count: " + workload["id"])
            report["workloads"].append({**workload, "observations": samples,
                                        "pass_counts": {name: next(iter(records)) for name, records in cpp_records.items()}
                                        | {"rust": next(iter(rust_records))}})
        # Do not merge this summary into the raw workload observations: evidence
        # must survive serialization so the decision can be independently rerun.
        report["gate"] = gate_report(report, workloads)
        report["passed"] = report["gate"]["passed"]
        report["at_parity_or_better_performance"] = report["gate"]["at_parity_or_better_performance"]
    except Exception as error:
        report["error"] = str(error)
        report["gate"] = {"workloads": [], "passed": False,
                          "at_parity_or_better_performance": False, "error": str(error)}
        if isinstance(error, SampleFailure):
            report["failed_run"] = error.details
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-cpp", type=Path, required=True)
    parser.add_argument("--development-cpp", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--test-root", type=Path, required=True)
    parser.add_argument("--workloads", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--release-source", type=Path, default=TARGETS["release"].source)
    parser.add_argument("--development-source", type=Path, default=TARGETS["development"].source)
    parser.add_argument("--release-build", type=Path, default=TARGETS["release"].build)
    parser.add_argument("--development-build", type=Path, default=TARGETS["development"].build)
    parser.add_argument("--release-cli", type=Path, default=TARGETS["release"].binary)
    parser.add_argument("--development-cli", type=Path, default=TARGETS["development"].binary)
    parser.add_argument("--samples", type=int, default=9)
    parser.add_argument("--warmups", type=int, default=3)
    args = parser.parse_args()
    report = run_campaign(args)
    print(json.dumps({"passed": report["passed"], "at_parity_or_better_performance": report["at_parity_or_better_performance"],
                      "report": str(args.report), "error": report.get("error")}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
