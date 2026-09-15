"""Local path dependencies are source inputs, not registry checksum identities."""
from pathlib import Path


def vendored_sources(root):
    root = Path(root)
    third_party = root / "third_party"
    utf8proc = root / "vendor" / "utf8proc-sys"
    return sorted(
        [*third_party.rglob("*.rs"), *third_party.rglob("Cargo.toml")]
        + [
            utf8proc / "Cargo.toml",
            utf8proc / "build.rs",
            utf8proc / "src" / "lib.rs",
            utf8proc / "src" / "generated.rs",
            utf8proc / "utf8proc" / "utf8proc.c",
            utf8proc / "utf8proc" / "utf8proc.h",
            utf8proc / "utf8proc" / "utf8proc_data.c",
            utf8proc / "PROVENANCE.md",
        ]
    )
