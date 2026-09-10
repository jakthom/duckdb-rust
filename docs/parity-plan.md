# Full DuckDB parity plan

This is the active goal. The implementation is an operational subset; completing
the development tracing package did not complete engine, test or ecosystem parity.
The [rewrite principles](../specs/rewrite-principles.md) and
[acceptance requirements](../specs/testing/parity.md) govern every step.

Both references are mandatory: v1.5.5 (`d8cdaa33fd`) and the separately pinned
development revision (`99063af2bd`). The [reference build runbook](reference-builds.md)
defines the selected binaries. Source and executable identities must be checked
before campaigns. Differences between reference expectations remain explicit;
one target cannot stand in for the other. Historical v1.3 results are not a
current compatibility baseline.

Implementation priority is porting database functionality to Rust. The current
increment completes binding namespaces with NATURAL/USING joins, set operations,
and the first window-function/QUALIFY implementation. See the
[relational port](relational-port.md) for exact capabilities and evidence. Use the existing tests and reference
builds to verify each increment. Change a harness when a missing behavior blocks
faithful verification; expanding verification infrastructure is not a prerequisite
for continuing engine implementation.

## Ordered work and completion evidence

| Step | Work | Evidence required | Status |
| --- | --- | --- | --- |
| 1 | Establish a current, complete parity baseline | Both source inventories, compiled registrations, generated instances, clients, extensions and configuration populations; fresh execution and untraced performance outcomes with exact identities | Open; use existing verification while porting behavior |
| 2 | Finish verification harness semantics | Unchanged SQLLogicTest assertions, controls, fixtures, comparison rules, connection/restart/concurrency behavior; mapped native/client assertions and negative harness tests | Open |
| 3 | Repair existing compatibility failures | v1.5.5 WAL metadata and ALTER discrepancies resolved; valid reference-produced fixtures and bidirectional read/write/recovery checks | Open |
| 4 | Binding namespaces and NATURAL/USING joins | Merged and qualified keys, wildcards, aliases, outer joins and correlated scopes; unchanged upstream cases and alternative join adapters | Implemented and verified in the relational port; two reference-version differences are explicit |
| 5 | Foundational types, casts and function resolution | Unsigned, decimal, temporal and nested families, collations/coercion/overloads across vectors, expressions, indexes, files and results | Open |
| 6 | Remaining SQL and catalog behavior | Windows/frames, joins/lateral/set operations, CTEs/functions, views/macros/sequences/dependencies/constraints/attachments and DDL | Set operations and core windows/QUALIFY implemented; remaining frames, SQL and catalog behavior open |
| 7 | Native storage and transaction compatibility | Required formats/codecs/metadata/WAL/checkpoints, bounded storage access, conflicts, incremental indexes and crash recovery | Open |
| 8 | Execution and resources at scale | Memory accounting, buffers, spill, parallel scheduling and required planning behavior; comparable size/concurrency/resource workloads | Open |
| 9 | External data access | Required CSV/JSON/Parquet and inventoried formats/filesystems, streaming, pushdown, import/export and failures | Open |
| 10 | Clients, UX and extensions | Compatible C interfaces, Arrow/ADBC, language clients, shell/UI behavior, extension installation/loading and separate ABI compatibility gates | Open |
| 11 | Complete acceptance matrix | Full required cases/configurations/platforms plus allocation, persistence, concurrency, fuzz, resource, mutation and coverage checks; every performance scope measured | Open |

Resolve extension compatibility contracts in step 1, before loader implementation:
stable C extension tables, version-coupled C/C++ interfaces, and source-level
registration are different obligations. SQL compatibility does not imply binary
extension compatibility.

## Requirements throughout the work

- Preserve original assertions and case identities. Unsupported, unported,
  skipped, timed-out, crashed, missing and unexecuted cases remain gaps.
- Distinguish retained source declarations from compiled registrations,
  generated instances and executed assertions. Report each population separately.
- Test built-in and alternative adapters through the same contracts. Prove
  each required seam with two meaningful implementations and unchanged callers.
- Use untraced release builds for performance acceptance. Every comparable
  median latency/cost ratio above 1.0 fails; throughput must not decrease.
  Faster cases cannot compensate for slower cases. Unmeasured costs remain open.
- Add relevant SQLite-inspired failure, concurrency and ownership checks as
  each subsystem is implemented; step 11 audits their complete coverage.
- Use ordinary checks/tests on each pass. Enable exhaustive tracing only for a
  focused investigation. Runtime telemetry stays disposable and Git-ignored.
- Keep source fixtures and compact validation summaries distinct from operation
  telemetry. Source inventory is an input obligation, never a passing result.
- Keep the root README identical to main. Do not edit README files.

The previous full SQL campaign and sixteen-workload performance results are
historical, scoped evidence. They cannot be promoted to a current full-parity
claim. See [test parity](testing-parity.md), [SQL/catalog work](sql-catalog-parity.md),
[architecture](architecture.md), and [SQLite testing](sqlite-testing-review.md).
