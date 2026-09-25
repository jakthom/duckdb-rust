"""Serial, fail-closed Gate P measurement for the mapped API lifecycle."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time


PINS = {
    "development": "99063af2bd7092aff02e14184a20e24699d34d71",
    "release": "d8cdaa33fda8df955cc76ef58a280f68f4cd43fa",
}
METRICS = ("wall_ns", "cpu_ns", "max_rss_bytes", "block_input", "block_output")
MARKER = "G01_API_LIFECYCLE_PASS 4"
ROOT = Path(__file__).resolve().parents[1]


class SampleFailure(RuntimeError):
    def __init__(self, message, observation):
        super().__init__(message)
        self.observation = observation


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def manifest(path):
    path = Path(path).resolve(strict=True)
    data = json.loads(path.read_text())
    required = {
        "schema", "id", "source_ids", "rust_test", "operations",
        "samples", "warmups", "metrics",
    }
    if (
        set(data) != required
        or data["schema"] != 1
        or data["id"] != "g01_api_lifecycle_drop_transaction"
        or data["samples"] != 9
        or data["warmups"] != 3
        or data["metrics"] != [*METRICS, "throughput"]
        or data["source_ids"] != {
            "development": "test/api/test_api.cpp:73:1",
            "release": "test/api/test_api.cpp:70:1",
        }
        or data["rust_test"] != "transactional_ddl_constraints_and_abandonment"
        or not isinstance(data["operations"], list)
        or not data["operations"]
    ):
        raise ValueError("malformed or changed lifecycle manifest")
    return {"path": str(path), "sha256": digest(path), "data": data}


def require_reference(root, label):
    root = Path(root).resolve(strict=True)
    revision = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()
    if revision != PINS[label]:
        raise ValueError(f"wrong {label} source identity")
    if subprocess.check_output(
        ["git", "status", "--porcelain"], cwd=root, text=True
    ):
        raise ValueError(f"dirty {label} source")
    build = root / "build" / {
        "development": "engine-walkthrough",
        "release": "rewrite-reference",
    }[label]
    cache = build / "CMakeCache.txt"
    library = build / "src/libduckdb.dylib"
    cache_text = cache.read_text() if cache.is_file() else ""
    if (
        "CMAKE_BUILD_TYPE:STRING=Release" not in cache_text
        or f"CMAKE_HOME_DIRECTORY:INTERNAL={root}" not in cache_text
        or not library.is_file()
    ):
        raise ValueError(f"wrong CMake source/build/library for {label}")
    return build, library, {
        "source": str(root),
        "source_revision": revision,
        "cmake_cache": str(cache),
        "cmake_cache_sha256": digest(cache),
        "compiler_lines": [
            line for line in cache_text.splitlines()
            if "CMAKE_CXX_COMPILER" in line
        ],
    }


def compile_reference(root, label, output):
    build, library, identity = require_reference(root, label)
    source = ROOT / "scripts/g01_api_lifecycle_reference.cpp"
    command = [
        "c++", "-O3", "-DNDEBUG", "-std=c++17",
        "-I", str(Path(root).resolve() / "src/include"), str(source),
        "-L", str(library.parent), "-lduckdb",
        "-Wl,-rpath," + str(library.parent), "-o", str(output),
    ]
    result = subprocess.run(command, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError("C++ compile failed: " + result.stderr)
    return {
        **identity,
        "build": str(build),
        "compile_command": command,
        "compile_stdout": result.stdout,
        "compile_stderr": result.stderr,
        "reference_source_sha256": digest(source),
        "library_sha256": digest(library),
        "binary_sha256": digest(output),
    }


def timed(command, execute=subprocess.run):
    if platform.system() != "Darwin":
        raise RuntimeError("requires macOS /usr/bin/time -l")
    command = [str(part) for part in command]
    started = time.perf_counter_ns()
    result = execute(
        ["/usr/bin/time", "-l", *command], text=True, capture_output=True
    )
    observation = {
        "command": command,
        "returncode": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "wall_ns": time.perf_counter_ns() - started,
    }
    if result.returncode or result.stdout.strip() != MARKER:
        raise SampleFailure("lifecycle workload did not emit its PASS marker", observation)
    usage = re.search(
        r"(?m)^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys\s*$",
        result.stderr,
    )
    if not usage:
        raise SampleFailure("missing CPU time", observation)
    observation["cpu_ns"] = int(
        (float(usage.group(2)) + float(usage.group(3))) * 1_000_000_000
    )
    for key, label in {
        "max_rss_bytes": "maximum resident set size",
        "block_input": "block input operations",
        "block_output": "block output operations",
    }.items():
        match = re.search(rf"(?m)^\s*(\d+)\s+{re.escape(label)}\s*$", result.stderr)
        if not match:
            raise SampleFailure(f"missing {key}", observation)
        observation[key] = int(match.group(1))
    return observation


def active_peers(check_output=subprocess.check_output):
    output = check_output(["ps", "-axo", "pid=,ppid=,command="], text=True)
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
    needles = ("cargo ", "rustc", "cmake", "ninja", "unittest", "sqllogictest")
    return [
        command for pid, (_, command) in processes.items()
        if pid not in ancestors and any(needle in command.lower() for needle in needles)
    ]


def require_quiet_host():
    peers = active_peers()
    if peers:
        raise RuntimeError("host is not quiet: " + " | ".join(peers))


def validate_sample(sample):
    required = {"command", "returncode", "stdout", "stderr", *METRICS}
    if not isinstance(sample, dict) or not required <= set(sample):
        raise ValueError("raw observation is incomplete")
    if (
        sample["returncode"] != 0
        or sample["stdout"].strip() != MARKER
        or not isinstance(sample["command"], list)
        or not sample["command"]
        or sample["wall_ns"] <= 0
        or any(not isinstance(sample[key], int) or sample[key] < 0 for key in METRICS)
    ):
        raise ValueError("raw observation has no valid workload verdict or metrics")


def gate(cpp, rust, samples=9):
    if set(cpp) != set(PINS) or set(rust) != set(PINS):
        raise ValueError("gate requires both pinned reference populations")
    if any(len(rows) != samples for rows in [*cpp.values(), *rust.values()]):
        raise ValueError(f"requires exactly {samples} paired samples")
    for rows in [*cpp.values(), *rust.values()]:
        for sample in rows:
            validate_sample(sample)
    cpp_medians = {
        target: {
            metric: statistics.median(sample[metric] for sample in rows)
            for metric in METRICS
        }
        for target, rows in cpp.items()
    }
    rust_medians = {
        target: {
            metric: statistics.median(sample[metric] for sample in rows)
            for metric in METRICS
        }
        for target, rows in rust.items()
    }
    fastest = {
        metric: min(cpp_medians["release"][metric], cpp_medians["development"][metric])
        for metric in METRICS
    }

    def ratio(value, baseline):
        if value == baseline == 0:
            return 1.0
        return float("inf") if baseline == 0 else value / baseline

    ratios = {
        target: {
            metric: ratio(values[metric], fastest[metric]) for metric in METRICS
        }
        for target, values in rust_medians.items()
    }
    cpp_throughput = {
        target: 1 / values["wall_ns"] for target, values in cpp_medians.items()
    }
    rust_throughput = {
        target: 1 / values["wall_ns"] for target, values in rust_medians.items()
    }
    best_cpp_throughput = max(cpp_throughput.values())
    passed = (
        all(value <= 1 for values in ratios.values() for value in values.values())
        and all(value >= best_cpp_throughput for value in rust_throughput.values())
    )
    return {
        "cpp_medians": cpp_medians,
        "cpp_fastest": fastest,
        "rust_medians": rust_medians,
        "rust_over_fastest": ratios,
        "cpp_throughput": cpp_throughput,
        "best_cpp_throughput": best_cpp_throughput,
        "rust_throughput": rust_throughput,
        "passed": passed,
        "at_parity_or_better_performance": passed,
    }


def run_campaign(args):
    if args.output_dir.exists():
        raise FileExistsError("output exists")
    args.output_dir.mkdir(parents=True)
    report_path = args.output_dir / "report.json"
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "passed": False,
        "at_parity_or_better_performance": False,
    }
    try:
        spec = manifest(args.manifest)
        references = {
            label: compile_reference(root, label, args.output_dir / f"{label}-reference")
            for label, root in (
                ("development", args.development_root),
                ("release", args.release_root),
            )
        }
        revision = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
        ).strip()
        if subprocess.check_output(
            ["git", "status", "--porcelain"], cwd=ROOT, text=True
        ):
            raise ValueError("Rust source tree must be clean for Gate P")
        build_command = [
            "cargo", "build", "--release", "--no-default-features",
            "--bin", "g01-api-lifecycle",
        ]
        build = subprocess.run(build_command, cwd=ROOT, text=True, capture_output=True)
        if build.returncode:
            raise RuntimeError(build.stderr)
        candidate = ROOT / "target/release/g01-api-lifecycle"
        report["identity"] = {
            "manifest": spec,
            "rust_revision": revision,
            "rust_build_command": build_command,
            "rust_build_stdout": build.stdout,
            "rust_build_stderr": build.stderr,
            "rust_binary": str(candidate),
            "rust_binary_sha256": digest(candidate),
            "candidate_source_sha256": digest(ROOT / "benchmark/g01_api_lifecycle.rs"),
            "measurement_script_sha256": digest(Path(__file__)),
            "cargo_toml_sha256": digest(ROOT / "Cargo.toml"),
            "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
            "cxx": subprocess.check_output(["c++", "--version"], text=True).strip(),
            "references": references,
        }
        if not args.run:
            report["prepared"] = True
            report_path.write_text(json.dumps(report, indent=2) + "\n")
            return report
        require_quiet_host()
        paths = {
            "release": args.output_dir / "release-reference",
            "development": args.output_dir / "development-reference",
            "rust": candidate,
        }
        for _ in range(spec["data"]["warmups"]):
            for target in ("release", "development", "rust"):
                timed([paths[target]])
        observations = {
            "release": [], "development": [],
            "rust_release": [], "rust_development": [],
        }
        for index in range(spec["data"]["samples"]):
            order = (
                ("release", paths["release"]),
                ("rust_release", candidate),
                ("development", paths["development"]),
                ("rust_development", candidate),
            )
            if index % 2:
                order = (
                    ("development", paths["development"]),
                    ("rust_development", candidate),
                    ("release", paths["release"]),
                    ("rust_release", candidate),
                )
            for label, path in order:
                observations[label].append(timed([path]))
        decision = gate(
            {
                "release": observations["release"],
                "development": observations["development"],
            },
            {
                "release": observations["rust_release"],
                "development": observations["rust_development"],
            },
            spec["data"]["samples"],
        )
        report.update(
            observations=observations,
            gate=decision,
            passed=decision["passed"],
            at_parity_or_better_performance=decision["at_parity_or_better_performance"],
        )
    except Exception as error:
        report["error"] = str(error)
        if isinstance(error, SampleFailure):
            report["failed_observation"] = error.observation
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--development-root", type=Path, required=True)
    parser.add_argument("--release-root", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--run", action="store_true")
    args = parser.parse_args()
    report = run_campaign(args)
    print(json.dumps({
        "passed": report["passed"],
        "at_parity_or_better_performance": report["at_parity_or_better_performance"],
        "report": str(args.output_dir / "report.json"),
        "error": report.get("error"),
    }))
    if args.run and not report["passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
