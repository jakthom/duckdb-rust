"""Retain and verify the complete pinned DuckDB source, including every test asset."""
import argparse
from collections import Counter
import hashlib
import gzip
import json
from pathlib import Path
import re
import subprocess
import shutil
import tarfile

ROOT = Path(__file__).resolve().parents[1]
DESTINATION = ROOT / "test/upstream/duckdb"
REVISION = "99063af2bd7092aff02e14184a20e24699d34d71"
SQL_SUFFIXES = (".test", ".test_slow", ".test_coverage")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def declarations(path, data):
    """Source declarations are a backlog, not a compiled test registry or a pass."""
    if path.endswith(SQL_SUFFIXES):
        return [{"id": path, "kind": "sqllogictest", "path": path, "line": 1}]
    if path.endswith((".cpp", ".hpp", ".cc")):
        pattern = r"(?m)^\s*(?:TEST_CASE(?:_METHOD)?|TEMPLATE_TEST_CASE(?:_METHOD)?|SCENARIO)\s*\("
        kind = "native_declaration"
    elif path.endswith(".py"):
        pattern = r"(?m)^\s*(?:async\s+)?def\s+test_[A-Za-z0-9_]+\s*\("
        kind = "python_declaration"
    elif path.endswith(".swift"):
        pattern = r"(?m)^\s*func\s+test[A-Za-z0-9_]+\s*\("
        kind = "swift_declaration"
    elif path.endswith(".benchmark"):
        return [{"id": path, "kind": "benchmark", "path": path, "line": 1}]
    else:
        return []
    text = data.decode("utf-8", errors="replace")
    result = []
    for match in re.finditer(pattern, text):
        line = text.count("\n", 0, match.start()) + 1
        result.append({"id": f"{path}:{line}", "kind": kind, "path": path, "line": line})
    return result


def capture(checkout, destination):
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=checkout, text=True).strip()
    if revision != REVISION:
        raise ValueError(f"expected pinned C++ revision {REVISION}, got {revision}")
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination / "source.tar.gz"
    if archive.exists():
        raise FileExistsError(f"refusing to overwrite pinned archive {archive}")
    with archive.open("xb") as output, gzip.GzipFile(fileobj=output, mode="wb", mtime=0, compresslevel=9) as compressed:
        process = subprocess.Popen(["git", "archive", "--format=tar", revision], cwd=checkout, stdout=subprocess.PIPE)
        with process.stdout:
            shutil.copyfileobj(process.stdout, compressed)
        if process.wait():
            raise RuntimeError("git archive failed")
    files, tests = [], []
    with tarfile.open(archive) as contents:
        for member in contents:
            if member.isdir():
                continue
            if not (member.isfile() or member.issym()):
                raise ValueError(f"unsupported source archive entry {member.name}")
            data = member.linkname.encode() if member.issym() else contents.extractfile(member).read()
            files.append({"path": member.name, "kind": "symlink" if member.issym() else "file", "mode": member.mode, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
            if member.isfile():
                tests.extend(declarations(member.name, data))
    manifest = {"repository": "https://github.com/duckdb/duckdb", "revision": revision,
                "archive": archive.name, "archive_sha256": digest(archive), "files": files,
                "tests": tests, "counts": dict(Counter(t["kind"] for t in tests)),
                "scope": "Every tracked file at the pinned revision is retained without rewriting. SQL files are exact runnable inputs; native/client declarations are unported obligations. Source discovery does not enumerate compiled parameterizations, external repositories, platform/configuration instances or generated tests, and does not establish passing parity."}
    (destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"files": len(files), "archive_bytes": archive.stat().st_size, "tests": manifest["counts"]}))


def verify(destination, extract=None):
    manifest = json.loads((destination / "manifest.json").read_text())
    if manifest["revision"] != REVISION:
        raise ValueError("unexpected upstream revision")
    archive = destination / manifest["archive"]
    if digest(archive) != manifest["archive_sha256"]:
        raise ValueError("upstream archive hash differs")
    expected = {entry["path"]: entry for entry in manifest["files"]}
    if len(expected) != len(manifest["files"]):
        raise ValueError("duplicate source paths")
    seen, tests, links = set(), [], []
    with tarfile.open(archive) as contents:
        for member in contents:
            if member.isdir():
                continue
            path = Path(member.name)
            if not (member.isfile() or member.issym()) or path.is_absolute() or ".." in path.parts or member.name in seen:
                raise ValueError(f"unsafe or duplicate archive entry {member.name}")
            seen.add(member.name)
            data = member.linkname.encode() if member.issym() else contents.extractfile(member).read()
            actual = {"path": member.name, "kind": "symlink" if member.issym() else "file", "mode": member.mode, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
            if expected.get(member.name) != actual:
                raise ValueError(f"upstream content differs: {member.name}")
            if member.isfile():
                tests.extend(declarations(member.name, data))
            if extract is not None:
                target = extract / path
                # Extract only into a newly created, owned directory.
                target.parent.mkdir(parents=True, exist_ok=True)
                if member.issym():
                    if not (target.parent / member.linkname).resolve().is_relative_to(extract.resolve()):
                        raise ValueError(f"symlink escapes source root: {member.name}")
                    links.append((target, member.linkname))
                else:
                    target.write_bytes(data)
                    target.chmod(member.mode & 0o777)
    if (seen != expected.keys() or tests != manifest["tests"]
            or dict(Counter(test["kind"] for test in tests)) != manifest["counts"]):
        raise ValueError("incomplete source or test inventory")
    for target, link in links:
        target.symlink_to(link)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture", type=Path)
    parser.add_argument("--destination", type=Path, default=DESTINATION)
    parser.add_argument("--extract", type=Path)
    args = parser.parse_args()
    if args.capture:
        capture(args.capture, args.destination)
    else:
        if args.extract:
            args.extract.mkdir(parents=True, exist_ok=False)
        manifest = verify(args.destination, args.extract)
        print(f"Verified {len(manifest['files'])} source assets; {manifest['counts']}")


if __name__ == "__main__":
    main()
