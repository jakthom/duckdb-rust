"""Generate the pinned C++ FUNCTION-default checkpoint regression fixture."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import tempfile

from default_interoperability_reference import CPP_SETUP
from reference_version import TARGETS, require_reference
from verify_reference import Engine, command


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, default="development")
    parser.add_argument("--duckdb", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    reference, identity = require_reference(args.duckdb, target=args.target)
    destination = args.output_dir.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    fixture = destination / "default_function.duckdb.gz"
    manifest = destination / "manifest.json"
    if fixture.exists() or manifest.exists():
        raise FileExistsError("refusing to overwrite retained default fixture")
    with tempfile.TemporaryDirectory(prefix="duckdb-default-fixture-") as temp:
        path = Path(temp) / "default_function.duckdb"
        command(
            Engine(
                reference,
                False,
                serialize_json_rows=TARGETS[args.target].serialize_json_rows,
            ),
            path,
            CPP_SETUP,
        )
        checkpoint = path.read_bytes()
    fixture.write_bytes(gzip.compress(checkpoint, mtime=0))
    manifest.write_text(
        json.dumps(
            {
                "writer": identity,
                "sql": CPP_SETUP,
                "checkpoint_bytes": len(checkpoint),
                "checkpoint_sha256": hashlib.sha256(checkpoint).hexdigest(),
                "fixture_sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(),
            },
            indent=2,
        )
        + "\n"
    )
    print(f"generated {fixture} from {identity['version']}")


if __name__ == "__main__":
    main()
