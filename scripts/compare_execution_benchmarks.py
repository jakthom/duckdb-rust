"""Compare a saved execution baseline with an equivalent measured run."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import statistics


def read(path):
    raw = path.read_bytes()
    report = json.loads(raw)
    if report["suite"] != "execution" or report["correctness"] != "passed":
        raise ValueError("only successful execution benchmark reports are comparable")
    return report, hashlib.sha256(raw).hexdigest()


def cases(report):
    results = {}
    for case in report["results"]:
        key = (case["workload"], dict(case["adapters"])["executor"])
        samples = case["samples"]
        if key in results or len(samples) != report["iterations"]:
            raise ValueError("duplicate workload or inconsistent sample count")
        if not samples or any(s["elapsed_ns"] <= 0 for s in samples):
            raise ValueError("elapsed samples must be nonempty and positive")
        checksums = {(s["rows"], s["sum"]) for s in samples}
        if len(checksums) != 1:
            raise ValueError("workload samples disagree on correctness")
        results[key] = (statistics.median(s["elapsed_ns"] for s in samples), checksums, case["sql"])
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("current", type=Path)
    parser.add_argument("--max-ratio", type=float, default=1.0)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if not 0 < args.max_ratio <= 1.0:
        raise ValueError("the regression limit must be positive and cannot permit a slowdown")
    baseline, baseline_hash = read(args.baseline)
    current, current_hash = read(args.current)
    for field in ["suite", "rows", "iterations", "batch_size", "warmups", "max_intermediate_rows", "rustc", "platform"]:
        if baseline[field] != current[field]:
            raise ValueError(f"benchmark configuration differs: {field}")
    before, after = cases(baseline), cases(current)
    if not before or before.keys() != after.keys():
        raise ValueError("benchmark workload sets differ or are empty")
    comparisons = []
    for (workload, executor), (elapsed, checksum, sql) in after.items():
        previous, expected, previous_sql = before[(workload, executor)]
        if checksum != expected or sql != previous_sql:
            raise ValueError("benchmark workload or correctness checksum changed")
        ratio = elapsed / previous
        comparisons.append({"workload": workload, "executor": executor, "baseline_median_ns": previous, "current_median_ns": elapsed, "ratio": ratio, "passed": ratio <= args.max_ratio})
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "max_ratio": args.max_ratio,
        "baseline": {"report": str(args.baseline), "report_sha256": baseline_hash, "source_sha256": baseline["source_sha256"], "binary_sha256": baseline["binary_sha256"]},
        "current": {"report": str(args.current), "report_sha256": current_hash, "source_sha256": current["source_sha256"], "binary_sha256": current["binary_sha256"]},
        "comparisons": comparisons,
        "passed": all(c["passed"] for c in comparisons),
        "scope": "Per-workload median comparison of serial in-memory execution; five samples do not establish a statistical confidence interval or a broader workload acceptance result.",
    }
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(f"{'passed' if report['passed'] else 'failed'}; maximum median ratio {max(c['ratio'] for c in comparisons):.3f}; limit {args.max_ratio}")
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
