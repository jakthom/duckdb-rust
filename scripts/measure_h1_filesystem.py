"""Prepared-artifact dual-pin H1 filesystem acceptance adapter."""
import argparse
import hashlib
import json
import math
import platform
import re
import statistics
import subprocess
import time
from pathlib import Path

from reference_version import TARGETS, require_checkout, require_reference
from run_upstream import worker_source_digest
from upstream_suite import ROOT, digest

WARMUPS, SAMPLES = 3, 21
METRICS = ("wall_ns", "cpu_ns", "max_rss_bytes", "block_input", "block_output")
FULLSYNC_REFERENCES = Path("/Users/jacobthomas/code/ddb/duckdb-rust/target/a2-fullfsync-reference")
RUST_INPUTS = ("Cargo.toml", "src/storage/filesystem.rs", "src/storage/filesystem/io.rs", "src/storage/filesystem/log.rs", "src/storage/filesystem/publication.rs", "src/storage/checkpoint.rs", "src/storage/logged.rs", "benchmark/h1_filesystem_native.rs")

def identity(path):
    path = Path(path).resolve(strict=True)
    return {"path": str(path), "sha256": digest(path)}

def text_identity(value):
    return {"sha256": hashlib.sha256(value.encode()).hexdigest(), "value": value}

def pattern(size):
    cycle = bytes((index * 31 + 7) & 255 for index in range(256))
    return cycle * (size // 256) + cycle[:size % 256]
def checksum(data):
    value = 0
    for byte in data: value = ((value * 257) + byte) & ((1 << 64) - 1)
    return value
def require_number(value, label):
    if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value) or value < 0: raise ValueError(f"{label} is not finite and non-negative")

def parse_resources(stderr):
    match = re.search(r"(?m)^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys\s*$", stderr)
    if not match: raise ValueError("missing /usr/bin/time CPU observation")
    result = {"cpu_ns": int((float(match.group(2)) + float(match.group(3))) * 1e9)}
    for label, key in (("maximum resident set size", "max_rss_bytes"), ("block input operations", "block_input"), ("block output operations", "block_output")):
        found = [line for line in stderr.splitlines() if line.strip().endswith(label)]
        if not found: raise ValueError(f"missing /usr/bin/time {label}")
        result[key] = int(float(found[-1].strip()[:-len(label)].strip().split()[0]))
    return result

def timed(command):
    """Keep stdout/stderr and an observation record even for failed children."""
    if platform.system() != "Darwin": raise RuntimeError("H1 process resources require Darwin /usr/bin/time -l")
    started = time.perf_counter_ns()
    run = subprocess.run(["/usr/bin/time", "-l", *map(str, command)], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    record = {"command": list(map(str, command)), "stdout": run.stdout, "stderr": run.stderr, "returncode": run.returncode, "wall_ns": time.perf_counter_ns() - started, "ok": False}
    try:
        if run.returncode: raise RuntimeError(f"timed child exit {run.returncode}")
        payload = json.loads(run.stdout)
        if not isinstance(payload, dict): raise ValueError("worker output is not an object")
        record.update(parse_resources(run.stderr)); record["payload"] = payload; record["ok"] = True
    except Exception as error: record["error"] = str(error)
    return record

def fixture(path, size, operation):
    data = pattern(size); path.parent.mkdir(parents=True, exist_ok=True)
    # Different old/new bytes make an omitted publication observable. Cleanup
    # must keep the old bytes; successful publication must install the seed.
    initial = data.translate(bytes(range(255, -1, -1))) if operation.startswith("publication") else data
    with path.open("xb") as output: output.write(initial)
    if operation.startswith("publication"):
        with path.with_suffix(".seed").open("xb") as output: output.write(data)
    return data
def validate_file_effect(path, expected, operation, before_names):
    after = expected.translate(bytes(range(255, -1, -1))) if operation == "publication-cleanup" else expected
    if path.read_bytes() != after:
        raise ValueError("publication or cleanup result mismatch")
    if {item.name for item in path.parent.iterdir()} != before_names:
        raise ValueError("publication left an unexpected staged file")
def ratio(value, reference): return 1.0 if value == reference == 0 else (float("inf") if reference == 0 else value / reference)

def reference_paths(target):
    directory = (FULLSYNC_REFERENCES / target).resolve(strict=True)
    return directory, directory / "provenance.json", directory / "CMakeCache.txt", directory / "compile_commands.json"
def fullsync_provenance(target, requested_source=None, requested_build=None):
    directory, provenance_path, cache_path, commands_path = reference_paths(target); provenance = json.loads(provenance_path.read_text())
    source = Path(provenance["source"]).resolve(strict=True)
    if requested_source and requested_source.resolve(strict=True) != source: raise ValueError("requested C++ source is not the attested full-sync source")
    if requested_build and requested_build.resolve(strict=True) != directory: raise ValueError("requested C++ build is not the attested full-sync build")
    revision = require_checkout(source, target); cli, _ = require_reference(directory / "duckdb", target=target)
    if revision != provenance.get("source_revision"): raise ValueError("provenance source revision disagrees with checkout")
    for name, expected in provenance.get("source_sha256", {}).items():
        if digest(source / "src/common" / name) != expected: raise ValueError(f"provenance source identity changed: {name}")
    expected_cli = {"path": str(cli.resolve()), "sha256": provenance["cli_identity"]["sha256"]}
    if identity(cli) != expected_cli: raise ValueError("provenance CLI identity changed")
    effective = provenance.get("compile_command", "") + "\n" + commands_path.read_text()
    cache = cache_path.read_text()
    if "-DHAVE_FULLFSYNC=1" not in effective: raise ValueError("full-sync provenance lacks effective -DHAVE_FULLFSYNC=1")
    if "HAVE_FULLFSYNC" in cache and not re.search(r"HAVE_FULLFSYNC[^\n]*=1", cache): raise ValueError("CMake cache contradicts full-sync compile flags")
    library = directory / "src" / ("libduckdb.dylib" if platform.system() == "Darwin" else "libduckdb.so")
    if digest(library) != provenance.get("alternate_artifact_sha256", {}).get("library"):
        raise ValueError("linked library differs from attested full-sync library")
    return {"directory": directory, "provenance": provenance, "provenance_path": provenance_path, "cache_path": cache_path, "commands_path": commands_path, "source": source, "revision": revision, "cli": cli, "library": library}
def compiler_identity():
    run = subprocess.run(["c++", "--version"], check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    return text_identity(run.stdout + run.stderr)
def validate_workloads(workloads):
    spec = json.loads(workloads.read_text()); operations = spec.get("operations") if isinstance(spec, dict) else None
    if not isinstance(spec, dict) or type(spec.get("bytes")) is not int or not 0 < spec["bytes"] <= 512 * 1024 * 1024: raise ValueError("workload bytes must be positive")
    if not isinstance(operations, list) or not operations: raise ValueError("workload spec must contain operations")
    names = set()
    for case in operations:
        if not isinstance(case, dict) or not isinstance(case.get("name"), str) or not case["name"] or not isinstance(case.get("operation"), str) or not case["operation"]: raise ValueError("invalid workload operation")
        if Path(case["name"]).name != case["name"] or case["name"] in (".", "..") or case["operation"] not in ("sequential-read", "positioned-read", "publication", "publication-cleanup"):
            raise ValueError("unsafe name or unsupported operation")
        if case["name"] in names: raise ValueError("workload operation names must be unique")
        names.add(case["name"])
    return spec

def prepare(args):
    if args.receipt.exists(): raise FileExistsError("preserve existing H1 preparation receipt")
    attested = fullsync_provenance(args.target, args.cpp_source, args.cpp_build); workloads = args.workloads.resolve(strict=True); validate_workloads(workloads)
    reference = args.receipt.parent / f"h1-reference-{args.target}"
    command = ["c++", "-std=c++17", "-O3", "-DNDEBUG", "-I" + str(attested["source"] / "src/include"), str(ROOT / "benchmark/h1_filesystem_reference.cpp"), str(attested["library"]), "-Wl,-rpath," + str(attested["library"].parent), "-o", str(reference)]
    subprocess.run(command, check=True); source_before = worker_source_digest()
    rust_command = ["cargo", "build", "--offline", "--release", "--no-default-features", "--bin", "duckdb-rust-h1-filesystem-measure"]
    subprocess.run(rust_command, cwd=ROOT, check=True); source_after = worker_source_digest()
    if source_before != source_after: raise ValueError("Rust dependency fingerprint changed during prepare")
    rust = ROOT / "target/release/duckdb-rust-h1-filesystem-measure"; args.receipt.parent.mkdir(parents=True, exist_ok=True)
    receipt = {"schema": "h1-prepared-v4", "target": args.target, "cpp_revision": attested["revision"], "cpp_source_revision": attested["provenance"]["source_revision"], "cpp_source_hashes": attested["provenance"]["source_sha256"], "cpp_cli": identity(attested["cli"]), "cpp_library": identity(attested["library"]), "cpp_worker": identity(reference), "cpp_source": identity(ROOT / "benchmark/h1_filesystem_reference.cpp"), "rust_worker": identity(rust), "rust_inputs": {path: identity(ROOT / path) for path in RUST_INPUTS}, "rust_source_digest_before_build": source_before, "rust_source_digest_after_build": source_after, "adapter": identity(__file__), "workloads": identity(workloads), "provenance": identity(attested["provenance_path"]), "cache": identity(attested["cache_path"]), "compile_commands": identity(attested["commands_path"]), "compiler": compiler_identity(), "commands": {"cpp": command, "rust": rust_command}}
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")

def verify_receipt(receipt, workloads, target):
    if receipt.get("schema") != "h1-prepared-v4" or receipt.get("target") != target: raise ValueError("wrong H1 preparation receipt")
    attested = fullsync_provenance(target)
    for key in ("cpp_cli", "cpp_library", "cpp_worker", "cpp_source", "rust_worker", "adapter", "workloads", "provenance", "cache", "compile_commands"):
        if identity(receipt[key]["path"]) != receipt[key]: raise ValueError(f"prepared identity changed: {key}")
    if identity(workloads) != receipt["workloads"]: raise ValueError("requested workload differs from prepared workload")
    if receipt.get("cpp_revision") != attested["revision"] or receipt.get("cpp_source_revision") != attested["provenance"]["source_revision"] or receipt.get("cpp_source_hashes") != attested["provenance"]["source_sha256"]: raise ValueError("C++ source/build provenance changed")
    if {path: identity(ROOT / path) for path in RUST_INPUTS} != receipt.get("rust_inputs"): raise ValueError("prepared Rust source changed")
    if worker_source_digest() != receipt.get("rust_source_digest_after_build"): raise ValueError("prepared Rust dependency fingerprint changed")
    if compiler_identity() != receipt.get("compiler"): raise ValueError("C++ compiler identity changed")

def validate_observation(record, case, size, engine, receipt):
    if not isinstance(record, dict) or record.get("ok") is not True or record.get("returncode") != 0: raise ValueError(f"{engine} observation failed")
    for metric in METRICS: require_number(record.get(metric), metric)
    payload = record.get("payload")
    if not isinstance(payload, dict) or payload.get("operation") != case["operation"] or payload.get("bytes") != size: raise ValueError("worker result shape")
    if not isinstance(payload.get("elapsed_ns"), int) or payload["elapsed_ns"] <= 0: raise ValueError("worker elapsed_ns must be positive")
    if engine == "cpp" and (not isinstance(payload.get("source_id"), str) or len(payload["source_id"]) < 7 or not receipt["cpp_revision"].startswith(payload["source_id"])): raise ValueError("C++ runtime source ID does not attest pin")
def summarize(samples): return {metric: statistics.median(sample[metric] for sample in samples) for metric in METRICS} | {"inner_latency_ns": statistics.median(sample["payload"]["elapsed_ns"] for sample in samples)}
def capture_case(report, fixture_root, case, size, receipt):
    row = {"name": case["name"], "operation": case["operation"], "cpp": {"warmups": [], "samples": []}, "rust": {"warmups": [], "samples": []}}
    for engine, binary in (("cpp", receipt["cpp_worker"]["path"]), ("rust", receipt["rust_worker"]["path"])):
        for index in range(WARMUPS + SAMPLES):
            destination = fixture_root / case["name"] / engine / str(index); phase = "warmups" if index < WARMUPS else "samples"; record = {"fixture": str(destination)}
            try:
                expected = fixture(destination, size, case["operation"])
                before_names = {item.name for item in destination.parent.iterdir()}
                record["initial_sha256"] = digest(destination)
                record["expected_seed_sha256"] = hashlib.sha256(expected).hexdigest()
                child = timed([binary, case["operation"], destination, size]); record.update(child)
                if child.get("ok"):
                    validate_observation(child, case, size, engine, receipt); payload = child["payload"]
                    if case["operation"].endswith("read") and payload.get("checksum") != checksum(expected): raise ValueError("read result checksum mismatch")
                    if case["operation"] in ("publication", "publication-cleanup"):
                        validate_file_effect(destination, expected, case["operation"], before_names)
            except Exception as error: record["ok"] = False; record["error"] = str(error)
            row[engine][phase].append(record)
            if record.get("ok") is not True: report["workloads"].append(row); raise RuntimeError(f"{case['name']} {engine} {phase} {index} failed")
    row["summary"] = {engine: summarize(row[engine]["samples"]) for engine in ("cpp", "rust")}; report["workloads"].append(row)

def measure(args):
    if args.report.exists(): raise FileExistsError("preserve prior H1 performance evidence")
    report = {"schema": "h1-process-v4", "prepared": None, "warmups": WARMUPS, "samples": SAMPLES, "metrics": METRICS, "workloads": [], "passed": False}
    try:
        receipt = json.loads(args.receipt.read_text()); workloads = args.workloads.resolve(strict=True); spec = validate_workloads(workloads); report["prepared"] = receipt; verify_receipt(receipt, workloads, args.target)
        fixture_root = args.report.parent / (args.report.stem + ".fixtures"); fixture_root.mkdir(parents=True, exist_ok=False)
        for case in spec["operations"]: capture_case(report, fixture_root, case, spec["bytes"], receipt)
        verify_receipt(receipt, workloads, args.target); report["source_digest_after_measure"] = worker_source_digest(); report["passed"] = True
    except Exception as error: report["error"] = str(error)
    args.report.parent.mkdir(parents=True, exist_ok=True); args.report.write_text(json.dumps(report, indent=2) + "\n"); raise SystemExit(0 if report["passed"] else 1)

def validate_report(report):
    if report.get("schema") != "h1-process-v4" or report.get("warmups") != WARMUPS or report.get("samples") != SAMPLES or tuple(report.get("metrics", METRICS)) != METRICS or report.get("passed") is not True or not isinstance(report.get("workloads"), list) or not report["workloads"]: raise ValueError("wrong, failed, or empty H1 report")
    receipt = report.get("prepared")
    if not isinstance(receipt, dict) or receipt.get("schema") != "h1-prepared-v4": raise ValueError("missing H1 receipt")
    workloads = Path(receipt.get("workloads", {}).get("path", "")).resolve(strict=True)
    verify_receipt(receipt, workloads, receipt.get("target"))
    if report.get("source_digest_after_measure") != receipt.get("rust_source_digest_after_build"): raise ValueError("stale or incomplete post-measure source identity")
    specification = validate_workloads(workloads)
    expected_cases = {case["name"]: case for case in specification["operations"]}
    rows = {}
    for row in report["workloads"]:
        name = row.get("name")
        if not isinstance(name, str) or name not in expected_cases or name in rows or row.get("operation") != expected_cases[name]["operation"]: raise ValueError("invalid workload population")
        calculated = {}
        for engine in ("cpp", "rust"):
            population = row.get(engine)
            if not isinstance(population, dict) or len(population.get("warmups", [])) != WARMUPS or len(population.get("samples", [])) != SAMPLES: raise ValueError("incomplete workload population")
            for observation in population["warmups"]: validate_observation(observation, row, specification["bytes"], engine, receipt)
            for observation in population["samples"]: validate_observation(observation, row, specification["bytes"], engine, receipt)
            calculated[engine] = summarize(population["samples"])
        if row.get("summary") != calculated: raise ValueError("fabricated or stale workload summary")
        rows[name] = (row, calculated)
    if set(rows) != set(expected_cases): raise ValueError("omitted configured workload")
    return receipt, rows

def gate(args):
    if len(args.reports) != 2: raise ValueError("gate requires exactly two reports")
    reports = [json.loads(path.read_text()) for path in args.reports]; validated = [validate_report(report) for report in reports]; receipts = [value[0] for value in validated]
    if {receipt.get("target") for receipt in receipts} != {"release", "development"}: raise ValueError("gate requires exactly one release and one development report")
    for key in ("rust_worker", "rust_inputs", "rust_source_digest_after_build", "adapter", "workloads"):
        if receipts[0].get(key) != receipts[1].get(key): raise ValueError(f"reports have different {key} identities")
    names = [set(rows) for _, rows in validated]
    if names[0] != names[1] or not names[0]: raise ValueError("reports do not contain the same nonempty workloads")
    output = []
    for name in sorted(names[0]):
        summaries = [rows[name][1] for _, rows in validated]; fastest = {metric: min(summary["cpp"][metric] for summary in summaries) for metric in METRICS}; fastest_inner = min(summary["cpp"]["inner_latency_ns"] for summary in summaries)
        bytes_count = validated[0][1][name][0]["cpp"]["samples"][0]["payload"]["bytes"]; fastest_throughput = max(bytes_count * 1e9 / summary["cpp"]["inner_latency_ns"] for summary in summaries); populations = []
        for report, summary in zip(reports, summaries):
            rust = summary["rust"]; throughput = bytes_count * 1e9 / rust["inner_latency_ns"]
            passed = ratio(rust["wall_ns"], fastest["wall_ns"]) <= 1 and ratio(rust["inner_latency_ns"], fastest_inner) <= 1 and throughput >= fastest_throughput and all(ratio(rust[metric], fastest[metric]) <= 1 for metric in METRICS if metric != "wall_ns")
            populations.append({"target": report["prepared"]["target"], "rust": rust, "throughput_bytes_per_second": throughput, "passed": passed})
        output.append({"name": name, "fastest_reference": {**fastest, "inner_latency_ns": fastest_inner, "throughput_bytes_per_second": fastest_throughput}, "rust_populations": populations, "passed": all(population["passed"] for population in populations)})
    result = {"schema": "h1-fastest-reference-v2", "reports": [str(path.resolve(strict=True)) for path in args.reports], "rust_worker": receipts[0]["rust_worker"], "workloads": output, "passed": all(row["passed"] for row in output)}
    if args.report.exists(): raise FileExistsError("preserve prior H1 gate evidence")
    args.report.parent.mkdir(parents=True, exist_ok=True); args.report.write_text(json.dumps(result, indent=2) + "\n"); raise SystemExit(0 if result["passed"] else 1)

def main():
    parser = argparse.ArgumentParser(); parser.add_argument("--prepare", action="store_true"); parser.add_argument("--measure", action="store_true"); parser.add_argument("--gate", action="store_true"); parser.add_argument("--target", choices=TARGETS); parser.add_argument("--cpp-source", type=Path); parser.add_argument("--cpp-build", type=Path); parser.add_argument("--receipt", type=Path); parser.add_argument("--report", type=Path); parser.add_argument("--reports", nargs=2, type=Path); parser.add_argument("--workloads", type=Path, default=ROOT / "benchmark/h1_filesystem_workloads.json"); args = parser.parse_args()
    if args.gate:
        if args.prepare or args.measure or not args.report or not args.reports: parser.error("gate requires exactly two reports and output")
        gate(args); return
    if args.prepare == args.measure or not args.target or not args.receipt: parser.error("choose exactly one phase with target and receipt")
    if args.prepare: prepare(args)
    elif args.report: measure(args)
    else: parser.error("measure requires report")
if __name__ == "__main__": main()
