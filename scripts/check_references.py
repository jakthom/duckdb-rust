"""Run separate compatibility and performance gates for both pinned C++ builds.

Build the references using docs/reference-builds.md before running this script.
No compilation or other benchmark campaigns should run concurrently with it.
Failures are retained and do not prevent the other target's checks from running.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import platform
import subprocess
import sys

from reference_version import ROOT, TARGETS, require_checkout, require_reference
from upstream_suite import digest


def build_identity(target):
    configuration = TARGETS[target]
    revision = require_checkout(configuration.source, target)
    binary, identity = require_reference(target=target)
    cache = configuration.build / "CMakeCache.txt"
    text = cache.read_text()
    if "CMAKE_BUILD_TYPE:STRING=Release\n" not in text:
        raise ValueError(f"{target} reference must use a release build")
    settings = {}
    for line in text.splitlines():
        if not line or line.startswith(("#", "//")) or "=" not in line:
            continue
        key, value = line.split("=", 1)
        name = key.split(":", 1)[0]
        if name.startswith(("CMAKE_CXX", "CMAKE_C_", "CMAKE_OSX", "BUILD_", "ENABLE_", "DISABLE_", "DUCKDB_", "EXTENSION_", "FORCE_", "NATIVE_ARCH")):
            settings[key] = value
    library = configuration.build / "src" / ("libduckdb.dylib" if platform.system() == "Darwin" else "libduckdb.so")
    identity.update({
        "source": str(configuration.source), "source_revision": revision,
        "build": str(configuration.build), "cmake_cache_sha256": digest(cache),
        "settings": settings, "library_sha256": digest(library),
        "extensions": json.loads(subprocess.check_output([
            str(binary), ":memory:", "-json", "-c",
            "SELECT extension_name, loaded, installed FROM duckdb_extensions() ORDER BY extension_name",
        ], text=True)),
    })
    for name in ["build.ninja", "compile_commands.json", "test/unittest"]:
        path = configuration.build / name
        identity[name] = {"sha256": digest(path)} if path.is_file() else {"available": False}
    return identity


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=21)
    args = parser.parse_args()
    if args.iterations < 9 or args.iterations % 2 == 0:
        raise ValueError("use an odd sample count of at least nine")
    destination = args.output_dir.resolve()
    destination.mkdir(parents=True, exist_ok=False)
    report = {
        "recorded_at": datetime.now(timezone.utc).isoformat(), "targets": {}, "steps": [],
        "complete_test_parity": False, "complete_performance_parity": False,
        "complete_compatibility_parity": False,
        "scope": "Two pinned C++ references; the existing supported-subset file/SQL campaigns and targeted performance workloads. Native/client mappings, full SQL/configuration matrices and all other performance workloads remain separate obligations.",
    }
    summary = destination / "summary.json"

    def save():
        summary.write_text(json.dumps(report, indent=2) + "\n")

    for target in TARGETS:
        try:
            report["targets"][target] = build_identity(target)
        except Exception as error:
            report["targets"][target] = {"error": str(error)}
            save()
            continue
        for name, script in [("compatibility", "verify_reference.py"),
                             ("alter-sql", "alter_reference.py"),
                             ("performance", "compare_native.py")]:
            output = destination / f"{target}-{name}.json"
            log = destination / f"{target}-{name}.log"
            command = [sys.executable, str(ROOT / "scripts" / script), "--target", target,
                       "--report", str(output)]
            if name == "performance":
                command += ["--iterations", str(args.iterations)]
            print(f"Running {target} {name}", flush=True)
            with log.open("w") as stream:
                completed = subprocess.run(command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT)
            step = {"target": target, "name": name, "command": command,
                    "exit_code": completed.returncode, "log": log.name, "log_sha256": digest(log),
                    "passed": False}
            if output.is_file():
                result = json.loads(output.read_text())
                step.update({"report": output.name, "report_sha256": digest(output),
                             "passed": completed.returncode == 0 and
                             (result.get("passed") is True or result.get("result") == "passed")})
            report["steps"].append(step)
            save()
            print(f"{target} {name}: {'passed' if step['passed'] else 'FAILED'}", flush=True)
    report["passed"] = (len(report["steps"]) == 3 * len(TARGETS)
                        and all(step["passed"] for step in report["steps"]))
    save()
    print(json.dumps({"passed": report["passed"], "summary": str(summary)}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
