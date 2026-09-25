"""Required release identity for independent SQL and file compatibility checks."""
import hashlib
from dataclasses import dataclass
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


@dataclass(frozen=True)
class ReferenceTarget:
    version: str
    revision: str
    source: Path
    build: Path
    serialize_json_rows: bool

    @property
    def binary(self):
        return self.build / ("duckdb.exe" if sys.platform == "win32" else "duckdb")


TARGETS = {
    "release": ReferenceTarget(
        "1.5.5", "d8cdaa33fda8df955cc76ef58a280f68f4cd43fa",
        ROOT.parent / "duckdb-v1.5.5",
        ROOT.parent / "duckdb-v1.5.5/build/rewrite-reference", True,
    ),
    "development": ReferenceTarget(
        "2.0.0-dev84019", "99063af2bd7092aff02e14184a20e24699d34d71",
        ROOT.parent / "duckdb", ROOT.parent / "duckdb/build/engine-walkthrough", False,
    ),
}


def require_checkout(source, target):
    expected = TARGETS[target]
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
    if revision != expected.revision or subprocess.check_output(["git", "diff", "HEAD", "--"], cwd=source):
        raise ValueError(f"{target} reference must be the unchanged checkout {expected.revision}")
    return revision


def require_reference(binary=None, *, target="release"):
    """Resolve once, reject other releases before effects, and record provenance."""
    expected = TARGETS[target]
    binary = expected.binary if binary is None else binary
    executable = shutil.which(str(binary))
    if executable is None:
        raise FileNotFoundError(f"DuckDB v{expected.version} executable not found: {binary}")
    path = Path(executable).resolve(strict=True)
    version = subprocess.check_output([str(path), "--version"], text=True, timeout=10).strip()
    if not re.match(rf"^v{re.escape(expected.version)}(?:\s|$)", version):
        raise ValueError(
            f"{target} checks require DuckDB v{expected.version}; "
            f"{path} reports {version!r}"
        )
    source_id = re.search(r"\b([0-9a-f]{10,40})$", version)
    if source_id is None or not expected.revision.startswith(source_id[1]):
        raise ValueError(f"{target} executable reports a different source revision: {version!r}")
    identity = {
        "target": target,
        "required_version": expected.version,
        "required_revision": expected.revision,
        "reported_source_id": source_id[1],
        "version": version,
        "path": str(path),
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }
    print(f"Reference ({target}): {version}\nExecutable: {path}\nSHA-256: {identity['sha256']}",
          file=sys.stderr, flush=True)
    return path, identity
