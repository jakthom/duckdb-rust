"""Fail-closed, source-derived inventory of native/API/client assertions.

This deliberately maps only assertions with an explicitly named Rust public
contract test.  It is not an ABI adapter and an unmapped assertion is evidence,
not a successful substitution with a SQL smoke test.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

PINS = {
    "development": "99063af2bd7092aff02e14184a20e24699d34d71",
    "release": "d8cdaa33fda8df955cc76ef58a280f68f4cd43fa",
}
ROOTS = ("test/api/", "test/py/", "tools/cpp/tests/", "tools/swift/duckdb-swift/Tests/")
ASSERTION = re.compile(r"\b(?:REQUIRE|CHECK|ASSERT)(?:_[A-Z]+)*\s*\(|\bassert\s+|\bself\.assert[A-Z]\w*\s*\(|\bXCTAssert\w*\s*\(")
TEST = re.compile(r"\b(?:TEST_CASE|TEST_CASE_METHOD)\s*\(")
RUST_TEST = re.compile(r"#\[test\][\s\S]{0,240}?\bfn\s+([A-Za-z0-9_]+)\s*\(")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require_pin(root, label):
    if not root.is_dir():
        raise ValueError(f"{label} root missing: {root}")
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    if head != PINS[label]:
        raise ValueError(f"{label} pin mismatch: expected {PINS[label]}, got {head}")
    if subprocess.check_output(["git", "status", "--porcelain"], cwd=root, text=True):
        raise ValueError(f"{label} root is dirty: {root}")


def assertions(root):
    result = []
    for path in sorted(root.rglob("*")):
        if not path.is_file() or path.suffix not in {".cpp", ".hpp", ".swift", ".py"}:
            continue
        relative = path.relative_to(root).as_posix()
        if not relative.startswith(ROOTS):
            continue
        text = path.read_text(errors="surrogateescape")
        case = "outside_test_case"
        ordinal = 0
        for line, source in enumerate(text.splitlines(), 1):
            if TEST.search(source):
                case = source.strip()
                ordinal = 0
            if ASSERTION.search(source):
                ordinal += 1
                result.append({"id": f"{relative}:{line}:{ordinal}", "path": relative,
                               "line": line, "ordinal_in_case": ordinal, "case": case,
                               "assertion": source.strip(), "source_sha256": sha(path)})
    if not result:
        raise ValueError(f"no selected assertions under {root}")
    return result


def rust_tests(root):
    found = {}
    for path in sorted((root / "test").rglob("*.rs")):
        text = path.read_text()
        for match in RUST_TEST.finditer(text):
            name = match.group(1)
            if name in found:
                raise ValueError(f"duplicate Rust test function name: {name}")
            end = text.find("#[test]", match.end())
            body = text[match.start(): None if end == -1 else end]
            found[name] = {"path": path.relative_to(root).as_posix(),
                           "uses_public_api": "Database::" in body and ".connect(" in body
                                              and (".query(" in body or ".execute(" in body or ".prepare(" in body)}
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--development-root", type=Path, required=True)
    parser.add_argument("--release-root", type=Path, required=True)
    parser.add_argument("--mapping", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    if args.output_dir.exists():
        raise ValueError(f"output already exists: {args.output_dir}")
    mapping = json.loads(args.mapping.read_text())
    if mapping.get("schema") != 1:
        raise ValueError("unsupported mapping schema")
    sources = {}
    for label, root in (("development", args.development_root), ("release", args.release_root)):
        require_pin(root, label)
        sources[label] = assertions(root)
    rust = rust_tests(Path.cwd())
    ids = {label: {entry["id"]: entry for entry in values} for label, values in sources.items()}
    seen_source, seen_rust, resolved = set(), set(), []
    for entry in mapping.get("entries", []):
        rust_test = entry["rust_test"]
        if set(entry) < {"source_ids", "rust_test", "invariant_category", "public_api_evidence"}:
            raise ValueError("mapping lacks source IDs, category, or public API evidence")
        if set(entry["source_ids"]) != set(PINS):
            raise ValueError("mapping must name exactly one source assertion for each pin")
        for label, source_id in entry["source_ids"].items():
            key = (label, source_id)
            if source_id not in ids[label]:
                raise ValueError(f"unknown mapped source assertion: {key}")
            if key in seen_source:
                raise ValueError(f"duplicate source mapping: {key}")
            seen_source.add(key)
        if rust_test not in rust:
            raise ValueError(f"unknown Rust public-contract test: {rust_test}")
        if rust_test in seen_rust:
            raise ValueError(f"duplicate Rust test mapping: {rust_test}")
        if not rust[rust_test]["uses_public_api"]:
            raise ValueError(f"Rust test does not demonstrate public Database/Connection API: {rust_test}")
        seen_rust.add(rust_test)
        resolved.append({**entry, "rust_path": rust[rust_test]["path"]})
    if mapping.get("expected_mapped") != len(resolved):
        raise ValueError("mapped count differs from manifest")
    unmapped = {label: len(values) - sum(pin == label for pin, _ in seen_source)
                for label, values in sources.items()}
    if mapping.get("expected_unmapped") != unmapped:
        raise ValueError(f"unmapped count differs from manifest: {unmapped}")
    args.output_dir.mkdir(parents=True)
    report = {"schema": 1, "pins": PINS, "selection_roots": ROOTS,
              "selected_assertion_counts": {key: len(value) for key, value in sources.items()},
              "mapped_count": len(resolved), "unmapped_counts": unmapped,
              "mappings": resolved, "status": "inventoried_not_adapter_validated",
              "gate_p": {"status": "open", "reason": "no comparable C++ inventory operation; no timing claimed"}}
    (args.output_dir / "api-contract-inventory.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"mapped": len(resolved), "unmapped": unmapped, "report": str(args.output_dir / "api-contract-inventory.json")}, sort_keys=True))


if __name__ == "__main__":
    main()
