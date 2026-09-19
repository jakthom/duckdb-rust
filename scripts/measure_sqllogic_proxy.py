"""Gate Python SQLLogic-proxy process costs against both pinned C++ runners."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import measure_sqllogic_performance as measure
import run_upstream
import sqllogic


def digest(path):
    with Path(path).open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def save(report, path):
    Path(path).write_text(json.dumps(report, indent=2) + "\n")


def validate_report(report):
    """Fail closed before deriving a gate from proxy raw observations."""
    if report.get("schema") != "sqllogic-proxy-v1" or report.get("samples") != 21:
        raise ValueError("invalid proxy evidence schema or sample count")
    if report.get("proxy_configuration") != {"threads": 1, "worker": "release-attested", "mode": "python-proxy"}:
        raise ValueError("invalid proxy serial configuration")
    if not isinstance(report.get("inputs"), dict) or not report["inputs"] or not all(isinstance(v, str) and len(v) == 64 for v in report["inputs"].values()):
        raise ValueError("missing input hashes")
    for entry in report.get("workloads", []):
        obs = entry.get("observations", {})
        if set(obs) != {"release", "development", "proxy"}:
            raise ValueError("missing proxy observation population")
        commands = entry["commands"]
        for name, samples in obs.items():
            if len(samples) != 21 or not samples:
                raise ValueError("partial observations")
            if any(not isinstance(s.get("records"), int) or s["records"] <= 0 for s in samples):
                raise ValueError("zero or invalid records")
            if len({s["records"] for s in samples}) != 1:
                raise ValueError("unstable record count")
            if any(s.get("command") != commands[name] for s in samples):
                raise ValueError("tampered or wrong serial command")
            for sample in samples:
                measure.validate_observation(sample)
        if len({samples[0]["records"] for samples in obs.values()}) != 1:
            raise ValueError("non-equivalent runner record counts")
    if not report.get("workloads"):
        raise ValueError("missing workloads")
    return True


def once(worker, root, relative, timeout=60):
    worker = Path(worker).resolve(strict=True)
    root = Path(root).resolve(strict=True)
    path = (root / relative).resolve(strict=True)
    if not path.is_relative_to(root) or not path.is_file():
        raise ValueError("workload must be a regular file below test root")
    records = sqllogic.parse(path.read_text())
    deadline = time.monotonic() + timeout
    with tempfile.TemporaryDirectory(prefix="ddb-proxy-measure-") as scratch:
        engine = run_upstream.RustEngine(worker, scratch, deadline)
        try:
            runner = sqllogic.Runner(engine, {
                "{TEST_DIR}": scratch, "__TEST_DIR__": scratch,
                "{WORKING_DIRECTORY}": scratch, "__WORKING_DIRECTORY__": scratch,
                "{TEST_NAME}": relative, "{BASE_TEST_NAME}": relative.replace("/", "_"),
                "__SOURCE_DIR__": str(root),
            })
            runner.run(records)
            if runner.passed <= 0 or runner.skipped:
                raise ValueError("proxy workload did not produce a nonzero unskipped PASS count")
            return runner.passed
        finally:
            engine.close()


def proxy_command(args, path):
    return [sys.executable, str(Path(__file__).resolve()), "--once", "--worker", str(args.worker),
            "--test-root", str(args.test_root), "--path", path]


def campaign(args):
    if args.samples != 21 or args.warmups != 3:
        raise ValueError("acceptance requires exactly 21 samples and 3 warmups")
    if measure.active_peers():
        raise RuntimeError("quiet-host acceptance measurement blocked by active peers")
    workloads = measure.validate_manifest(args.workloads, args.test_root)
    worker, provenance = run_upstream.checked_worker_provenance(args.worker, args.worker_provenance)
    references = {
        "release": measure.identity("release", args.release_cpp, args.release_source, args.release_build,
                                    args.release_cli, args.test_root, workloads),
        "development": measure.identity("development", args.development_cpp, args.development_source,
                                        args.development_build, args.development_cli, args.test_root, workloads),
    }
    if Path(args.report).exists():
        raise FileExistsError("preserve prior evidence: choose a new report path")
    report = {"schema": "sqllogic-proxy-v1", "recorded_at": datetime.now(timezone.utc).isoformat(), "samples": args.samples,
              "warmups": args.warmups, "test_root": str(Path(args.test_root).resolve()),
              "workloads_manifest": str(Path(args.workloads).resolve()), "workloads": [],
              "references": references, "worker": {"path": str(worker), "sha256": digest(worker),
              "provenance": provenance}, "python": {"executable": sys.executable,
              "binary_sha256": digest(sys.executable), "version": sys.version, "sqllogic_sha256": digest(Path(__file__).with_name("sqllogic.py")),
              "proxy_sha256": digest(__file__)}, "execution_configuration": measure.SERIAL_CONFIGURATION,
              "proxy_configuration": {"threads": 1, "worker": "release-attested", "mode": "python-proxy"},
              "inputs": {name: digest(Path(__file__).with_name(name)) for name in ("run_upstream.py", "source_identity.py", "reference_version.py", "measure_sqllogic_performance.py", "sqllogic.py")} | {"workloads": digest(args.workloads)},
              "order": "paired alternating release, development, proxy", "passed": False}
    try:
      for workload in workloads:
        commands = {"release": [args.release_cpp, "--test-dir", args.test_root, workload["path"],
                    "--use-colour", "no", "--durations", "no", "--single-threaded"],
                    "development": [args.development_cpp, "--test-dir", args.test_root, workload["path"],
                    "--use-colour", "no", "--durations", "no", "--single-threaded"],
                    "proxy": proxy_command(args, workload["path"])}
        observed = {name: [] for name in commands}
        for round_number in range(args.warmups + args.samples):
            names = ["release", "development", "proxy"]
            names = names[round_number % 3:] + names[:round_number % 3]
            for name in names:
                observed[name].append(measure.run_timed(commands[name], "rust" if name == "proxy" else "cpp"))
        kept = {name: values[args.warmups:] for name, values in observed.items()}
        if any(len({sample["records"] for sample in values}) != 1 for values in kept.values()):
            raise ValueError("inconsistent record count: " + workload["id"])
        report["workloads"].append({**workload, "commands": commands, "observations": kept})
      validate_report(report)
      gate_inputs = {target: {"workloads": [{**item, "cpp": entry["observations"][target],
                     "rust": entry["observations"]["proxy"]} for item, entry in zip(workloads, report["workloads"])]}
                     for target in ("release", "development")}
      report["gate"] = measure.gate(gate_inputs["release"], gate_inputs["development"], workloads)
      report["passed"] = report["gate"]["passed"]
    except Exception as error:
      report["error"] = str(error)
      report["failed_invocation"] = getattr(error, "details", None)
    save(report, args.report)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--once", action="store_true"); parser.add_argument("--validate", type=Path); parser.add_argument("--worker", type=Path, required=False)
    parser.add_argument("--test-root", type=Path, required=True); parser.add_argument("--path")
    parser.add_argument("--worker-provenance", type=Path); parser.add_argument("--workloads", type=Path)
    parser.add_argument("--release-cpp", type=Path); parser.add_argument("--development-cpp", type=Path)
    parser.add_argument("--release-source", type=Path); parser.add_argument("--development-source", type=Path)
    parser.add_argument("--release-build", type=Path); parser.add_argument("--development-build", type=Path)
    parser.add_argument("--release-cli", type=Path); parser.add_argument("--development-cli", type=Path)
    parser.add_argument("--samples", type=int, default=21); parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if args.validate:
        validate_report(json.loads(args.validate.read_text())); return
    if args.once:
        if not args.path: parser.error("--once requires --path")
        count = once(args.worker, args.test_root, args.path)
        print(f"PASS {args.path} ({count} records)\n{count} records passed; 0 skipped")
        return
    required = ("workloads", "release_cpp", "development_cpp", "release_source", "development_source",
                "release_build", "development_build", "release_cli", "development_cli", "report")
    if any(getattr(args, name) is None for name in required): parser.error("campaign arguments are incomplete")
    result = campaign(args); print(json.dumps({"passed": result["passed"], "report": str(args.report)})); raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__": main()
