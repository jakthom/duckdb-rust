#!/usr/bin/env python3
"""Materialize deterministic F2 CSV inputs into a new caller-owned directory."""
import argparse, csv, hashlib, json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / "benchmark/f2_csv_workloads.json"

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
    output.mkdir(parents=True)
    result = {"spec": str(SPEC), "spec_sha256": digest(SPEC), "rows": spec["rows"], "files": []}
    for item in spec["workloads"]:
        width = item["columns"]
        path = output / (item["id"] + ".csv")
        with path.open("w", newline="") as file:
            writer = csv.writer(file, lineterminator="\n")
            writer.writerow(["id"] + [f"v{i}" for i in range(width - 1)])
            for index in range(spec["rows"]): writer.writerow(row(index, width))
        result["files"].append({"id": item["id"], "path": str(path), "sha256": digest(path),
            "bytes": path.stat().st_size, "columns": width, "expected": expected(spec["rows"], width),
            "query": query(path, width)})
    (output / "manifest.json").write_text(json.dumps(result, indent=2) + "\n")
    native = {"schema_version": 1, "rows": spec["rows"], "threads": 1, "setup": "", "workloads": []}
    process = {"workloads": []}
    for entry in result["files"]:
        total = entry["expected"]["count"] + entry["expected"]["sum_id"] + sum(entry["expected"]["length_sums"]) + sum(entry["expected"]["non_null_counts"])
        native["workloads"].append({"name": entry["id"], "sql": entry["query"], "rows": 1, "sum": str(total)})
        path = output / (entry["id"] + ".test")
        lines = []
        for _ in range(16):
            values = [entry["expected"]["count"], entry["expected"]["sum_id"], *entry["expected"]["length_sums"], *entry["expected"]["non_null_counts"]]
            lines.extend(["query " + "I" * (2 + 2 * (entry["columns"] - 1)), entry["query"], "----", "\t".join(map(str, values)), ""])
        path.write_text("\n".join(lines))
        process["workloads"].append({"id": entry["id"], "path": path.name})
    (output / "native-workloads.json").write_text(json.dumps(native, indent=2) + "\n")
    (output / "process-workloads.json").write_text(json.dumps(process, indent=2) + "\n")
    return result

if __name__ == "__main__":
    parser = argparse.ArgumentParser(); parser.add_argument("output", type=Path)
    print(json.dumps(materialize(parser.parse_args().output), indent=2))
