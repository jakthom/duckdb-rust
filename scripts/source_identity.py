"""Local path dependencies are source inputs, not registry checksum identities."""
from pathlib import Path


def vendored_sources(root):
    directory = Path(root) / "third_party"
    return sorted([*directory.rglob("*.rs"), *directory.rglob("Cargo.toml")])
