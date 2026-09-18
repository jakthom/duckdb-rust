"""Check grouping SQL against both explicitly pinned C++ references."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import tempfile

from reference_version import TARGETS, require_reference
from sql_reference import verify_corpus
from verify_reference import Engine, command

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "test/sql/grouping.test"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/release/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("preserve prior evidence: choose a new report")
    rust = Engine(args.rust.resolve(strict=True), True)
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(),
              "rust_binary_sha256": hashlib.sha256(rust.binary.read_bytes()).hexdigest(),
              "corpus": str(CORPUS.relative_to(ROOT)),
              "corpus_sha256": hashlib.sha256(CORPUS.read_bytes()).hexdigest(),
              "scope": "Unchanged local grouping SQL values and success/failure assertions, plus independent zero-argument GROUPING syntax outcomes. CLI startup/setup are not performance measurements. Exact diagnostics, complete upstream assertions and full compatibility remain open.",
              "targets": [], "common_corpus_passed": False, "passed": False,
              "complete_test_parity": False, "complete_compatibility": False}
    with tempfile.TemporaryDirectory(prefix="ddb-grouping-reference-") as directory:
        directory = Path(directory)
        for target, selected in TARGETS.items():
            trial = {"target": target, "records": [], "common_corpus_passed": False}
            report["targets"].append(trial)
            try:
                binary, trial["reference_identity"] = require_reference(target=target)
                reference = Engine(binary, False, serialize_json_rows=selected.serialize_json_rows)
                engines = [(rust, directory / f"{target}-rust.db"),
                           (reference, directory / f"{target}-cpp.db")]
                try:
                    verify_corpus(CORPUS, engines, command, trial["records"], fail_fast=False)
                    trial["common_corpus_passed"] = True
                except Exception as error:
                    trial["corpus_error"] = str(error)
                sql = "SELECT GROUPING() AS g FROM (VALUES(1))t(a) GROUP BY CUBE(a) ORDER BY g"
                outcomes = []
                for engine, path in engines:
                    try:
                        outcomes.append({"rows": command(engine, path, sql, json_output=True)})
                    except RuntimeError as error:
                        outcomes.append({"error": str(error)})
                trial["zero_argument_grouping"] = {
                    "sql": sql, "rust": outcomes[0], "cpp": outcomes[1],
                    "matches": outcomes[0] == outcomes[1],
                }
                trial["passed"] = trial["common_corpus_passed"] and trial["zero_argument_grouping"]["matches"]
            except Exception as error:
                trial.update(passed=False, error=str(error))
    report["common_corpus_passed"] = all(t["common_corpus_passed"] for t in report["targets"])
    report["passed"] = all(t["passed"] for t in report["targets"])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "common_corpus_passed": report["common_corpus_passed"],
                      "report": str(args.report)}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
