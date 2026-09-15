"""Freeze both configured extension populations without fetching external sources."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

PINS = {"development": "99063af2bd7092aff02e14184a20e24699d34d71", "release": "d8cdaa33fda8df955cc76ef58a280f68f4cd43fa"}
CALL = re.compile(r"duckdb_extension_load\(\s*([A-Za-z0-9_]+)(.*?)\)", re.S)
KEY = re.compile(r"^\s*(GIT_URL|GIT_TAG|LOAD_TESTS|DONT_LINK|APPLY_PATCHES|TEST_DIR)(?:\s+([^\s)]+))?\s*$", re.M)

def digest(path): return hashlib.sha256(path.read_bytes()).hexdigest()
def active(text): return "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))

def collect(root, label):
    if not root.is_dir(): raise ValueError(f"{label} root missing: {root}")
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    if head != PINS[label]: raise ValueError(f"{label} pin mismatch: {head}")
    if subprocess.check_output(["git", "status", "--porcelain"], cwd=root, text=True): raise ValueError(f"{label} root is dirty")
    refs = []
    for path in sorted((root / ".github/config/extensions").glob("*.cmake")):
        for name, body in CALL.findall(active(path.read_text())):
            values = {key: value.strip('"') if value else True for key, value in KEY.findall(body)}
            tag, url = values.get("GIT_TAG"), values.get("GIT_URL")
            if not url or not tag or not re.fullmatch(r"[0-9a-f]{40}", str(tag)):
                raise ValueError(f"malformed or unpinned configured ref: {path.relative_to(root)}:{name}")
            patches = sorted(({"path": p.relative_to(root).as_posix(), "sha256": digest(p)} for p in (root / ".github/patches/extensions" / name).glob("**/*") if p.is_file()), key=lambda item: item["path"])
            refs.append({"id": f"{label}:{name}", "name": name, "config": path.relative_to(root).as_posix(), "config_sha256": digest(path), "git_url": url, "git_tag": tag, "configured_ref": tag, "ref_status": "immutable_syntax_unverified_existence", "remote_checkout": "absent_not_fetched", "patches": patches, "load_tests": "LOAD_TESTS" in values, "dont_link": "DONT_LINK" in values, "abi_classification": "internal_cpp_coupled", "abi_note": "External implementation is not locally checked out; no binary compatibility claim."})
    return {"pin": PINS[label], "in_tree_extensions": sorted(p.name for p in (root / "extension").iterdir() if p.is_dir() and p.name != "delta"), "static_registrations": [line.strip() for line in (root / "extension/extension_config.cmake").read_text().splitlines() if line.strip().startswith("duckdb_extension_load(")], "configured_external": refs}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--release-root", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    if args.output_dir.exists(): raise ValueError(f"output already exists: {args.output_dir}")
    frozen = json.loads(args.manifest.read_text())
    if frozen.get("schema") != 2 or frozen.get("pins") != PINS: raise ValueError("unsupported or wrong-pin extension manifest")
    populations = {"development": collect(args.source_root, "development"), "release": collect(args.release_root, "release")}
    for label, population in populations.items():
        expected = frozen["populations"][label]
        actual_refs = {item["name"]: item["git_tag"] for item in population["configured_external"]}
        if actual_refs != expected["configured_external"] or population["in_tree_extensions"] != expected["in_tree_extensions"] or population["static_registrations"] != expected["static_registrations"]: raise ValueError(f"{label} extension population differs from frozen manifest")
    surfaces = [{"abi_classification": "stable_c_table", "header": "src/include/duckdb_extension.h", "compatibility": "version-gated stable table prefix"}, {"abi_classification": "unstable_c_table", "header": "src/include/duckdb_extension.h", "compatibility": "exact matching DuckDB build/version"}, {"abi_classification": "cpp_wrapper_over_c_v2", "header": "tools/cpp/duckdb_cpp_extension.hpp", "compatibility": "wrapper routes through C v2; demo uses pinned unstable surface"}, {"abi_classification": "internal_cpp_coupled", "header": "src/include/duckdb/main/extension.hpp", "compatibility": "internal C++ extensions require compatible build/version/platform"}]
    args.output_dir.mkdir(parents=True)
    development_refs = {item["name"]: item["git_tag"] for item in populations["development"]["configured_external"]}
    release_refs = {item["name"]: item["git_tag"] for item in populations["release"]["configured_external"]}
    report = {"schema": 2, "pins": PINS, "populations": populations, "abi_surfaces": surfaces,
              "pin_diffs": {"development_only": sorted(development_refs.keys() - release_refs.keys()), "release_only": sorted(release_refs.keys() - development_refs.keys()), "tag_changed": sorted(name for name in development_refs.keys() & release_refs.keys() if development_refs[name] != release_refs[name]), "in_tree_changed": populations["development"]["in_tree_extensions"] != populations["release"]["in_tree_extensions"], "static_registration_changed": populations["development"]["static_registrations"] != populations["release"]["static_registrations"]},
              "status": "inventoried_not_loader_validated", "gate_p": {"status": "open", "reason": "no comparable C++ inventory operation; no timing claimed"}}
    (args.output_dir / "extension-pin-inventory.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"development_configured_external": len(populations["development"]["configured_external"]), "release_configured_external": len(populations["release"]["configured_external"]), "report": str(args.output_dir / "extension-pin-inventory.json")}, sort_keys=True))

if __name__ == "__main__": main()
