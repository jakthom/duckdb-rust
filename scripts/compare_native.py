"""Measure Rust against pinned C++ DuckDB. Any measured median slowdown fails."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import selectors
import statistics
import subprocess
import tempfile

from upstream_suite import ROOT, digest
from reference_version import TARGETS, require_checkout, require_reference


class Worker:
    def __init__(self, binary, setup, query, phases=()):
        self.errors = tempfile.TemporaryFile(mode="w+t")
        self.process = subprocess.Popen([str(binary), str(setup), str(query), *map(str, phases)], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.errors, text=True, bufsize=1)
        self.events = selectors.DefaultSelector()
        self.events.register(self.process.stdout, selectors.EVENT_READ)
        try:
            self.metadata = self.read()
            if self.metadata.get("ready") is not True:
                raise ValueError("measurement worker did not initialize")
        except BaseException:
            self.close()
            raise

    def read(self):
        if not self.events.select(timeout=30):
            raise TimeoutError("measurement worker exceeded 30 seconds")
        line = self.process.stdout.readline()
        if not line:
            self.errors.seek(0)
            raise RuntimeError("measurement worker failed: " + self.errors.read()[:4000])
        return json.loads(line)

    def sample(self):
        self.process.stdin.write("sample\n")
        self.process.stdin.flush()
        return self.read()

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.process.stdout.close()
        self.events.close()
        self.errors.close()


def compare(cpp, rust, expected):
    """Never compensate one workload's regression with another's speedup."""
    for sample in [*cpp, *rust]:
        if (sample["rows"], sample["sum"]) != (expected["rows"], expected["sum"]):
            raise ValueError(f"incorrect result in {expected['name']}")
        if not isinstance(sample["elapsed_ns"], int) or sample["elapsed_ns"] <= 0:
            raise ValueError("invalid elapsed sample")
    if len(cpp) != len(rust) or len(cpp) < 9:
        raise ValueError("at least nine paired samples are required")
    before = statistics.median(s["elapsed_ns"] for s in cpp)
    after = statistics.median(s["elapsed_ns"] for s in rust)
    return {"cpp_median_ns": before, "rust_median_ns": after,
            "rust_over_cpp": after / before, "passed": after <= before}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, default="development")
    parser.add_argument("--cpp-source", type=Path)
    parser.add_argument("--cpp-build", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=9)
    parser.add_argument("--workloads", type=Path, default=ROOT / "benchmark/native_workloads.json")
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("preserve prior evidence: choose a new report path")
    if args.iterations < 9 or args.iterations % 2 == 0:
        raise ValueError("use an odd sample count of at least nine")
    target = TARGETS[args.target]
    source = (args.cpp_source or target.source).resolve()
    build = (args.cpp_build or target.build).resolve()
    revision = require_checkout(source, args.target)
    _, reference_identity = require_reference(build / target.binary.name, target=args.target)
    cache = (build / "CMakeCache.txt").read_text()
    if "CMAKE_BUILD_TYPE:STRING=Release\n" not in cache:
        raise ValueError("C++ baseline must use a release build")
    library = build / "src" / ("libduckdb.dylib" if platform.system() == "Darwin" else "libduckdb.so")
    cpp = ROOT / f"target/reference-measure-{args.target}"
    cpp.parent.mkdir(exist_ok=True)
    compile_command = ["c++", "-std=c++17", "-O3", "-DNDEBUG", "-I" + str(source / "src/include"),
                       str(ROOT / "benchmark/reference.cpp"), str(library), "-Wl,-rpath," + str(library.parent), "-o", str(cpp)]
    subprocess.run(compile_command, check=True)
    subprocess.run(["cargo", "build", "--offline", "--release", "--bin", "duckdb-rust-measure"], cwd=ROOT, check=True)
    rust = ROOT / "target/release/duckdb-rust-measure"
    source_hash = hashlib.sha256()
    for path in sorted([ROOT / "Cargo.toml", ROOT / "Cargo.lock", *(ROOT / "src").rglob("*.rs"), ROOT / "benchmark/native.rs"]):
        source_hash.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    workloads_path = args.workloads.resolve(strict=True)
    workloads = json.loads(workloads_path.read_text())
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "baseline": "pinned-cpp-duckdb",
              "reference_identity": reference_identity,
              "max_ratio": 1.0, "iterations": args.iterations, "warmups": 3,
              "order": "paired, alternating engine order", "cpp_revision": revision,
              "cpp_library_sha256": digest(library), "cpp_worker_sha256": digest(cpp),
              "cpp_worker_source_sha256": digest(ROOT / "benchmark/reference.cpp"),
              "cpp_build_configuration_sha256": digest(build / "CMakeCache.txt"), "compile_command": compile_command,
              "compiler": subprocess.check_output(["c++", "--version"], text=True).strip(),
              "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
              "rust_source_sha256": source_hash.hexdigest(), "rust_binary_sha256": digest(rust),
              "workloads_path": str(workloads_path), "workloads_sha256": digest(workloads_path), "platform": platform.platform(),
              "machine": platform.machine(), "workloads": [], "passed": False,
              "complete_performance_parity": False,
              "scope": "Serial in-memory embedded APIs against pinned C++. Query cases time execution and complete result validation. DDL cases reset state before every sample and verify effects after timing; their timing includes execution, result consumption and any required prepared-statement rebind. Setup, initial preparation, process startup and DDL reset/effect checks are untimed. Cold I/O, durable commits, concurrency, other APIs/tooling and full workloads remain unmeasured. Every ratio above 1 fails."}
    try:
        with tempfile.TemporaryDirectory(prefix="ddb-native-measure-") as temporary:
            temporary = Path(temporary)
            setup = temporary / "setup.sql"
            setup.write_text(workloads["setup"])
            query = temporary / "query.sql"
            for case in workloads["workloads"]:
                query.write_text(case["sql"])
                phases = []
                if ("reset" in case) != ("verify" in case):
                    raise ValueError("DDL cases require both reset and effect verification")
                if "reset" in case:
                    for phase in ["reset", "verify"]:
                        path = temporary / f"{phase}.sql"
                        path.write_text(case[phase])
                        phases.append(path)
                workers, samples = [], [[], []]
                try:
                    workers.append(Worker(cpp, setup, query, phases))
                    workers.append(Worker(rust, setup, query, phases))
                    source_id = workers[0].metadata["source_id"]
                    if len(source_id) < 10 or not revision.startswith(source_id):
                        raise ValueError("C++ library reports a different source revision")
                    for iteration in range(3 + args.iterations):
                        for index in ([0, 1] if iteration % 2 == 0 else [1, 0]):
                            sample = workers[index].sample()
                            if (sample["rows"], sample["sum"]) != (case["rows"], case["sum"]):
                                raise ValueError(f"incorrect result in {case['name']} engine {index}")
                            if iteration >= 3:
                                samples[index].append(sample)
                    result = {**case, "workers": [w.metadata for w in workers], "cpp": samples[0], "rust": samples[1], **compare(*samples, case)}
                    report["workloads"].append(result)
                finally:
                    for worker in workers:
                        worker.close()
        report["passed"] = len(report["workloads"]) == len(workloads["workloads"]) and all(c["passed"] for c in report["workloads"])
    except Exception as error:
        report["error"] = str(error)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "regressions": [c["name"] for c in report["workloads"] if not c["passed"]], "error": report.get("error"), "report": str(args.report)}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
