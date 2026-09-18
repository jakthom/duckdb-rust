"""Check native version/identity preservation across independent checkpoint mutations.

This is a header/publication diagnostic, not complete file compatibility or a
timing gate. Earlier runs are immutable. Both producer identities are retained.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import struct
import tempfile

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


def header(path):
    with path.open("rb") as stream:
        data = stream.read(12288)
    if len(data) != 12288 or data[8:12] != b"DUCK":
        raise ValueError("Missing native headers")
    integer = lambda offset: struct.unpack_from("<Q", data, offset)[0]
    selected = 4096 if integer(4104) > integer(8200) else 8192
    main, database = integer(12), integer(selected + 56)
    effective = database if main == 999 or database >= 69 else {0: 64, 1: 64, 2: 64, 3: 64, 4: 65, 5: 66, 6: 67, 7: 68, 64: 64}[database]
    return {"main": main, "database": database, "effective": effective,
            "identifier": data[124:140].hex(), "iteration": integer(selected + 8),
            "root": integer(selected + 16)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=ROOT / "target/release/duckdb-rust")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError("Preserve earlier evidence; choose a new report path")
    before = source_fingerprint()
    report = {"recorded_at": datetime.now(timezone.utc).isoformat(),
              "source_sha256": before, "rust_binary_sha256": digest(args.rust),
              "script_sha256": digest(Path(__file__)), "references": {}, "cases": [],
              "scope": __doc__, "full_parity": False}
    references = {}
    for target in ("development", "release"):
        require_checkout(TARGETS[target].source, target)
        binary, identity = require_reference(target=target)
        report["references"][target] = identity
        references[target] = Engine(binary, False, serialize_json_rows=False)
    rust = Engine(args.rust, True)
    query = "SELECT id,s,d::VARCHAR AS amount FROM t ORDER BY id"
    with tempfile.TemporaryDirectory(prefix="native-version-reference-") as directory:
        for target, versions in [("development", ["v1.0.0", "v1.2.0", "v1.5.0", "v2.0.0"]),
                                 ("release", ["v1.0.0", "v1.5.0"])]:
            for version in versions:
                path = Path(directory) / f"{target}-{version}.duckdb"
                case = {"target": target, "storage_version": version}
                report["cases"].append(case)
                try:
                    command(references[target], Path(":memory:"),
                            f"ATTACH '{path}' AS db (STORAGE_VERSION '{version}'); USE db; "
                            "CREATE TABLE t(id INTEGER PRIMARY KEY,s VARCHAR,d DECIMAL(8,2)); "
                            "INSERT INTO t VALUES(1,'initial',1.25); CHECKPOINT")
                    case["before"] = header(path)
                    command(rust, path, "UPDATE t SET s='changed',d=2.50 WHERE id=1; INSERT INTO t VALUES(2,'new',3.75)")
                    case["after"] = header(path)
                    case["rust_rows"] = command(rust, path, query, json_output=True, readonly=True)
                    case["reference_rows"] = command(references[target], path, query, json_output=True, readonly=True)
                    case["rows_match"] = case["rust_rows"] == case["reference_rows"] == [
                        {"id": 1, "s": "changed", "amount": "2.50"},
                        {"id": 2, "s": "new", "amount": "3.75"}]
                    case["version_preserved"] = case["before"]["effective"] == case["after"]["effective"]
                    case["identity_preserved"] = case["before"]["identifier"] == case["after"]["identifier"]
                    case["generation_advanced"] = case["after"]["iteration"] > case["before"]["iteration"]
                    case["passed"] = all(case[key] for key in ("rows_match", "version_preserved", "identity_preserved", "generation_advanced"))
                except Exception as error:
                    case["error"] = str(error)
                    case["passed"] = False
    report["source_unchanged"] = before == source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(c["passed"] for c in report["cases"])
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"report": str(args.report), "source_unchanged": report["source_unchanged"],
                      "passed": report["passed"], "cases": [{key: case.get(key) for key in
                      ("target", "storage_version", "rows_match", "version_preserved", "identity_preserved", "generation_advanced", "error")}
                      for case in report["cases"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
