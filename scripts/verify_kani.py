"""Run the pinned, complete Kani suite at a rewrite stage boundary."""

from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
COMMAND = [
    "cargo", "kani", "-p", "duckdb-rust", "--lib", "--no-default-features",
    "--output-format", "terse", "-Z", "unstable-options", "--harness-timeout", "5m",
]
SUMMARY = re.compile(
    r"^Complete - (\d+) successfully verified harnesses, (\d+) failures, (\d+) total\.$",
    re.MULTILINE,
)


def verification_status(returncode, output):
    """Require both a successful process and a complete, nonempty proof result."""
    if returncode:
        return returncode if returncode > 0 else 1
    summaries = SUMMARY.findall(output)
    if len(summaries) != 1:
        return 1
    passed, failed, total = map(int, summaries[0])
    return 0 if passed == total and total > 0 and failed == 0 else 1


def main():
    version = (ROOT / ".kani-version").read_text().strip()
    try:
        installed = subprocess.run(
            ["cargo", "kani", "--version"], cwd=ROOT, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        if installed.returncode or installed.stdout.strip() != f"cargo-kani {version}":
            print(
                f"Kani stage validation requires version {version}. Run:\n"
                f"cargo install --locked kani-verifier --version {version}\n"
                "cargo kani setup", file=sys.stderr,
            )
            return 1
        print("Stage validation: " + " ".join(COMMAND), flush=True)
        output = []
        with subprocess.Popen(
            COMMAND, cwd=ROOT, text=True, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, bufsize=1,
        ) as process:
            for line in process.stdout:
                print(line, end="", flush=True)
                output.append(line)
            status = verification_status(process.wait(), "".join(output))
        if status:
            print("Kani stage gate is incomplete; inspect the output above.", file=sys.stderr)
        return status
    except OSError as error:
        print(f"Kani stage validation could not run: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
