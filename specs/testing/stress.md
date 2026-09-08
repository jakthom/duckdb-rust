# Stress, fault injection, memory, and sanitizers

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Memory-growth supervisor

[test_memory_leaks.py](../../../duckdb/test/memoryleak/test_memory_leaks.py) discovers `[memoryleak]` cases and starts them with `--memory-leak-tests`. Those C++ tests deliberately loop until terminated. The supervisor samples RSS using `ps` and decides whether growth stabilizes within configured percentage/absolute thresholds and a timeout. It tests repeated lifetime behavior such as create/drop or connect/disconnect.

This is a memory-growth heuristic, distinct from an allocation-level leak sanitizer. Allocator retention and workload warmup affect RSS. The script contains broad `killall -9 unittest` cleanup behavior; run it only in a dedicated test environment with no unrelated `unittest` processes. That is an observed operational property of this harness, not a command required for normal test execution.

Source: [memory-leak README](../../../duckdb/test/memoryleak/README.md).

## Crash/recovery and initialization

Native persistence cases use process/restart behavior to exercise WAL and recovery. SQL storage regressions add explicit `load`, `restart`, transactions and checkpoints. Forced-storage/restart configurations apply those boundaries to a larger part of the SQL corpus.

[test_zero_initialize.py](../../../duckdb/scripts/test_zero_initialize.py) runs selected storage cases with different initialization patterns and compares produced database bytes using its format-specific handling. Its purpose is to detect bytes written from uninitialized memory. It has explicit header/block assumptions and a fixed selected case list; it is not a byte-level verifier for every storage format or every SQL test.

## Sanitizers and dynamic instrumentation

Address/undefined-behavior sanitizers, ThreadSanitizer, Valgrind, assertion-enabled release builds, allocation instrumentation and forced asynchronous blocking are complementary mechanisms. They execute existing cases under altered instrumentation rather than supplying a common new expected-result format.

The [nightly workflow](../../../duckdb/.github/workflows/NightlyTests.yml) demonstrates a custom Valgrind batch command. [Main.yml](../../../duckdb/.github/workflows/Main.yml) and the Makefile supply TSAN-specific configurations and intraquery/interquery selections. Suppression files are explicit parts of the effective instrumentation configuration.

## Persistence and fault-injection division

Ordinary persistence tests exercise committed state across close/reopen and explicit recovery scenarios. The storage operation fuzzer adds one-shot filesystem failures and expected-state comparison; its detailed algorithm and oracle limits are in [fuzzer.md](fuzzer.md). SQL restart configurations broaden reopen coverage across the corpus but cannot simulate every process-crash or failed-fsync ordering merely by opening the file again.

A deterministic persistence regression should identify the acknowledgment boundary: which transaction completed successfully, which operation failed, and what the next connection/process must observe. Retain the database plus relevant WAL/checkpoint state after failure. Deleting recovery files to make a rerun open successfully destroys the evidence under test.

## Instrumentation and resource contracts

ASan/UBSan target memory and undefined behavior; TSan targets races; Valgrind and allocation instrumentation have their own coverage and suppressions. They require compatible builds and dependencies. Combining every tool in one binary is not an assumed supported configuration. Suppression files and disabled tests are part of the effective campaign and should be reported.

The RSS supervisor repeatedly samples a process whose test is intentionally long-running. Its result depends on warmup, allocator behavior, sampling interval, thresholds, and timeout. A stable RSS does not rule out all leaks, and increasing RSS does not by itself identify a missing free. Preserve the time series and workload identity rather than treating the verdict as allocation-level proof.

## Acceptance and safe execution

Use a dedicated environment for the memory supervisor because of its broad process cleanup behavior. For sanitizer/stress runs, constrain worker processes and engine threads together, preserve first-failure diagnostics, and separate resource exhaustion from a confirmed engine invariant failure. Forced asynchronous blocking and repeated interquery/intraquery tests can expose readiness races that deterministic SQL result checks miss.

Infrastructure success means the intended instrumented binary ran the selected cases with expected capabilities; engine success additionally means no applicable sanitizer/invariant/result failure occurred. Neither establishes exhaustion of all concurrency schedules or failure sites. [CI](ci.md) records actual configured campaigns; [runbook](runbook.md) contains ordinary bounded reproduction recipes.
