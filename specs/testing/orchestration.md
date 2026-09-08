# Parallel test process orchestration

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Python orchestrator protocol

Generated `build/<config>/test/run` wrappers resolve a sibling `unittest` executable, establish the source root, and invoke [run_tests.py](../../../duckdb/scripts/ci/run_tests.py). Windows gets `run.py` and `run.bat`. The wrapper is a process scheduler and reporter; it does not independently interpret SQLLogicTest SQL.

| Input/option | Behavior |
| --- | --- |
| Positional patterns | Select native registry names/tags or SQL paths |
| `--test-list` | Explicit list of tests |
| `--changed-tests` | Additional changed-test list, requiring `--test-list` |
| `--workers` | Integer/percentage worker selection; default is 75% |
| `--batch-size` | Tests grouped into a child process; default 10 |
| `--batch-timeout` | Process/batch deadline; defaults depend on worker count |
| `--test-flags` | Flags passed to native listing/execution |
| Repeated `--test-config` | Run each configuration independently |
| `--retry`, `--max-retries` | Retry policy; retries default to zero |
| `--fail-fast`, `--max-failures` | Stop-launching/failure limits |
| `--fail-require-skip` | Treat reported missing requirements as failure |
| `--track-runtime`, `--track-rss-memory` | Report slow or high-memory batches |
| `--stabilize-tests` | Repeat selected tests under the fast/slow stabilization policy |
| `--coverage-report` | Collect LLVM coverage data and generate an LCOV HTML report |
| `--test-command` | Custom command template using `{binary}`, `{flags}`, `{test_list}`; used for tools such as Valgrind |

The default batch timeout is 600 seconds, reduced to 300 seconds above the configured high-worker threshold of 10. These are wrapper defaults, distinct from per-query `max_execution_time`. The orchestrator collects child output, identifies failures and unfinished tests, handles subprocess timeouts, and prints reproducer commands. A retry success is not equivalent to proving that a flaky failure cannot recur.

## Isolation granularity

There are three relevant parallelism levels: Python worker processes; concurrent tests/loops within a native process where explicitly requested; and DuckDB worker threads within each query. Multiplying those levels can increase memory and CPU demand substantially. Configuration sweeps and parallel execution therefore need deliberate worker/thread settings.

File-list batching means several tests share a process lifetime even though the SQL harness gives them individual database/temporary state. Process-global leakage and cross-test contamination can require a one-test batch to isolate or a retained batch order to reproduce.

## Internal component map

`TestRunnerConfig` carries execution policy, while `TestCase`, `BatchRunState`, and failure/attempt records track selection and progress. Listing functions query the native executable; batch construction creates child file lists/commands. `run_batch` owns process execution and collection. Output parsers recover assertion locations, SQL diagnostics, fatal signals, unfinished tests, runtime data, and requirement skips. Rendering/reproducer helpers convert those records into user-facing diagnostics.

Failure attribution is necessarily weaker for a hard crash than for an explicit assertion: the orchestrator may infer the active test from the last reported progress marker. Preserve raw stdout/stderr and the original batch when that inference is ambiguous. A timeout applies to the process/batch, not individually to every SQL statement in it.

Source: [run_tests.py](../../../duckdb/scripts/ci/run_tests.py).

## Retry, stop, and reporting semantics

Retry policy records failed attempts and can narrow a retry target using parsed diagnostics. A recovered test should still be reported as having failed before retry; treating it as an ordinary clean pass loses evidence of nondeterminism. Fail-fast and maximum-failure settings govern scheduling decisions, while already running work requires its own completion/termination handling.

Repeated configuration arguments produce separate execution populations. Changed-test and explicit-list modes need the required list inputs; they are not an automatic proof that unselected code is unaffected. Stabilization deliberately repeats selected cases and treats fast/slow workloads differently to control cost.

Coverage mode collects instrumented profiles and runs external coverage tooling over the selected binaries/objects. Instrumentation, loaded extension objects, exclusion patterns, and selected tests define the resulting coverage denominator. It is not equivalent to merely counting `.test` files.

## Harness acceptance criteria

The orchestrator's Python tests should exercise output parsing, crash/timeout attribution, require-skip policy, batch splitting, retries, quoting/reproducer commands, and stop behavior without needing a complete engine campaign. Then run small end-to-end batches for clean success, assertion failure, missing requirements, and process termination. Use source-root-relative paths and an explicitly selected local binary; the [runbook](runbook.md) documents the wrapper/native flag boundary.
