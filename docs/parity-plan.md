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

Development behavior takes precedence when the two references disagree. For
each semantically comparable workload, the faster reference is the performance
baseline, with a maximum Rust/C++ median latency ratio of 1.0. Record both
measurements and the selected baseline; non-comparable or unmeasured scopes
remain open. See the acceptance requirements for the 2026-09-10 clarification.

Implementation priority is porting database functionality to Rust. The current
large assignment is the [value-and-expression subsystem](../specs/value-expression-milestone.md),
with [continuous integration progress](value-expression-progress.md). Numeric,
temporal and nested workers share that end-to-end outcome; type declarations and
small commits are internal steps. Its third integrated checkpoint preserves all
prior upstream passes (391 passing files / 18,606 passing records), repairs three
diagnostic regressions, and passes all 34 existing faster-reference workloads.
The later fifth checkpoint reaches 423 passing files and 19,240 successful
records, with no lost full-file pass; its three retained timing trials still fail
decimal total aggregation (33/34), so follow-up publication is held for repair.
These later results do not inherit the earlier performance pass.
Full value/function/native parity remains open. The preceding increment added unsigned and
decimal types and coercion; its
[progress report](numeric-port.md) records the implemented foundation and open
correctness, native-file and performance obligations. The latest
[binding follow-up](binding-regressions.md) corrects destination-directed VALUES
assignment and schema/CTE name resolution. The
[numeric batch work](numeric-batches.md) addresses the measured regressions
through selected type, cast and aggregate interfaces. Its final 34-workload
matrix passes the faster-reference gate and its full SQL refresh loses no
previously passing file. Retained trials and validation are separate from the
foundation checkpoint; full numeric and development-file parity remain open.
The preceding [relational port](relational-port.md) implements NATURAL/USING joins, set
operations and the first window-function/QUALIFY support. Use the existing tests and reference
builds to verify each increment. Change a harness when a missing behavior blocks
faithful verification; expanding verification infrastructure is not a prerequisite
for continuing engine implementation.

## Ordered work and completion evidence

| Step | Work | Evidence required | Status |
| --- | --- | --- | --- |
| 1 | Establish a current, complete parity baseline | Both source inventories, compiled registrations, generated instances, clients, extensions and configuration populations; fresh execution and untraced performance outcomes with exact identities | Open; use existing verification while porting behavior |
| 2 | Finish verification harness semantics | Unchanged SQLLogicTest assertions, controls, fixtures, comparison rules, connection/restart/concurrency behavior; mapped native/client assertions and negative harness tests | Open |
| 3 | Repair existing compatibility failures | v1.5.5 WAL metadata and ALTER discrepancies resolved; valid reference-produced fixtures and bidirectional read/write/recovery checks | Open |
| 4 | Binding namespaces and NATURAL/USING joins | Merged and qualified keys, wildcards, aliases, outer joins and correlated scopes; unchanged upstream cases and alternative join adapters | Selected join and schema/CTE namespace contracts verified; two reference-version differences are explicit; full binding parity remains open |
| 5 | Foundational types, casts and function resolution | Unsigned, decimal, temporal and nested families, collations/coercion/overloads across vectors, expressions, indexes, files and results | Active value-and-expression assignment: numeric, BLOB/UUID/ENUM, temporal and nested SQL/checkpoint increments integrated; mixed binding, native streams and regression checks are in the value-expression report. Broader families, catalog/functions, nested WAL/defaults and full parity remain open |
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
- At each substantial implementation-stage boundary, run
  `python3 scripts/verify_kani.py` and briefly report findings and limitations.
  Investigate counterexamples; let proofs follow the emerging design. Kani
  success is not required for exploratory stage completion, and verifier limits
  must not dictate implementation choices. See the [Kani
  policy](../specs/testing/kani.md); formal proof requirements can come later.
- Keep source fixtures and compact validation summaries distinct from operation
  telemetry. Source inventory is an input obligation, never a passing result.
- Keep the root README identical to main. Do not edit README files.

The previous full SQL campaign and sixteen-workload performance results are
historical, scoped evidence. They cannot be promoted to a current full-parity
claim. See [test parity](testing-parity.md), [SQL/catalog work](sql-catalog-parity.md),
[architecture](architecture.md), and [SQLite testing](sqlite-testing-review.md).
