# Benchmark execution and regression measurement

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Native and interpreted benchmark runner

The tree contains 1,160 tracked `.benchmark` files. [BenchmarkRunner](../../../duckdb/benchmark/benchmark_runner.cpp) registers both compiled benchmarks and interpreted workload files. [Benchmark](../../../duckdb/benchmark/include/benchmark.hpp) defines `Initialize`, `Assert`, `Run`, `Cleanup`, `Verify`, `Finalize`, `Interrupt`, query/display metadata and optional reinitialization/timeouts.

The interpreted runner parses setup/load statements, the measured query, expected results and supported metadata/options. The implementation in [interpreted_benchmark.cpp](../../../duckdb/benchmark/interpreted_benchmark.cpp) is the authoritative format. Benchmark data is normally maintained under `duckdb_benchmark_data`; some workloads need downloaded or generated datasets.

The documented default is one warmup followed by five timed runs, with `--timed-runs` override. It supports listing, regex selection, query/info output, profiling, timing output and timeout controls. Workload verification is a separate operation from measuring elapsed time; a faster wrong result must still fail verification where a workload supplies that oracle.

Workload families include microbenchmarks, TPCH/TPCDS, joins, external formats, IMDB/LDBC, recursive CTEs and other analytical/query suites. They are inputs to the common runner, not separate execution engines.

Sources: [benchmark README](../../../duckdb/benchmark/README.md), [benchmark configuration](../../../duckdb/benchmark/include/benchmark_configuration.hpp), [benchmark CMake](../../../duckdb/benchmark/CMakeLists.txt).

## SQL benchmark smoke runner

[test_benchmark_sql_runner.py](../../../duckdb/scripts/test_benchmark_sql_runner.py) operates on benchmark directories with `init/` and `queries/` SQL. It starts CLI subprocesses against a per-workload database, runs setup and queries, and fails on unsuccessful subprocesses. `make test_benchmark_sql` points it at a `relassert` shell.

This is a crash/error smoke test for those SQL workloads. It does not implement the full native benchmark timing/regression protocol or provide an expected-result oracle for every successful statement.

## Baseline/current performance comparisons

| Tool | Measurement boundary |
| --- | --- |
| [scripts/regression/test_runner.py](../../../duckdb/scripts/regression/test_runner.py) and [comparison.py](../../../duckdb/scripts/regression/comparison.py) | Benchmark baseline/current sampling, medians/ranges, confirmation behavior, aggregate thresholds and diagnostics |
| [scripts/regression/benchmark.py](../../../duckdb/scripts/regression/benchmark.py) | Benchmark invocation and artifact/workload integration |
| [regression_check.py](../../../duckdb/scripts/regression_check.py) | Earlier comparison of timing files using configured relative/absolute thresholds |
| [regression_test_storage_size.py](../../../duckdb/scripts/regression_test_storage_size.py) | Storage-size comparison using old/new engines |
| [regression_test_extension_size.py](../../../duckdb/scripts/regression_test_extension_size.py) | Extension binary-size comparisons |
| [plan_cost_runner.py](../../../duckdb/scripts/plan_cost_runner.py) | Plan-cost measurement/comparison inputs |
| [run_benchmark.py](../../../duckdb/scripts/run_benchmark.py) | CLI-driven workload timing utility |
| [regression_test_python.py](../../../duckdb/scripts/regression_test_python.py) | Python-client performance utility with external/client-build prerequisites |

Regression thresholds and adaptive sampling live in the corresponding tool and workflow. Different tools do not share one universal pass/fail threshold. [Regression.yml](../../../duckdb/.github/workflows/Regression.yml) separates benchmark groups, storage, binary-size and plan-cost jobs; [ExtendedTests.yml](../../../duckdb/.github/workflows/ExtendedTests.yml) adds compiler/LTO comparisons.

Artifacts and caches affect measurements. Regression tooling explicitly resolves artifact-local extensions and manages shared/writable benchmark state. A meaningful report records engine hashes, compiler/options, extension versions, dataset/cache state, memory/thread settings, repetitions and measurement variability.

## Benchmark state machine and measurement boundary

`Initialize` creates owned benchmark state; `Assert` checks post-load/pre-run conditions; `Run` performs the workload; `Verify` checks its result; `Cleanup` releases per-run resources. `Finalize` and `Interrupt` cover end-of-runner and timeout responsibilities. Reinitialization policy determines whether data/state survives repetitions. The measured interval must be interpreted using the runner implementation rather than assuming setup, verification, and cleanup are all timed identically.

Warmup can populate caches and compiled/prepared state. Repeated timed runs therefore measure a specified warm/cold policy, not a universal property of the SQL string. Workloads requiring generated/downloaded data can spend substantial time outside the timed query. A test that lacks a strong result oracle can still provide a crash/error signal, but its timing alone cannot prove correctness.

## Baseline/current comparison protocol

Keep hardware, compiler/build flags, datasets, extension artifacts, memory/thread settings, and cache policy comparable. The regression tools use configured sampling/threshold/confirmation logic; report the samples and the actual threshold decision instead of inventing a common percentage across tools. Storage size, extension binary size, plan cost, and elapsed query time have different units and failure interpretations.

Plan changes can explain a timing change independently of the implementation of an operator. Preserve query plans/profiles when investigating optimizer regressions. Conversely, unchanged plan text does not prove unchanged execution because vector representation, memory policy, codec, or I/O behavior can change underneath it.

## Verification of measurement infrastructure

Regression comparison and local-extension-resolution self-tests validate parsing, threshold decisions, and artifact selection. Smoke-runner success validates that selected setup/query subprocesses complete; it does not run the native benchmark's full verification contract. Test benchmark infrastructure with controlled small inputs before attributing a large campaign difference to the database engine.

Artifacts should include workload revision, producer commands, engine hashes, timings, profiles, errors, and dataset/cache provenance. Keep performance findings separate from the correctness test summary. This specification does not contain new measurements or claim that any benchmark campaign was run.
