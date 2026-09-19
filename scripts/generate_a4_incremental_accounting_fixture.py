"""Generate deterministic A4 raw-report fixtures; never run this during measurement."""
import argparse
import hashlib
import json
from pathlib import Path

from reference_version import TARGETS


def report(target, rows, config):
    return {"engine_git_revision": "a4-fixture", "rust_source_sha256": "fixture", "rust_binary_sha256": "fixture",
            "harness_sha256": {"run_upstream.py": "fixture"}, "campaign_kind": "suite-campaign", "selection_kind": "suite",
            "worker_profile": "release", "timeout_seconds": 10, "execution_mode": config, "path_prefixes": [],
            "source_fingerprint_before": "fixture", "source_fingerprint_after": "fixture", "stale_source": False,
            "populations": {target: {"identity": {"revision": TARGETS[target].revision, "archive_sha256": "fixture"},
                                      "selected": [{"path": row["path"]} for row in rows], "results": rows}}}


def write(root, cases):
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()): raise FileExistsError("refusing to overwrite fixture")
    for name, target, state in (("baseline", "development", "passed"), ("current", "development", "passed")):
        rows = [{"path": f"test/a4/{index:05d}.test", "status": state} for index in range(cases)]
        if name == "current":
            rows[0]["status"] = "failed"; rows.pop(1)
        data = report(target, rows, "ordinary")
        release_rows = [{"path": f"test/a4/{index:05d}.test", "status": "passed"} for index in range(cases)]
        release = report("release", release_rows, "ordinary")["populations"]["release"]
        if name == "current": release["stale_source"] = True
        data["populations"]["release"] = release
        path = root / f"{name}.json"; path.write_text(json.dumps(data, indent=2) + "\n")
    manifest = {path.name: {"sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "bytes": path.stat().st_size} for path in sorted(root.glob("*.json"))}
    (root / "fixture-identities.json").write_text(json.dumps(manifest, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument("--root", type=Path, required=True); parser.add_argument("--cases", type=int, required=True); args = parser.parse_args()
    if args.cases < 2: raise ValueError("fixture needs at least two cases")
    if any(args.root.iterdir()) if args.root.exists() else False: raise FileExistsError("refusing to overwrite fixture")
    write(args.root, args.cases)


if __name__ == "__main__": main()
