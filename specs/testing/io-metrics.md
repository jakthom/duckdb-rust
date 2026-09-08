# Independent I/O metrics verification

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Independent I/O metrics oracle

[run_io_metrics_test.py](../../../duckdb/test/io_metrics/run_io_metrics_test.py) compiles [io_shim.c](../../../duckdb/test/io_metrics/io_shim.c), interposes selected libc file operations, runs CLI workloads and compares intercepted bytes with `io.total_bytes_read`/`io.total_bytes_written` profiling values. It uses macOS interposition or Linux/glibc `LD_PRELOAD` and requires a C compiler.

Preparation happens outside the measured process where appropriate. Each workload selects path prefixes, optional setup, measured statements and read/write checks. Non-skipped mismatches fail exactly, without tolerance. Entries with `skip` record known-wrong instrumentation; matching skipped entries are reported as stale skips for maintenance.

The [I/O configuration](../../../duckdb/test/configs/io_metrics.json) uses its own schema and must not be supplied to `test/run --test-config`. This harness specifically checks attribution accuracy; a normal SQL result comparison cannot detect missing `QueryContext` propagation to I/O.

## Measurement pipeline and interfaces

The harness locates an explicit shell or searches the release/reldebug/debug build paths, creates a temporary work area, and compiles a platform-specific shared interposition library with `cc`. It uses `DYLD_INSERT_LIBRARIES` on macOS or `LD_PRELOAD` on Linux/glibc. Unsupported platforms or failed shim compilation are infrastructure failures, not engine metric mismatches.

Configuration workloads define a name, included path prefixes, optional exclusions, preparation SQL, measured statements, selected read/write dimensions, optional settings/database, and optional skip reason. `{dir}` expands to the workload scratch directory. Preparation runs in an uninstrumented process so fixture creation does not contaminate measured counts.

The measured process enables JSON profiling and the two byte metrics, disables external-file caching, and uses one engine thread for repeatable comparison. For multiple statements, the driver uses per-statement profiles and output sentinels, reading each profile before the next statement overwrites the relevant output. It sums reported counts and compares with the shim's process-level output for the chosen file paths.

## Oracle limits and artifacts

The shim measures intercepted libc calls attributed to selected paths. Those counts are independent of DuckDB's query-context counters, but are not physical device-sector traffic or a universal capture of every possible network/system I/O mechanism. Cache hits, mmap behavior, un-interposed functions, or wrongly selected prefixes require careful interpretation.

Missing or unreadable engine profile JSON is converted to zero counts by the helper, so raw profile artifacts matter when diagnosing a zero-versus-nonzero mismatch. The shim output and CLI exit status supply separate failure channels. Known-wrong skipped metrics remain visible; a matching skipped case is a signal to review the skip, not evidence that all unselected metrics are correct.

Use `--filter`, `--verbose`, and `--keep` for targeted reproduction with an explicit `--duckdb` binary. Retain workload config, shell hash, platform/shim build, profile JSON, shim counts, and included/excluded paths. A query returning correct rows can still fail this harness because the intended subject is metric attribution, not relational semantics.
