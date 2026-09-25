"""Generate a pinned C++ checkpoint with retained nested literal defaults."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import tempfile

from reference_version import TARGETS, require_reference
from verify_reference import Engine, command


SETUP = """
CREATE TABLE reference_list_defaults(
    id INTEGER PRIMARY KEY,
    list_default INTEGER[] DEFAULT [3,NULL,1],
    array_cast_default INTEGER[] DEFAULT [3,NULL,1]::INTEGER[3],
    sorted_default INTEGER[] DEFAULT list_sort([3,NULL,1],'ASC','NULLS LAST'),
    array_sorted_default INTEGER[] DEFAULT array_sort([3,NULL,1]::INTEGER[3],'DESC','NULLS FIRST')
);
INSERT INTO reference_list_defaults(id) VALUES (1);
CHECKPOINT;
"""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--duckdb", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    reference, identity = require_reference(args.duckdb, target=args.target)
    destination = args.output_dir.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    fixture = destination / "unbound_default.duckdb.gz"
    manifest = destination / "manifest.json"
    if fixture.exists() or manifest.exists():
        raise FileExistsError("refusing to overwrite retained default fixture")
    with tempfile.TemporaryDirectory(prefix="duckdb-unbound-default-") as temporary:
        path = Path(temporary) / "unbound_default.duckdb"
        command(
            Engine(
                reference,
                False,
                serialize_json_rows=TARGETS[args.target].serialize_json_rows,
            ),
            Path(":memory:"),
            f"ATTACH '{path}' AS fixture (STORAGE_VERSION 'v1.5.0'); USE fixture; {SETUP}",
        )
        checkpoint = path.read_bytes()
    compressed = gzip.compress(checkpoint, mtime=0)
    fixture.write_bytes(compressed)
    manifest.write_text(
        json.dumps(
            {
                "writer": identity,
                "storage_version": "v1.5.0",
                "sql": SETUP,
                "checkpoint_bytes": len(checkpoint),
                "checkpoint_sha256": digest(checkpoint),
                "fixture_sha256": digest(compressed),
            },
            indent=2,
        )
        + "\n"
    )
    print(f"generated {fixture} from {identity['version']}")


if __name__ == "__main__":
    main()
