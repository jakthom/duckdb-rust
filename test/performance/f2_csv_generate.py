#!/usr/bin/env python3
"""Materialize deterministic F2 CSV inputs into a new caller-owned directory."""
import argparse
import csv
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / "benchmark/f2_csv_workloads.json"

EXPECTED_DIALECT = {"delimiter": ",", "quote": '"', "escape": '"',
                    "nullstr": "\\N", "header": True, "auto_detect": False, "threads": 1}
EXPECTED_WORKLOADS = [
    {"id": "f2_csv_narrow", "columns": 3, "shape": "INTEGER,VARCHAR,VARCHAR"},
    {"id": "f2_csv_wide", "columns": 17, "shape": "INTEGER,VARCHARx16"},
]


def validate_spec(spec):
    if (not isinstance(spec, dict)
            or set(spec) != {"version", "rows", "process_repetitions", "dialect", "workloads"}
            or type(spec["version"]) is not int or spec["version"] != 1
            or type(spec["rows"]) is not int or spec["rows"] <= 0
            or type(spec["process_repetitions"]) is not int or spec["process_repetitions"] != 16
            or spec["dialect"] != EXPECTED_DIALECT
            or spec["workloads"] != EXPECTED_WORKLOADS):
        raise ValueError("unsupported or malformed F2 workload specification")
    # JSON booleans and integers compare equal in Python; reject that ambiguity.
    if any(type(spec["dialect"][key]) is not type(value)
           for key, value in EXPECTED_DIALECT.items()):
        raise ValueError("invalid F2 dialect value types")
    if any(type(item["columns"]) is not int for item in spec["workloads"]):
        raise ValueError("invalid F2 column count type")

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def row(index, width):
    values = [index]
    for column in range(width - 1):
        if (index + column) % 97 == 0:
            values.append("\\N")
        elif (index + column) % 100 == 0:
            values.append(f"quoted,{column}\nrow-{index}")
        else:
            values.append(f"c{column}-{index % 1009}")
    return values

def expected(rows, width):
    totals = [0] * (width - 1)
    non_null = [0] * (width - 1)
    for index in range(rows):
        for column, value in enumerate(row(index, width)[1:]):
            if value != "\\N":
                totals[column] += len(value)
                non_null[column] += 1
    return {"count": rows, "sum_id": rows * (rows - 1) // 2,
            "length_sums": totals, "non_null_counts": non_null}

def query(path, width):
    columns = ["id INTEGER"] + [f"v{i} VARCHAR" for i in range(width - 1)]
    names = ["id"] + [f"v{i}" for i in range(width - 1)]
    aggregates = ["count(*)", "sum(id)"]
    aggregates += [f"sum(length({name}))" for name in names[1:]]
    aggregates += [f"count({name})" for name in names[1:]]
    schema = ",".join(f"'{name}':'{typ.split()[1]}'" for name, typ in zip(names, columns))
    escaped = str(path).replace("'", "''")
    return "SELECT " + ",".join(aggregates) + " FROM read_csv('" + escaped + "', columns={" + schema + "}, auto_detect=false, header=true, delim=',', quote='\"', escape='\"', nullstr='\\N')"

def materialize(output):
    output = output.resolve()
    if output.exists():
        raise FileExistsError("refuse to overwrite existing evidence: " + str(output))
    spec = json.loads(SPEC.read_text())
    validate_spec(spec)
    output.mkdir(parents=True)
    result = {"spec": str(SPEC.resolve()), "spec_sha256": digest(SPEC),
              "generator": str(Path(__file__).resolve()), "generator_sha256": digest(Path(__file__)),
              "rows": spec["rows"], "dialect": spec["dialect"],
              "process_repetitions": spec["process_repetitions"], "files": []}
    for item in spec["workloads"]:
        width = item["columns"]
        path = output / (item["id"] + ".csv")
        with path.open("w", newline="", encoding="utf-8") as file:
            writer = csv.writer(file, lineterminator="\n")
            writer.writerow(["id"] + [f"v{i}" for i in range(width - 1)])
            for index in range(spec["rows"]):
                writer.writerow(row(index, width))
        result["files"].append({"id": item["id"], "path": str(path), "sha256": digest(path),
            "bytes": path.stat().st_size, "columns": width, "expected": expected(spec["rows"], width),
            "query": query(path, width)})
    native = {"schema_version": 1, "rows": spec["rows"], "threads": 1, "setup": "", "workloads": []}
    process = {"workloads": []}
    for entry in result["files"]:
        total = entry["expected"]["count"] + entry["expected"]["sum_id"] + sum(entry["expected"]["length_sums"]) + sum(entry["expected"]["non_null_counts"])
        native["workloads"].append({"name": entry["id"], "sql": entry["query"], "rows": 1, "sum": str(total)})
        path = output / (entry["id"] + ".test")
        lines = []
        for _ in range(spec["process_repetitions"]):
            values = [entry["expected"]["count"], entry["expected"]["sum_id"], *entry["expected"]["length_sums"], *entry["expected"]["non_null_counts"]]
            lines.extend(["query " + "I" * (2 + 2 * (entry["columns"] - 1)), entry["query"], "----", "\t".join(map(str, values)), ""])
        path.write_text("\n".join(lines))
        process["workloads"].append({"id": entry["id"], "path": path.name})
        entry["process_fixture"] = {"path": str(path), "sha256": digest(path),
                                    "records": spec["process_repetitions"]}
    result["adapter_manifests"] = {}
    for name, data in (("native", native), ("process", process)):
        path = output / f"{name}-workloads.json"
        path.write_text(json.dumps(data, indent=2) + "\n")
        result["adapter_manifests"][name] = {"path": str(path), "sha256": digest(path)}
    (output / "manifest.json").write_text(json.dumps(result, indent=2) + "\n")
    return result

def verify(output):
    """Recheck frozen external inputs before and after an acceptance campaign."""
    output = output.resolve(strict=True)
    result = json.loads((output / "manifest.json").read_text())
    spec = json.loads(SPEC.read_text())
    validate_spec(spec)
    if (result.get("spec_sha256") != digest(SPEC)
            or result.get("generator_sha256") != digest(Path(__file__))
            or result.get("rows") != spec["rows"]
            or result.get("dialect") != spec["dialect"]
            or result.get("process_repetitions") != spec["process_repetitions"]):
        raise ValueError("stale F2 source or configuration")
    files = result.get("files", [])
    if [entry.get("id") for entry in files] != [entry["id"] for entry in spec["workloads"]]:
        raise ValueError("missing, reordered or duplicate F2 workload")
    for entry, declared in zip(files, spec["workloads"]):
        path = output / (declared["id"] + ".csv")
        fixture = output / (declared["id"] + ".test")
        if (entry.get("path") != str(path) or path.is_symlink()
                or entry.get("sha256") != digest(path)
                or entry.get("bytes") != path.stat().st_size
                or entry.get("columns") != declared["columns"]
                or entry.get("expected") != expected(spec["rows"], declared["columns"])
                or entry.get("query") != query(path, declared["columns"])
                or entry.get("process_fixture") != {"path": str(fixture),
                    "sha256": digest(fixture), "records": spec["process_repetitions"]}
                or fixture.is_symlink()):
            raise ValueError("changed F2 data, fixture or oracle")
    for name in ("native", "process"):
        path = output / f"{name}-workloads.json"
        if (result.get("adapter_manifests", {}).get(name) !=
                {"path": str(path), "sha256": digest(path)} or path.is_symlink()):
            raise ValueError("changed F2 adapter manifest")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    print(json.dumps(verify(args.output) if args.verify else materialize(args.output), indent=2))
