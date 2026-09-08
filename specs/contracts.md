# Cross-component execution contracts

[Specification index](README.md) · [Testing](testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

For the rewrite, [principle #1: pluggable by construction](rewrite-principles.md#1-pluggable-by-construction) requires explicit interfaces and interchangeable adapters. The lifecycles and invariants below inform those contracts; they do not mandate the existing C++ types, module dependencies, or persistence implementation.

## SELECT with a blocking aggregate

1. A client submits SQL through a connection.
2. Parsing creates syntax objects; binding resolves table columns and aggregate overloads.
3. Optimization moves eligible predicates/projections and chooses plan structure.
4. Physical planning selects scan/filter/aggregate/result operators and resolves column positions.
5. The aggregate's build pipeline consumes chunks into local/shared states.
6. Combine/finalize publishes completed aggregate state; dependent output work becomes runnable.
7. Aggregate output flows into a materialized or streaming collector.
8. Result exhaustion completes query cleanup and any autocommit transaction.

## Persistent write and restart

1. Binding resolves the target and validates columns, types, constraints and statement properties.
2. DML execution modifies transaction-local/native storage and records required undo/index effects.
3. Commit chooses the valid durability path and reports flush/commit failures.
4. Checkpoint eventually consolidates metadata and data into persistent format.
5. Reopening loads headers/metadata and replays committed WAL as needed.
6. Recovered reads must match the committed state; aborted or incomplete work must not appear as committed data.

## External scan

1. A table function or replacement scan binds a file/resource and produces a schema.
2. The optimizer negotiates projection, filters, partitions and statistics.
3. Global/local scan state assigns work and opens resources through the appropriate file-system/Arrow interface.
4. Decoding produces correctly typed vectors and propagates errors and I/O context.
5. Remaining SQL operators evaluate predicates and transformations not handled by the source.
6. Completion/cancellation releases file handles, Arrow dependencies and scan state.

## Cross-component invariants

| Invariant | Typical failure if violated |
| --- | --- |
| Bindings remain valid after each logical rewrite | Wrong-column reads, invalid plans, optimizer assertions |
| Reordering respects volatility, effects, security and transaction-visible data | Incorrect results or unintended external effects |
| Vector type, size, selection and validity agree | Out-of-bounds access, NULL corruption, wrong nested values |
| Physical-plan/operator state outlives its tasks | Use-after-free during cancellation or shutdown |
| Blocked work resumes exactly once with preserved state | Lost rows, duplicate rows, deadlock or busy loop |
| Shared state is synchronized; local state has one lifecycle | Races, duplicate finalization, corruption |
| Storage/index/undo changes participate in the same transaction | Constraint failures, rollback anomalies, stale indexes |
| Durable publication follows the selected WAL/checkpoint protocol | Lost committed data or inconsistent recovery |
| Pinned/borrowed memory is not used after owner release | Buffer, Arrow, API or result lifetime bugs |
| Serialized fields and API slots preserve compatibility contracts | Old plans/databases/extensions become unreadable or misdispatch calls |
| Query context reaches attributed I/O | Incorrect profiling byte counts |
| Test selection and skips are observable | Apparent green runs with missing coverage |
