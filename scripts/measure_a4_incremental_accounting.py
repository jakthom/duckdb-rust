"""Fresh-process, dual-pin measurement of the public A4 accounting operation."""
import argparse
import csv
import hashlib
import json
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time

from measure_sqllogic_performance import METRICS, parse_time
from reference_version import require_checkout
from summarize_upstream import classify_incremental, elapsed_stages, observations

SAMPLES, WARMUPS = 21, 3


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def require_identity(path, expected):
    actual = digest(path)
    if expected is None or actual != expected:
        raise ValueError(f"immutable fixture digest required and differs: {path}")
    return actual


def file_identity(path):
    path = Path(path).resolve(strict=True)
    if not path.is_file(): raise ValueError(f"identity input is not a regular file: {path}")
    return {"path": str(path), "sha256": digest(path), "bytes": path.stat().st_size}


def reference_identity(args, name):
    source = getattr(args, f"{name}_source").resolve(strict=True)
    build = getattr(args, f"{name}_build").resolve(strict=True)
    worker = getattr(args, f"{name}_worker").resolve(strict=True)
    library = getattr(args, f"{name}_library").resolve(strict=True)
    cache = getattr(args, f"{name}_cmake_cache").resolve(strict=True)
    compiler = getattr(args, f"{name}_compiler")
    if compiler is None: raise ValueError(f"{name}: compiler is required")
    revision = require_checkout(source, name)
    expected_source = getattr(args, f"{name}_source_id")
    if not isinstance(expected_source, str) or len(expected_source) < 10 or not revision.startswith(expected_source):
        raise ValueError(f"{name}: expected SourceID does not identify the pinned checkout")
    if not worker.is_file() or not worker.stat().st_mode & 0o111: raise ValueError(f"{name}: classifier worker is not executable")
    if not cache.is_relative_to(build): raise ValueError(f"{name}: cache escapes build directory")
    if not library.is_relative_to(build): raise ValueError(f"{name}: library escapes build directory")
    compiler_version = subprocess.check_output([str(compiler), "--version"], text=True).strip()
    return {"revision": revision, "source_id": getattr(args, f"{name}_source_id"), "source": str(source), "build": str(build), "worker": file_identity(worker), "library": file_identity(library), "cmake_cache": file_identity(cache), "compiler": {"path": str(Path(compiler).resolve(strict=True)), "version": compiler_version},
            "helper": file_identity(Path(__file__).resolve()), "summary": file_identity(Path(__file__).with_name("summarize_upstream.py")), "cpp_source": file_identity(Path(__file__).parents[1] / "benchmark/reference_a4.cpp")}


def relations(baseline, current, directory):
    directory.mkdir(parents=True, exist_ok=False)
    left, right = observations(baseline), observations(current)
    left_keys, right_keys = {item.key for item in left}, {item.key for item in right}
    left_case, right_case = {}, {}
    for item in left: left_case.setdefault((item.path, item.pin), []).append(item)
    for item in right: right_case.setdefault((item.path, item.pin), []).append(item)
    # This is shared input normalization, not classification: a sole changed
    # configuration has one comparison key and an explicit evidence bit.
    remap, changed_left = {}, set()
    for case in set(left_case) & set(right_case):
        old = [item for item in left_case[case] if item.key not in right_keys]
        new = [item for item in right_case[case] if item.key not in left_keys]
        if len(old) == len(new) == 1:
            remap[new[0].key] = old[0].configuration
            changed_left.add(old[0].key)
    result = []
    for name, source in (("baseline", left), ("current", right)):
        path = directory / f"{name}.csv"
        with path.open("x", newline="") as output:
            writer = csv.writer(output)
            writer.writerow(("case_path", "pin", "runtime_config_digest", "status", "population_digest", "is_stale", "configuration_changed"))
            for item in source:
                writer.writerow((item.path, item.pin, remap.get(item.key, item.configuration), item.status, item.population, int(bool(item.stale)), int(item.key in remap or item.key in changed_left)))
        result.append(path)
    return result


def cpp_classifications(worker, baseline, current, directory, source_id):
    left, right = relations(baseline, current, directory)
    completed = subprocess.run([str(worker), str(left), str(right)], text=True, capture_output=True, check=True)
    lines = completed.stdout.splitlines()
    if not lines or not lines[0].startswith("READY\t"):
        raise ValueError("C++ classifier omitted source attestation")
    ready = lines.pop(0).split("\t")
    if len(ready) != 3 or len(ready[1]) < 10 or not ready[2] or not source_id.startswith(ready[1]):
        raise ValueError("C++ classifier source identity differs")
    result = {}
    for line in lines:
        row = line.split("\t")
        if len(row) != 4 or not all(row): raise ValueError("malformed C++ classifier row")
        key = tuple(row[:3])
        if key in result: raise ValueError("duplicate C++ classifier key")
        result[key] = row[3]
    return result


def _canonical_from_cpp(baseline, current, classifications):
    """Attach shared normalized evidence to independently classified C++ rows."""
    old, new = {item.key: item for item in observations(baseline)}, {item.key: item for item in observations(current)}
    old_keys, new_keys = set(old), set(new)
    old_case, new_case = {}, {}
    for item in old.values(): old_case.setdefault((item.path, item.pin), []).append(item)
    for item in new.values(): new_case.setdefault((item.path, item.pin), []).append(item)
    paired = {}
    for case in set(old_case) & set(new_case):
        left = [item for item in old_case[case] if item.key not in new_keys]
        right = [item for item in new_case[case] if item.key not in old_keys]
        if len(left) == len(right) == 1: paired[left[0].key] = right[0]
    keys = old_keys | (new_keys - {item.key for item in paired.values()})
    if keys != set(classifications): raise ValueError("C++ classifier population differs")
    result = []
    for key in sorted(keys):
        left, right = old.get(key), new.get(key)
        if right is None and key in paired: right = paired[key]
        kind = classifications[key]
        allowed = {"new_current_only", "lost_pass_omitted", "baseline_only_omitted", "uncomparable_configuration", "stale", "uncomparable_population", "fresh_failure", "unchanged_passed", "unchanged_failed", "unchanged_incomplete", "changed_nonpass"}
        if kind not in allowed: raise ValueError("unknown C++ classification")
        result.append({"path": key[0], "pin": key[1], "runtime_configuration_sha256": key[2], "classification": kind,
                       "fresh_failure": kind == "fresh_failure", "lost_pass": kind in {"fresh_failure", "lost_pass_omitted"},
                       "baseline_status": None if left is None else left.status, "current_status": None if right is None else right.status,
                       "stale_reasons": sorted(set((left.stale if left else ()) + (right.stale if right else ()))),
                       "baseline_provenance": None if left is None else left.provenance, "current_provenance": None if right is None else right.provenance,
                       "baseline_elapsed_seconds": None if left is None else left.elapsed_seconds,
                       "current_elapsed_seconds": None if right is None else right.elapsed_seconds})
    return result


def once(args):
    """Caller-visible operation: load JSON, normalize, classify, serialize JSON."""
    baseline, current = json.loads(args.baseline.read_text()), json.loads(args.current.read_text())
    if args.classifier == "python": rows = classify_incremental(baseline, current)["rows"]
    else:
        with tempfile.TemporaryDirectory(prefix="ddb-a4-once-") as temporary:
            classifications = cpp_classifications(args.cpp_worker, baseline, current, Path(temporary) / "relations", args.cpp_source_id)
        rows = _canonical_from_cpp(baseline, current, classifications)
    output = {"schema": "a4-incremental-v1", "baseline_sha256": digest(args.baseline), "current_sha256": digest(args.current), "rows": rows,
              "elapsed_stages": {"baseline": elapsed_stages(baseline), "current": elapsed_stages(current)}}
    args.output.write_text(json.dumps(output, sort_keys=True, separators=(",", ":")) + "\n")
    print(json.dumps({"rows": len(rows), "output_sha256": digest(args.output)}, sort_keys=True))


def run_timed(command, log_root):
    if platform.system() != "Darwin": raise RuntimeError("A4 acceptance requires macOS /usr/bin/time -l")
    started = time.perf_counter_ns()
    result = subprocess.run(["/usr/bin/time", "-l", *map(str, command)], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    wall_ns = time.perf_counter_ns() - started
    log_root.with_suffix(".stdout").write_text(result.stdout)
    log_root.with_suffix(".stderr").write_text(result.stderr)
    if result.returncode: raise RuntimeError(f"timed A4 child failed: {result.stderr[:4000]}")
    data = json.loads(result.stdout)
    values = parse_time(result.stderr)
    values["wall_ns"] = wall_ns
    if not isinstance(data.get("rows"), int) or data["rows"] <= 0: raise ValueError("A4 child emitted no rows")
    if wall_ns <= 0 or any(not isinstance(values[key], int) or values[key] < 0 for key in METRICS): raise ValueError("incomplete A4 resource sample")
    return {**values, "wall_ns": wall_ns, "rows_per_second": data["rows"] * 1e9 / wall_ns, **data}


def gate(samples):
    medians = {name: {metric: statistics.median(sample[metric] for sample in values) for metric in (*METRICS, "wall_ns")}
               for name, values in samples.items()}
    fastest = {metric: min(medians["release"][metric], medians["development"][metric]) for metric in medians["python"]}
    checks = {metric: medians["python"][metric] <= fastest[metric] for metric in fastest}
    throughput = {name: statistics.median(sample["rows_per_second"] for sample in values) for name, values in samples.items()}
    checks["throughput"] = throughput["python"] >= max(throughput["release"], throughput["development"])
    return {"medians": medians, "fastest_reference": fastest, "throughput": throughput, "checks": checks, "passed": all(checks.values())}


def validate_output(sample, output):
    if sample["output_sha256"] != digest(output):
        raise ValueError("child output hash differs")


def preflight(args, root):
    """Untimed semantic check: all three fresh children emit identical bytes."""
    hashes = {}
    for name, classifier, worker, source_id in (("python", "python", None, None), ("release", "cpp", args.release_worker, args.release_source_id), ("development", "cpp", args.development_worker, args.development_source_id)):
        output = root / f"preflight-{name}.json"
        command = [sys.executable, Path(__file__).resolve(), "--once", "--baseline", args.baseline, "--current", args.current, "--output", output, "--classifier", classifier]
        if worker is not None: command.extend(("--cpp-worker", worker, "--cpp-source-id", source_id))
        subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
        hashes[name] = digest(output)
    if len(set(hashes.values())) != 1: raise ValueError("preflight classifier outputs differ")
    return hashes


def measure(args):
    if args.report.exists(): raise FileExistsError("choose a fresh A4 report")
    if args.iterations != SAMPLES: raise ValueError("A4 acceptance requires exactly 21 samples")
    baseline_sha, current_sha = require_identity(args.baseline, args.baseline_sha256), require_identity(args.current, args.current_sha256)
    inputs_before = {"references": {name: reference_identity(args, name) for name in ("release", "development")}, "fixtures": {"baseline": file_identity(args.baseline), "current": file_identity(args.current)}}
    samples = {"python": [], "release": [], "development": []}
    outputs = {name: [] for name in samples}
    root = args.report.with_suffix(".artifacts")
    root.mkdir(parents=True, exist_ok=False)
    # Keep successful and failed child evidence under the caller's target/ tree.
    (root / "inputs-before.json").write_text(json.dumps(inputs_before, indent=2) + "\n")
    preflight_hashes = preflight(args, root)
    for iteration in range(WARMUPS + SAMPLES):
        order = ("python", "release", "development") if iteration % 2 == 0 else ("development", "release", "python")
        for name in order:
            output = root / f"{iteration}-{name}.json"
            command = [sys.executable, Path(__file__).resolve(), "--once", "--baseline", args.baseline, "--current", args.current, "--output", output,
                       "--classifier", "python" if name == "python" else "cpp", "--cpp-worker", getattr(args, f"{name}_worker", None) or args.release_worker,
                       "--cpp-source-id", getattr(args, f"{name}_source_id", None) or args.release_source_id]
            sample = run_timed(command, root / f"{iteration}-{name}")
            (root / f"{iteration}-{name}.sample.json").write_text(json.dumps(sample, indent=2) + "\n")
            validate_output(sample, output)
            outputs[name].append({"sha256": sample["output_sha256"], "bytes": output.stat().st_size})
            if iteration >= WARMUPS: samples[name].append(sample)
    if any(len(values) != SAMPLES for values in samples.values()): raise ValueError("incomplete A4 population")
    if len({item["sha256"] for values in outputs.values() for item in values}) != 1: raise ValueError("classifier output bytes differ")
    inputs_after = {"references": {name: reference_identity(args, name) for name in ("release", "development")}, "fixtures": {"baseline": file_identity(args.baseline), "current": file_identity(args.current)}}
    if inputs_before != inputs_after: raise ValueError("reference worker or build changed during A4 measurement")
    report = {"schema": "a4-incremental-measure-v1", "samples": SAMPLES, "warmups": WARMUPS, "baseline": {"sha256": baseline_sha}, "current": {"sha256": current_sha}, "inputs_before": inputs_before, "inputs_after": inputs_after,
              "workers": {name: {"path": str(getattr(args, f"{name}_worker", None) or args.release_worker), "sha256": digest(getattr(args, f"{name}_worker", None) or args.release_worker), "source_id": getattr(args, f"{name}_source_id", None) or args.release_source_id} for name in ("release", "development")},
              "preflight_output_sha256": preflight_hashes, "outputs": outputs, "observations": samples, "gate": gate(samples)}
    args.report.parent.mkdir(parents=True, exist_ok=True); args.report.write_text(json.dumps(report, indent=2) + "\n")
    if not report["gate"]["passed"]: raise SystemExit(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--once", action="store_true"); parser.add_argument("--baseline", type=Path, required=True); parser.add_argument("--current", type=Path, required=True); parser.add_argument("--output", type=Path)
    parser.add_argument("--classifier", choices=("python", "cpp")); parser.add_argument("--cpp-worker", type=Path); parser.add_argument("--cpp-source-id")
    parser.add_argument("--report", type=Path); parser.add_argument("--iterations", type=int, default=SAMPLES)
    parser.add_argument("--baseline-sha256"); parser.add_argument("--current-sha256")
    for name in ("release", "development"):
        parser.add_argument(f"--{name}-worker", type=Path); parser.add_argument(f"--{name}-source", type=Path); parser.add_argument(f"--{name}-build", type=Path); parser.add_argument(f"--{name}-library", type=Path); parser.add_argument(f"--{name}-cmake-cache", type=Path); parser.add_argument(f"--{name}-compiler", type=Path); parser.add_argument(f"--{name}-source-id")
    args = parser.parse_args()
    if args.once:
        if args.classifier is None or args.output is None or args.output.exists(): raise ValueError("--once needs a classifier and a fresh output")
        if args.classifier == "cpp" and (args.cpp_worker is None or not args.cpp_source_id): raise ValueError("C++ child needs worker and source ID")
        once(args)
    else:
        required = ("worker", "source", "build", "library", "cmake_cache", "compiler", "source_id")
        if args.report is None or any(getattr(args, f"{name}_{field}") is None for name in ("release", "development") for field in required): raise ValueError("measurement needs both pinned C++ source/build/library/cache/compiler/worker identities")
        measure(args)


if __name__ == "__main__": main()
