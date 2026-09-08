# Shell, Swift, and harness contract tests

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Shell pytest

There are 32 shell test modules at the baseline. [conftest.py](../../../duckdb/tools/shell/tests/conftest.py) provides `ShellTest`, `TestResult`, temporary fixtures and extension checks. `--shell-binary` selects the process under test. Tests build command lines and stdin scripts and compare stdout, stderr and status, with platform handling where needed.

Coverage includes CLI arguments, open/import/export/dump, stdin and history, metadata commands, output formats and duckbox streaming, Unicode/large values, errors, safe mode, environment interaction, prompt/highlighting/autocomplete, pager behavior, profiling/logging, temporary directories and transaction behavior of meta commands. Extension-dependent fixtures can skip when the extension is unavailable.

This verifies the shell's process/UI contract in addition to engine SQL semantics. SQLLogicTest cannot by itself assert terminal rendering, command-line argument handling, or stdout/stderr behavior.

## Native stdin and temporary-directory tests

[tools/sqllogic/tests/test_unittest_stdin.py](../../../duckdb/tools/sqllogic/tests/test_unittest_stdin.py) invokes the native runner with `--stdin`. Its fixture requires `--unittest-binary`; it is not a Python implementation of the database or the main SQL interpreter.

[test_temp_dir_contract.py](../../../duckdb/test/py/test_temp_dir_contract.py) launches a native runner and inspects state after it exits. It verifies cleanup, preservation of preexisting/adopted paths, startup failures, relative/absolute paths, reserved environment variables and related isolation contracts. It accepts `DUCKDB_UNITTEST_BINARY` or discovers a supported build under `DUCKDB_ROOT`. These lifecycle checks observe behavior that an in-process SQL test cannot see after its own runner has terminated.

## Tag validator

[validate_tags_usage.sh](../../../duckdb/test/sqlite/validate_tags_usage.sh) runs special [tag fixtures](../../../duckdb/test/sqlite/tags/) and checks selected emitted SQL for individual tags and AND tag sets. It defaults to the debug runner and can be configured with its environment variables. Its printed checks should be read as diagnostics: the script's shell `expect` logic does not uniformly propagate every mismatch as a nonzero process exit. It is not interchangeable with a strict Catch/pytest failure gate.

## Swift harness

The nine Swift test files cover database, prepared statement, appender, logical type, conversion, Foundation, decimal, coding metadata and extension behavior. [create_package.py](../../../duckdb/tools/swift/create_package.py) prepares the package; [Swift.yml](../../../duckdb/.github/workflows/Swift.yml) runs `xcodebuild test` against the generated workspace/scheme and selected macOS/iOS/tvOS destinations. Simulator coverage is conditional in the workflow.

This is separate from C/C++ tests: language-level conversions and Swift ownership/error behavior can fail even when the C API underneath passes.

## Infrastructure self-tests

`make test_ci` invokes Python `unittest` discovery under [scripts/ci](../../../duckdb/scripts/ci/). Modules cover run orchestration, retry behavior, job-stage selection, version handling, staged-extension checks, release-artifact packaging and exported-symbol checking. [scripts/test_package_build_version.py](../../../duckdb/scripts/test_package_build_version.py) adds package-version tests outside that discovery root.

[scripts/regression/test_comparison.py](../../../duckdb/scripts/regression/test_comparison.py) and [test_local_extensions.py](../../../duckdb/scripts/regression/test_local_extensions.py) exercise regression measurement, sampling/threshold logic, diagnostics, artifact-local extension resolution and command integration. The similarly named `scripts/regression/test_runner.py` is production regression-runner support, not simply a test module inferred from its filename.

These suites validate the reliability of the measurement/automation itself. They are not evidence of database SQL correctness.
