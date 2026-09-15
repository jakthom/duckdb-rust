"""Freeze configured extension refs without fetching or claiming ABI compatibility."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

PIN = "99063af2bd7092aff02e14184a20e24699d34d71"
CALL = re.compile(r"duckdb_extension_load\(\s*([A-Za-z0-9_]+)(.*?)\)", re.S)
KEY = re.compile(r"^\s*(GIT_URL|GIT_TAG|LOAD_TESTS|DONT_LINK|APPLY_PATCHES|TEST_DIR)(?:\s+([^\s)]+))?\s*$", re.M)


def digest(path): return hashlib.sha256(path.read_bytes()).hexdigest()

def active(text): return "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args(); root = args.source_root
    if args.output_dir.exists(): raise ValueError(f"output already exists: {args.output_dir}")
    if not root.is_dir(): raise ValueError(f"source root missing: {root}")
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    if head != PIN: raise ValueError(f"development pin mismatch: {head}")
    if subprocess.check_output(["git", "status", "--porcelain"], cwd=root, text=True): raise ValueError("development root is dirty")
    frozen = json.loads(args.manifest.read_text())
    if frozen.get("schema") != 1 or frozen.get("development_pin") != PIN:
        raise ValueError("unsupported or wrong-pin extension manifest")
    configs = sorted((root / ".github/config/extensions").glob("*.cmake"))
    refs = []
    for path in configs:
        for name, body in CALL.findall(active(path.read_text())):
            values = {key: value.strip('"') if value else True for key, value in KEY.findall(body)}
            tag, url = values.get("GIT_TAG"), values.get("GIT_URL")
            if not url or not tag or not re.fullmatch(r"[0-9a-f]{40}", str(tag)):
                raise ValueError(f"malformed or unpinned configured ref: {path.relative_to(root)}:{name}")
            patches = sorted(p.relative_to(root).as_posix() for p in (root / ".github/patches/extensions" / name).glob("**/*") if p.is_file())
            refs.append({"name": name, "config": path.relative_to(root).as_posix(), "config_sha256": digest(path),
                         "git_url": url, "git_tag": tag, "resolved_ref": tag, "ref_status": "resolved_immutable_config_ref",
                         "remote_checkout": "absent_not_fetched", "patches": patches,
                         "load_tests": "LOAD_TESTS" in values, "dont_link": "DONT_LINK" in values,
                         "abi_classification": "internal_cpp_coupled",
                         "abi_note": "External implementation is not locally checked out; no binary compatibility claim."})
    in_tree = sorted(p.name for p in (root / "extension").iterdir() if p.is_dir() and p.name != "delta")
    actual_refs = {item["name"]: item["git_tag"] for item in refs}
    if actual_refs != frozen.get("configured_external"):
        raise ValueError("configured external refs differ from frozen manifest")
    if in_tree != frozen.get("in_tree_extensions"):
        raise ValueError("in-tree extensions differ from frozen manifest")
    surfaces = [
        {"abi_classification": "stable_c_table", "header": "src/include/duckdb_extension.h", "compatibility": "version-gated stable table prefix"},
        {"abi_classification": "unstable_c_table", "header": "src/include/duckdb_extension.h", "compatibility": "exact matching DuckDB build/version"},
        {"abi_classification": "cpp_wrapper_over_c_v2", "header": "tools/cpp/duckdb_cpp_extension.hpp", "compatibility": "wrapper routes through C v2; demo uses pinned unstable surface"},
        {"abi_classification": "internal_cpp_coupled", "header": "src/include/duckdb/main/extension.hpp", "compatibility": "internal C++ extensions require compatible build/version/platform"},
    ]
    args.output_dir.mkdir(parents=True)
    report = {"schema": 1, "development_pin": PIN, "in_tree_extensions": in_tree,
              "configured_external": refs, "abi_surfaces": surfaces,
              "status": "inventoried_not_loader_validated",
              "gate_p": {"status": "open", "reason": "no comparable C++ inventory operation; no timing claimed"}}
    (args.output_dir / "extension-pin-inventory.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"configured_external": len(refs), "in_tree": len(in_tree), "report": str(args.output_dir / "extension-pin-inventory.json")}, sort_keys=True))


if __name__ == "__main__": main()
