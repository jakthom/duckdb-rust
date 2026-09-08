# Settings, profiling, logging, and errors

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

`DBConfig` owns database-global options and registries. `ClientConfig` contains connection-level choices. Generated settings metadata defines names, types, defaults and scopes, with custom setting implementations where needed. Changing a setting can require scheduler reconfiguration, a different planning path, or different storage behavior; it is not always a simple map update.

Profiling spans planning, optimizer passes, physical operators, execution, storage and I/O. `QueryProfiler`, metric definitions and profiling nodes produce structured or rendered output. Logging uses typed events and scope/context information with pluggable storage. Progress and cancellation are separate from profiling completion.

Errors flow through typed exceptions and `ErrorData`, then become API-specific errors/results. `ValidChecker` can invalidate a transaction, attachment or database depending on the failure. Assertions represent violated internal invariants, while malformed SQL and invalid user values should receive normal errors.

`max_execution_time` is checked through cooperative interrupt points. It is not an unconditional wall-clock limit on planning or every foreign callback. The process-level test runner therefore needs its own timeout for hangs or crashes that the query interrupt mechanism cannot handle.

Sources: [configuration](../../../duckdb/src/include/duckdb/main/config.hpp), [settings source](../../../duckdb/src/common/settings.json), [profiling](../../../duckdb/src/main/profiler/), [query profiler](../../../duckdb/src/include/duckdb/main/query_profiler.hpp), [logging](../../../duckdb/src/logging/), [error_data.cpp](../../../duckdb/src/common/error_data.cpp), [client context](../../../duckdb/src/main/client_context.cpp).

## Configuration propagation

Settings metadata supplies type/default/scope information, while implementation hooks validate and apply changes. Database-wide changes can affect shared services such as worker pools or buffer policy; connection settings can affect parsing, planning, execution, output conversion, and profiling. A setting accepted by SQL should not be assumed to affect an already prepared plan retroactively unless that code path consults it at execution time.

Generated settings source and custom implementations must stay aligned. Tests should cover invalid values, scope restrictions, initialization-time versus runtime changes, and restoration between test cases. Query verification/debug settings deliberately perturb representations or plans and belong to testing configuration rather than a user-facing semantic feature.

## Profiling and metrics flow

Planning and optimizer phases contribute timing; execution contributes operator-level state, cardinality, and work metrics; storage/filesystems contribute I/O attributed through context. `QueryProfiler` assembles these into profiling information for structured or rendered output. Parallel timing can represent accumulated worker work rather than wall-clock duration, so adding operator times is not a universal query-latency calculation.

A profile is completed through query lifecycle cleanup. Streaming execution can remain active after the initiating API call; inspecting a profile too early may not describe completed work. Progress estimates are advisory and use a different interface from final row counts or successful commit. Disabling progress display does not disable execution or enforce a timeout.

## Logging and error domains

`LogManager`, `Logger`, `LogType`, and `LogStorage` separate configuration/routing, scoped emission, typed events, and retention. A logging backend must respect concurrent emission and shutdown lifetimes. Runtime teardown keeps logging alive for early cleanup and then stops it before destroying remaining dependent services.

`ErrorData` normalizes engine diagnostics for propagation through result/API boundaries. `ValidChecker` can mark state unusable after serious failures. The distinction between user input errors, interrupt/timeouts, transaction conflicts, I/O failures, and internal errors affects both caller recovery and test oracles. String matching in a test harness is a weaker interface than a stable structured error category and should be documented as such.

Sources: [logging interfaces](../../../duckdb/src/include/duckdb/logging/), [ErrorData](../../../duckdb/src/include/duckdb/common/error_data.hpp), [validity checking](../../../duckdb/src/include/duckdb/main/valid_checker.hpp).

## Verification requirements

Check setting scope/type validation, profiling output after full and partial consumption, metrics under parallelism, logging storage behavior, and error translation in every API family. Avoid exact timing assertions in correctness tests. I/O counters can have exact fixture-specific expectations, handled by the [I/O metric harness](../testing/io-metrics.md). Query-level timeouts need separate process-timeout coverage in [orchestration](../testing/orchestration.md); cooperative interrupt checks cannot recover an arbitrary hung foreign callback.
