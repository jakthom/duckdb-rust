"""Prepare byte-independent B2 scalar-macro exchange witnesses for both pins.

The Rust command is an explicit argv template so this script never assumes a
foreign API: it must contain `{db}` and `{sql}` placeholders.  It records every
command, file digest and selected result under `target/` for later verification.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shlex
import subprocess

SQL = "CREATE MACRO main.b2(x, y := 2) AS CASE WHEN x > y THEN x + y ELSE y END"
CHECK = "SELECT b2(3), b2(y := 4, x := 1)"

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def run(command, db, sql):
    argv = [part.format(db=db, sql=sql) for part in shlex.split(command)]
    return subprocess.run(argv, check=True, text=True, capture_output=True).stdout

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-cli", required=True)
    parser.add_argument("--development-cli", required=True)
    parser.add_argument("--rust-command", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.out.exists(): raise FileExistsError("preserve evidence: output already exists")
    args.out.mkdir(parents=True)
    report = {"sql": SQL, "check": CHECK, "directions": []}
    for label, producer in (("release", args.release_cli), ("development", args.development_cli)):
        path = args.out / f"{label}-to-rust.duckdb"
        run(f"{shlex.quote(producer)} {{db}} -c {{sql}}", path, SQL)
        report["directions"].append({"producer": label, "consumer": "rust", "file": str(path), "sha256": digest(path), "result": run(args.rust_command, path, CHECK)})
    for label, consumer in (("release", args.release_cli), ("development", args.development_cli)):
        path = args.out / f"rust-to-{label}.duckdb"
        run(args.rust_command, path, SQL + "; CHECKPOINT")
        report["directions"].append({"producer": "rust", "consumer": label, "file": str(path), "sha256": digest(path), "result": run(f"{shlex.quote(consumer)} {{db}} -c {{sql}}", path, CHECK)})
    (args.out / "report.json").write_text(json.dumps(report, indent=2) + "\n")

if __name__ == "__main__": main()
