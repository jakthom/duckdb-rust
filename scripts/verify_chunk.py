#!/usr/bin/env python3
"""Run the complete chunk verification sweep in progressive, fail-fast stages."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
EXHAUSTIVE_RECOVERY_TEST = (
    "every_framed_tail_truncation_exposes_only_complete_transactions"
)


@dataclass(frozen=True)
class Stage:
    name: str
    description: str
    command: tuple[str, ...]


STAGES = (
    Stage(
        "format",
        "Rust formatting",
        ("cargo", "fmt", "--all", "--", "--check"),
    ),
    Stage(
        "check",
        "workspace and all-target type checking",
        ("cargo", "check", "--workspace", "--all-targets"),
    ),
    Stage(
        "clippy",
        "workspace and all-target linting",
        (
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ),
    ),
    Stage(
        "tests",
        "full test suite except the exhaustive recovery truncation sweep",
        (
            "cargo",
            "test",
            "--workspace",
            "--all-targets",
            "--",
            "--skip",
            EXHAUSTIVE_RECOVERY_TEST,
        ),
    ),
    Stage(
        "recovery",
        "exhaustive recovery truncation sweep",
        (
            "cargo",
            "test",
            "--test",
            "recovery",
            EXHAUSTIVE_RECOVERY_TEST,
            "--",
            "--exact",
            "--nocapture",
        ),
    ),
    Stage(
        "kani",
        "maintained exploratory Kani checkpoint",
        (sys.executable, "scripts/verify_kani.py"),
    ),
)


def display_command(command: tuple[str, ...]) -> str:
    return " ".join(command)


def list_stages() -> None:
    for number, stage in enumerate(STAGES, 1):
        print(f"{number}. {stage.name}: {stage.description}")
        print(f"   {display_command(stage.command)}")


def run() -> int:
    total_started = time.monotonic()
    print(f"Chunk verification: {len(STAGES)} progressive stages", flush=True)
    for number, stage in enumerate(STAGES, 1):
        started = time.monotonic()
        print(
            f"\n[{number}/{len(STAGES)}] START {stage.name}: {stage.description}\n"
            f"$ {display_command(stage.command)}",
            flush=True,
        )
        try:
            result = subprocess.run(stage.command, cwd=ROOT)
        except OSError as error:
            elapsed = time.monotonic() - started
            print(
                f"[{number}/{len(STAGES)}] ERROR {stage.name} "
                f"after {elapsed:.1f}s: {error}",
                file=sys.stderr,
                flush=True,
            )
            return 1
        elapsed = time.monotonic() - started
        if result.returncode:
            print(
                f"[{number}/{len(STAGES)}] FAIL {stage.name} "
                f"after {elapsed:.1f}s (exit {result.returncode})",
                file=sys.stderr,
                flush=True,
            )
            if stage.name == "kani":
                print(
                    "Kani was run but was unsuccessful or incomplete. Per "
                    "specs/testing/kani.md, report the limitation; this status "
                    "does not by itself reject an exploratory chunk.",
                    file=sys.stderr,
                    flush=True,
                )
            return result.returncode if result.returncode > 0 else 1
        print(
            f"[{number}/{len(STAGES)}] PASS {stage.name} ({elapsed:.1f}s)",
            flush=True,
        )

    elapsed = time.monotonic() - total_started
    print(f"\nChunk verification PASS ({elapsed:.1f}s total)", flush=True)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list",
        action="store_true",
        help="list the progressive stages without running them",
    )
    arguments = parser.parse_args()
    if arguments.list:
        list_stages()
        return 0
    return run()


if __name__ == "__main__":
    sys.exit(main())
