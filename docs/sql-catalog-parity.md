# SQL and catalog parity work

This tracks the complete SQL/types/functions and catalog/DDL goal against C++
DuckDB `99063af2bd7092aff02e14184a20e24699d34d71`. The goal is active and
substantially incomplete. A supported subset or a passing local test does not
complete any broader row below. The [rewrite principles](../specs/rewrite-principles.md)
and [strict test/performance acceptance](../specs/testing/parity.md) still apply.

SQL and file compatibility now additionally require the pinned **v1.5.5** source
build. The [two-reference campaign](reference-builds.md) preserves differing ALTER
behavior in both references as separate failing assertions; neither result is
silently substituted for the other.

| Required surface | Current evidence and remaining implementation |
| --- | --- |
| Window functions | Window binding, function registration, partitions, ordering, frames and execution remain unimplemented. |
| Recursive CTEs | New lexical iteration bindings and UNION/UNION ALL execution; full upstream CTE parity remains incomplete, including materialization syntax, USING KEY/recurring relations and combinations with other missing SQL features. |
| Grouping sets | GROUPING SETS, ROLLUP, CUBE and GROUPING/GROUPING_ID now bind to explicit set metadata and selectable aggregation algorithms. Empty inputs, duplicate sets, source/subtotal NULLs, FILTER and DISTINCT have shared conformance checks. [Reference, upstream and performance gaps](grouping/README.md) remain; this does not establish full grouping parity. |
| Broader joins and DuckDB syntax | ORDER BY ALL, typed ordering settings and global/session SET/RESET now have [shared contracts and two-reference checks](settings/README.md). NATURAL/USING, GROUP BY ALL, additional join kinds, lateral relations, set operations and broader DuckDB syntax remain open. |
| Numeric types | Decimals, unsigned families and complete coercion rules remain open; current signed integer/FLOAT/DOUBLE contracts remain covered by their existing tests. |
| Temporal types | DATE exists; remaining temporal types, timezone behavior and function coverage remain open. |
| Nested types | LIST/ARRAY/STRUCT/MAP/UNION and the broader nested value/type/function model remain open. |
| Functions and collations | Contextual scalar binding and typed current_setting use ordinary registration. The complete function catalog, overloads, collation behavior, and exact NULL/error behavior require implementation and unchanged upstream assertions; random() still blocks an upstream grouping file. |
| Views and macros | Transaction-visible definitions, binding, native persistence and lifecycle semantics remain open. |
| Sequences | Sequence state, transaction behavior, functions/default dependencies and native persistence remain open. |
| Constraints and defaults | Primary/unique/NOT NULL and literal defaults exist; broader checks, foreign keys, generated/default expressions and their dependencies remain open. |
| ALTER | Table/column rename, add/drop column, literal defaults and SET/DROP NOT NULL now execute atomically and have native WAL encoding/recovery. Type changes, other objects, nested fields, constraint DDL, dependencies/cascade, concurrent DDL timing and complete error parity remain open. |
| Attachments | Multi-catalog naming, routing, lifecycle, storage/transaction coordination and tests remain open. |
| Dependencies | General object dependencies, drop/alter behavior and invalidation remain open. |
| SQL index creation | Runtime key indexes exist; SQL index DDL and persistence metadata remain open. |
| Range access | Ordered bounds, lookup contracts, planner selection and conformance remain open. |
| Incremental index maintenance | Existing table mutations rebuild indexes; transactional incremental updates and their verification remain open. |

## Recursive execution

`PlanNode::Recursive` owns a seed and step with the same types. `RecursiveId`
identifies a lexical binding independently of SQL spelling or plan cloning.
`RecursiveInput` is valid only inside the step of a matching binding. Shared
plan validation checks scope and schemas for SQL and programmatic plans.

`RecursiveAlgorithm` controls fixed-point evaluation through the selected
physical planner. `StreamingRecursion` delivers seed batches and then each new
generation. `MaterializingRecursion` collects a finite result before delivery.
Both use public `RecursiveFrame` bindings and the ordinary physical stream,
transaction, type, cancellation and resource contracts. The step sees the
complete previous generation, including when referenced multiple times.
UNION deduplicates across generations using canonical keys from the selected
type adapters; UNION ALL retains duplicates. Recursive-input dependencies
prevent scalar subquery values being cached across changing generations.

The seed can be consumed incrementally. Subsequent generations currently
materialize the complete step result before delivery. The row budget bounds
each retained generation and UNION's distinct state; it is not byte accounting
or spill support. LIMIT/consumer stop can end the streaming algorithm without
evaluating further generations. An eager selection can encounter later errors
or resource exhaustion before returning a prefix.

`test/component/recursive.rs` exercises the shared SQL corpus across both
recursion algorithms, both optimizers, both expression evaluators, both result
executors and three batch sizes. Additional checks cover independent graph
transitive closure, prepared parameters and snapshots, late errors/rollback,
NULL/NaN equality, retained chunks, cancellation and invalid lexical bindings.

The first unchanged upstream CTE campaign selected
all 88 files under `test/sql/cte/`: three passed, 34 failed and 51 were
unsupported. Some recursive files now execute successful prefixes before a
later unsupported directive or assertion failure. Those files remain gaps.
The campaign predates subsequent local test additions and does not establish
current full-suite parity. The previous full 5,638-file report is also historical.

## Table alteration

`TableAlteration` is an owned catalog operation. `CatalogMut::alter_table` owns
the atomic metadata/row transition and rejects unsupported adapters before
effects. Binding and shared plan validation check the same metadata operation;
storage validates defaults, names and affected values before publishing a new
table version. Renames and defaults retain vectors and indexes. ADD uses a
constant column; DROP retains the other column vectors. Row IDs and append
high-water marks survive. Indexed columns and preceding column drops remain
restricted, following the reference's current index ordinal rules.

The transaction bundle also retains its starting rows with catalog mutations
applied. New NOT NULL constraints validate that basis and current transaction
rows. A transaction's uncommitted deletion or replacement of a committed NULL
does not make adding the constraint succeed. This does not yet reproduce every
C++ outstanding-update error or its timing. Writer conflicts still use the
existing optimistic commit contract; early conflicting DDL rejection remains
open.

The native encoder writes ALTER_INFO records and translates pending rows through
schema/name changes. It emits DML after the final catalog version, preserving
native undo-entry ownership and avoiding deletion of staged insertions. Recovery
settles pending validity updates before altering a schema and publishes only
complete FLUSH transactions. Tests cut a WAL at every byte after its header,
retain old snapshots/results, check rollback and prepared-statement rebinding,
and replay an independently generated native WAL with both index adapters.

The 117-file unchanged upstream ALTER campaign
passed 17 files, failed 23 and reported 77 unsupported. All 17 passes were gaps
in the historical full-suite report. Configuration directives, concurrent DDL,
type changes, other catalog objects and exact diagnostics still prevent full
ALTER parity; these outcomes are not added to a guessed full-suite total.

The pinned C++ SQL corpus comparison remains failing.
Its default PEG transformer accepts `ADD COLUMN ... NOT NULL` while discarding
that constraint; Rust rejects this currently unsupported ADD form. The local
negative assertion is retained, and the discrepancy is not counted as a pass.
The older v1.3 file oracle also differs on dropping a primary-key NOT NULL
constraint. SQL baseline behavior and native-format checks therefore have
separate reports. Initial native trials preserve
earlier encoder failures, an older DuckDB update/rename WAL that fails its own
reader, and development-version file incompatibilities. Current native-file
checks pass for their documented v1.3-supported scope.

The 12-case performance report adds ADD COLUMN,
DROP COLUMN and table rename against pinned C++. Each DDL sample resets the
catalog before timing and independently queries its effect afterward. Reset
and effect checks are untimed; actual DDL execution, result consumption and
required prepared-statement rebinding are timed. These are in-memory cases,
not evidence for durable DDL, NOT NULL validation or all catalog workloads.

## NATURAL and USING join work

The two-reference design probes capture
14 queries per pinned C++ build. These are reference behavior checks; Rust
NATURAL/USING execution is still unimplemented and none counts as a Rust pass.

USING requires more than an equality predicate. An unqualified key is merged,
while qualified left/right keys retain their separate values and types. FULL
joins coalesce the key in a common type; LEFT and RIGHT joins expose their
preserved side's key type. Unqualified wildcard output retains the left column
position and omits the corresponding right key; qualified wildcards retain
both original columns. Chained FULL joins use the preceding merged key, and
duplicate USING names are removed. NATURAL joins with no common names fail.

Binding therefore needs an explicit namespace model separate from physical row
layout, covering unqualified expressions, qualified columns, wildcard expansion,
aliases and correlated scopes. Encoding hidden SQL names in physical `Field`
names or dropping the original keys would lose required behavior. The probes
also preserve a baseline difference: an alias on a parenthesized joined table
works in the development build but is not visible to the tested v1.5.5 query.
That difference must remain explicit in future parity results.

## Completion evidence

Each surface needs explicit source-case mappings, unchanged upstream SQL and
API assertions, transaction/restart and native-format checks where applicable,
and conformance of built-in and alternative implementations. Required cases,
configuration matrices and external dependencies cannot be omitted or counted
as passed because they are unavailable. Performance evidence must cover the
new operations and preserve the existing strict C++ gates. No aggregate pass
percentage or limited benchmark can discharge the complete goal.
