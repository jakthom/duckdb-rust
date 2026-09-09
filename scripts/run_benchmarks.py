"""Build and record an adapter comparison with source provenance."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--suite", choices=["execution", "compression", "casts", "types", "operators", "subqueries"], default="execution")
    parser.add_argument("--rows", type=int, default=50000)
    parser.add_argument("--iterations", type=int, default=5)
    parser.add_argument("--batch-size", type=int, default=256)
    args = parser.parse_args()
    subprocess.run(["cargo", "build", "--offline", "--release", "--bin", "duckdb-rust-benchmark"], cwd=ROOT, check=True)
    binary = ROOT / "target/release/duckdb-rust-benchmark"
    source_hash = hashlib.sha256()
    for path in sorted([ROOT / "Cargo.toml", ROOT / "Cargo.lock", *(ROOT / "src").rglob("*.rs"), *(ROOT / "benchmark").rglob("*.rs")]):
        source_hash.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    report = json.loads(subprocess.check_output([str(binary), "--suite", args.suite, "--rows", str(args.rows), "--iterations", str(args.iterations), "--batch-size", str(args.batch_size)], text=True))
    report.update({
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "source_sha256": source_hash.hexdigest(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "platform": platform.platform(),
        "scope": {
            "execution": "In-memory, prepared SQL with rebinding, serial execution, validation included; no DuckDB performance comparison.",
            "compression": "In-memory DuckDB bitpacking segments; checked registry dispatch, allocation, physical type checks and full-value correctness included. No file I/O or DuckDB performance comparison.",
            "operators": "Checked bound LIKE operations and prepared SQL filtering; inputs and database setup precede timing, validation and correctness checks are included. No DuckDB performance comparison.",
            "subqueries": "Serial prepared SQL subqueries against one statement snapshot; setup precedes timing, binding/planning/execution and correctness checks are included. No file I/O or DuckDB performance comparison.",
            "types": "Checked registered-type comparisons, canonical keys and SQL grouping; setup precedes timing, correctness checks are included. No file I/O or DuckDB performance comparison.",
            "casts": "Checked bound casts and prepared SQL rebinding/aggregation over signed decimal integer strings; allocation, type checks and checksum included. No file I/O or DuckDB performance comparison.",
        }[args.suite],
    })
    output = json.dumps(report, indent=2) + "\n"
    if args.report:
        args.report.write_text(output)
    print(output, end="")
    if report["correctness"] != "passed" or not report.get("selection_budget", {}).get("passed", True):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
