# Test execution and reproduction runbook

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

These are source-derived recipes, not commands executed during this specification. Run them from `ddb/duckdb` (the sibling C++ repository root), not from `duckdb-rust/specs`, after installing the required compiler/build tooling. Use binaries and extensions from the intended build. A comprehensive verification campaign is a matrix, not one command.

## Build and ordinary correctness

```bash
# Optimized build with debug information, recommended by repository guidance.
make reldebug

# Fast/default selection through the batching wrapper.
build/reldebug/test/run

# One SQL file, directly through the native interpreter/Catch runner.
build/reldebug/test/unittest test/sql/order/test_limit.test

# Include hidden/slow tests, subject to requirements and configuration.
build/reldebug/test/run '*'

# Default release all-unit recipe (builds outside CI unless configured otherwise).
make allunit

# A source-controlled smoke-test list through the configured runner.
make smoke
```

Native `unittest` flags and wrapper flags are distinct. Pass engine-test options through wrapper `--test-flags` when the wrapper does not expose them directly.

## Configuration and representation matrix

```bash
# These Make targets use build/release/test/run; build release first.
make release
make test_configs
make test_vector
make test_table_scan

# One independent configuration on a chosen build.
build/reldebug/test/run --test-config test/configs/force_storage_restart.json

# Low-concurrency reproducer with native flags.
build/reldebug/test/run --workers 1 --batch-size 1 --retry 0 \
  --test-flags='--force-storage' 'test/sql/storage/*'
```

Configuration runs may need additional extensions/fixtures. Missing `require` conditions should be reviewed; use `--fail-require-skip` when the selected run is expected to satisfy all such requirements.

## Shell and runner contracts

```bash
python3 -m pytest tools/shell/tests --shell-binary build/reldebug/duckdb
python3 -m pytest tools/sqllogic/tests --unittest-binary build/reldebug/test/unittest
DUCKDB_UNITTEST_BINARY=build/reldebug/test/unittest \
  python3 -m pytest test/py/test_temp_dir_contract.py

make test_ci
python3 -m unittest scripts.test_package_build_version
python3 -m unittest scripts.regression.test_comparison scripts.regression.test_local_extensions
```

These require Python dependencies appropriate to the suite. ADBC additionally needs `DUCKDB_INSTALL_LIB` set to the intended library; Swift requires package preparation and the Xcode destination from its workflow.

## Benchmark and measurement entry points

```bash
BUILD_BENCHMARK=1 BUILD_TPCH=1 make release
build/release/benchmark/benchmark_runner --list
build/release/benchmark/benchmark_runner \
  benchmark/micro/nulls/no_nulls_addition.benchmark --timed-runs 5

# Use an existing shell built with extensions required by the workloads.
python3 scripts/test_benchmark_sql_runner.py --shell build/reldebug/duckdb
python3 test/io_metrics/run_io_metrics_test.py --duckdb build/reldebug/duckdb
```

Long-running memory, sanitizer, fuzzing and compatibility campaigns have extra environment/binary prerequisites described above. The memory-growth supervisor also has broad process cleanup behavior, so its README invocation belongs in a dedicated environment. Storage/plan compatibility can download or build historical binaries and extensions; those are substantial operations, not prerequisites for reading this document.

## Capturing a reproducible failure

Retain the source hash, build configuration, extension configuration/versions, native command or wrapper reproducer, selected config files, worker/thread counts, seeds, fixture paths and the failure/skip summaries. Preserve failed-test scratch state using the runner's directory/database disposition options where needed. Reproduce with the same batch when process contamination is suspected, then narrow to one test to separate local failure from order dependence.
