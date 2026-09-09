"""Check whether the SQL suite detects selected semantic faults in isolated copies."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

from upstream_suite import ROOT

MUTATIONS = [
    ("scalar-cardinality", "src/execution/subquery.rs", "if self.value.is_some() {", "if false {"),
    ("membership-null", "src/execution/subquery.rs", "if needle.is_null() || candidate.is_null() {", "if false {"),
    ("exists-negation", "src/execution/subquery.rs", "SubqueryRequest::Exists { negated } => return Ok(Some(Value::Boolean(!negated)))", "SubqueryRequest::Exists { negated } => return Ok(Some(Value::Boolean(negated)))"),
]


def run(workspace, log):
    with log.open("w") as output:
        result = subprocess.run(["cargo", "test", "--offline", "--manifest-path", str(workspace / "Cargo.toml"),
                                 "--target-dir", str(ROOT / "target/mutation-build" / workspace.name), "--test", "subqueries"],
                                cwd=workspace, stdout=output, stderr=subprocess.STDOUT, timeout=180)
    text = log.read_text()
    if result.returncode == 0 and "test result: ok." in text:
        return "survived"
    if result.returncode != 0 and "test result: FAILED" in text:
        return "killed"
    return "invalid"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("retain prior mutation evidence; choose a new report")
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(), "results": [], "passed": False,
              "scope": "Three explicit semantic mutants and the unchanged subquery conformance suite. Compile errors and timeouts do not count as detected semantic faults. This is not a whole-engine mutation-coverage result."}
    try:
        with tempfile.TemporaryDirectory(prefix="ddb-mutation-") as temporary:
            workspace = Path(temporary)
            for name in ["Cargo.toml", "Cargo.lock"]:
                shutil.copy2(ROOT / name, workspace / name)
            for name in ["src", "test", "tools", "benchmark", "examples"]:
                shutil.copytree(ROOT / name, workspace / name, ignore=shutil.ignore_patterns("upstream", "__pycache__"))
            source = hashlib.sha256()
            for path in sorted(p for p in workspace.rglob("*") if p.is_file()):
                source.update(str(path.relative_to(workspace)).encode()+b"\0"+path.read_bytes())
            report["source_sha256"] = source.hexdigest()
            logs = ROOT / "target/mutation-logs" / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%f")
            logs.mkdir(parents=True)
            baseline = run(workspace, logs / "baseline.log")
            report["baseline"] = baseline
            if baseline != "survived":
                raise RuntimeError("unchanged tests do not pass; mutation results would be invalid")
            for name, file, before, after in MUTATIONS:
                path = workspace / file
                original = path.read_text()
                if original.count(before) != 1:
                    raise ValueError(f"mutation anchor is ambiguous or stale: {name}")
                path.write_text(original.replace(before, after))
                log = logs / f"{name}.log"
                try:
                    status = run(workspace, log)
                except subprocess.TimeoutExpired:
                    status = "timeout"
                finally:
                    path.write_text(original)
                report["results"].append({"name": name, "file": file, "before": before, "after": after, "status": status,
                                          "log": str(log.relative_to(ROOT)), "log_sha256": hashlib.sha256(log.read_bytes()).hexdigest()})
                print(f"{name}: {status}", flush=True)
    except Exception as error:
        report["error"] = str(error)
    report["passed"] = "error" not in report and len(report["results"]) == len(MUTATIONS) and all(r["status"] == "killed" for r in report["results"])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2)+"\n")
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
