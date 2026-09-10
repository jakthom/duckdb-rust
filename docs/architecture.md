# Implemented architecture and remaining work

This describes the implementation, separately from the accepted [rewrite principles](../specs/rewrite-principles.md) and the specifications of the C++ source system. The implementation is an operational, incomplete v0. Contracts remain editable; there is no stable Rust ABI or private snapshot format promise.

## Composition and boundaries

[`DatabaseBuilder`](../src/main/database.rs) is the composition root. Callers select trait implementations through owned `Arc` handles. Adapters are fixed for a database instance. There are no downcasts, hidden calls into C++ DuckDB, or runtime subprocesses. The independent DuckDB executable is used only by verification scripts.

| Boundary | Contract and implementation | Replacement evidence |
| --- | --- | --- |
| Catalog | `Catalog` / `CatalogMut`; transaction-local schemas, `TableDefinition` and atomic owned `TableAlteration` operations | Snapshot implementation only; ALTER shares transaction, index, checkpoint and WAL callers |
| Access methods | `TableStorage` / `TableStorageMut`; stable row identities, incremental `TableScan`, positional fetch, mutations, capability discovery | Snapshot implementation; direct indexed equality and ordered positional fetch |
| Indexes | `IndexFactory` / `KeyIndex`; owned immutable equality indexes, typed keys, unique constraints, row IDs, cancellation | Hash and B-tree implementations run the same contracts and restart matrix |
| Transactions | `TransactionManager` / `Transaction`; catalog and data share visibility and publication | Optimistic snapshot implementation only |
| Durability | `Durability`; load, optional journal capture, durable commit publication and explicit checkpoint maintenance | Memory, full file checkpoint and native WAL adapters share transaction callers; maintenance failure and uncertain commit are distinct |
| Checkpoint scheduling | `CheckpointPolicy`; pure decisions over committed log progress and pending append size | Log-size and commit-count policies share live publication, visibility, interruption and independent file checks |
| Transaction logging | `TransactionLog` / `LogSession`; pure initialization, transaction encoding and checkpoint rebasing with owned successor state | Native v2 encoder only; a second meaningful encoder remains required |
| Recovery | `Recovery` / `RecoveryTarget`; checkpoint-family validation, atomic replay, deferred constraints and pure publication preparation | DuckDB WAL v2 adapter and snapshot target only; this seam still needs a second meaningful implementation |
| Representation | `SnapshotFormat`; complete owned catalog/data encoding and decoding, explicit successor row mappings | JSON and native DuckDB formats share value/schema tests; native successor layout is checked before publication |
| Segment decoding | `SegmentDecoder` / `DecoderRegistry`; typed owned output, borrowed block source, explicit wire IDs, cancellation and checked dispatch | Word and scalar bitpacking adapters share conformance and file replacement tests; ten native codec families |
| File publication | `CheckpointStorage`; owned reads, writer lease, durable replacement and recovery log retirement | Local filesystem implementation only; named I/O boundaries accept fault injectors |
| SQL frontend | `Parser`; SQL to syntax tree | DuckDB dialect through the Rust `sqlparser` crate |
| Binding | `Binder`; syntax and transaction catalog to typed statements | SQL binder only; alternative frontends can submit typed statements |
| Logical contract | `BoundStatement`, `LogicalPlan`, `BoundExpr`; schema, ordinals, types and operation semantics | Validation before execution and after optimization; malformed plan tests |
| Optimization | `Optimizer`; owned `ValidatedPlan` transformation | Identity and configurable pass pipeline; expression simplification, index selection and conservative EXISTS decorrelation, with validation after each pass; no cost model |
| Physical planning | `PhysicalPlanner` / `PhysicalOperator` | Native compiler only; immutable shared plans open independent `BatchStream` state; delivery modes are visible in EXPLAIN |
| Join algorithms | `JoinAlgorithm`; supported predicates, independent cursors and SQL join semantics | Hash and nested-loop algorithms share contracts; hash semi/anti joins stream probes over a bounded build |
| Aggregation algorithms | `AggregationAlgorithm`; bound groups, grouping sets, aggregate functions and grouping masks over one input stream | Hash and ordered grouping indexes share SQL, effect, resource and snapshot contracts; physical selection is explicit |
| Sorting algorithms | `SortAlgorithm`; consume one validated stream, evaluate typed keys once and return owned ordered rows | Stable comparison merge sort and integer radix sort share SQL, type, effect, encoding, cancellation and row-limit contracts; physical selection is explicit |
| Casts | `CastFunction` / `CastRegistry` / `BoundCast`; exact source/target/mode selection, retained adapters, checked physical values and errors | Standard-library and checked-digit integer parsers share conformance and SQL/storage paths |
| Scalar operators | `OperatorRegistry` / `OperatorFunction` / `BoundOperator`; overload selection, explicit casts, retained effects and checked results | Numeric/date arithmetic and concatenation; dynamic-programming and greedy LIKE implementations share contracts and SQL callers |
| Expressions | `ExpressionEvaluator` / `EvaluationContext`; typed scalar and batch results, explicit outer rows and relational dependencies, NULL/error and evaluation behavior | Scalar and batched evaluators share SQL, replacement, overflow, lazy-branch and effect tests; resource-only contexts reject nested query execution |
| Subquery execution | `SubqueryExecutor`; scalar cardinality, existence and typed membership over fresh physical streams | Streaming and materializing adapters share SQL, scope, type, mutation, cancellation and independent file checks |
| Recursive execution | `RecursiveAlgorithm`, `RecursivePlan`, `RecursiveFrame`; lexical iteration input, UNION equality and fixed-point evaluation | Streaming and materializing algorithms share scope/type/SQL contracts; full CTE parity remains open in [SQL/catalog tracking](sql-catalog-parity.md) |
| Functions | `ScalarFunction`, `ScalarBindArguments`, `AggregateFunction`, aggregate states; registration, signatures and optional contextual specialization | Ordinary registration used by built-ins, typed `current_setting` and replacement functions; no binary extension loader |
| Configuration | `Setting`, `SettingRegistry`, `Configuration` / `ConfigurationSession`; typed registration, global/session scope, immutable statement views and atomic updates | Snapshot and locked providers share scope, type, ownership, cancellation and scheduler-failure contracts |
| Execution | `Executor` / `ResultSink`; demand, owned chunks, early stop and completion | Pull and eager materialization adapters run the same complete-result contracts |
| Scheduling | `Scheduler`; execute the supplied task exactly once or return an error | Inline scheduler only |
| Resources | `QueryContext`; cancellation, deadline, batch configuration, row count limits | Row accounting only; no byte allocator, spill, buffer pool or scheduler fairness |
| Types | `TypeRegistry` / `TypeAdapter` / `BoundType`; parameterized identity, validation, common-type selection, comparison and canonical keys | Primitive types use ordinary registration; materialized and streaming ASCII adapters share contracts, SQL operators, index and restart tests |
| Values and vectors | Owned `Value`, shared extension metadata/payloads, immutable flat/constant/dictionary `Vector`, `DataChunk` | Physical shape and selection checks; registered logical values are validated at operator boundaries |

The snapshot transaction bundle exposes its catalog, access, indexes, durability, format, and file contracts separately. Its `Snapshot` is also the shared checkpoint model for that bundle. A different transaction/storage engine can implement `TransactionManager` without using this representation. Catalog and data must still share the transaction contract. Combining independent stores does not provide cross-store atomicity.

A declared interface alone is not a proven seam. The table records implementation diversity, not production acceptance. The current logical operation enum covers relational operations only. General numeric/nested type families, broader function overload resolution, graph operations, asynchronous/parallel execution, file-format projection/pushdown, compression analysis/encoding/access policies and resource policies remain unfinished requirements.

## Ownership, concurrency and failure

Connections require exclusive mutable access for execution. Batch callbacks run synchronously within that borrow and may retain their owned chunks; the statement snapshot stays alive for the complete call. Successful early stop restores an explicit transaction or finishes the implicit read transaction. Consumer errors and cancellation use the ordinary aborted-transaction rules. A failed batch query may already have delivered a prefix, which is not a successful result. Multiple connections share a database; query results own their data and outlive both connection and database. Vectors keep selected values alive through shared ownership and validate cardinality and selected positions. Vector constructors and table mutations require physically typed values; they never perform implicit SQL conversions.

Each transaction starts with a copy-on-write snapshot. Row IDs are stable within a table's transaction history, are not reused after deletion, and positional fetch preserves requested order, duplicates and missing positions. IDs are not durable external identities across file reopen. Key lookup uses only an advertised index and returns ascending matching IDs and rows. An unavailable key returns `Unsupported` without scanning. Index factories are selected when constructing the transaction bundle, and table/index replacements are published atomically. Reopen rebuilds runtime indexes from decoded rows. These access methods do not imply selective file I/O.

Published snapshot tables retain immutable columns and sorted row identities.
Bulk scans return owning contiguous views; single-row demand constructs just the
requested row. A private writer materializes rows once, applies changes, validates
constraints and seals columns before publication. The two representations replace
each other; there is no duplicate row/column cache or cached query result. The
snapshot format still describes logical rows independently of this layout. This
removes repeated transposition during scans, but incremental column mutation and
measurement of write, memory and cold-read costs remain open.

Vector construction validates physical values and establishes a conservative
proof when every value is non-NULL. Slices and selections preserve that proof;
adapters cannot assert it unchecked. A flat vector retains its original allocation
through shared ownership. Scalar operators and type comparisons have checked batch
interfaces with scalar defaults. The default batched expression evaluator uses
retained adapters only for trees proved total and without effects. Operators must
explicitly supply the proof for their signature and known arguments. Casts can
also advertise totality; unproved casts, lazy branches, relational dependencies
and expressions without a proof preserve scalar
evaluation order. Batch boundaries check schema, cardinality, logical payloads,
NULL propagation and cancellation, including values a filter would reject.

The optimizer obtains catalog and access capabilities from the same statement snapshot. Its independently selected passes own plan transformations; built-in traversal consumes inputs without copying subtrees. Equality lookup currently accepts conjunctions of column/constant comparisons and retains the complete predicate. It declines expressions whose skipped evaluation could suppress errors or effects. SQL UPDATE and DELETE still scan, and indexes rebuild after every table mutation. Range seeks, expression indexes, index DDL, incremental maintenance, and costing remain open.

`ValidatedPlan` owns an immutable logical plan and borrows its statement's optimizer context. Its public constructor checks the complete plan; there is no unchecked constructor or mutable plan accessor. Optimizers use `rewrite` to consume a plan and validate its replacement. An unchanged plan can retain its validation, while each pipeline pass validates its full output. The runtime accepts the result only for the exact context object that supplied the input, preventing transfer of validation from another snapshot. This removes duplicate runtime/optimizer checks without trusting an adapter's claim that its output is valid. It checks structural and type invariants; preserving the query's meaning and effects remains an optimizer conformance obligation.

The default expression-simplification pass folds successful constant casts and operators whose declared effects permit folding through their retained adapters and removes filters known to be true. Failed attempts remain expressions, preserving error timing for short-circuit branches and empty inputs. Folding preserves declared logical metadata and floating-point bits. Functions with effects are never evaluated by this pass. Expression-child and local-plan-expression traversal preserve ownership and selected adapters; each completed pass still undergoes full plan validation.

Selected scalar specializations declare both argument types and per-argument
cast modes. The ordinary language binder inserts checked casts through its
selected registry; functions do not stringify or convert values behind that
boundary. The default remains implicit conversion. An explicit conversion for
one argument does not grant it to another argument, and SQL literal privilege
remains separate from typed API parameter identity. Tests cover retained selected
casts, both evaluators/optimizers, NULLs, prepared arguments and fatal failures
through an outer TRY_CAST followed by rollback.

Read-only transactions retain their snapshots. A writer conflicts with every intervening successful writer, including disjoint writes. The result is a conservative serializable model with no automatic retries. A committed snapshot is published before becoming visible. Dropping a transaction discards it. Definite publication failure preserves committed state; `CommitUnknown` makes subsequent transaction starts fail until the database is reopened and recovered.

Explicit transaction execution errors abort the transaction and require `ROLLBACK`. Binding errors retain it. Autocommit operates per statement: earlier statements remain committed if a later statement in the submitted SQL fails. Prepared statements retain syntax and rebind catalog and parameters for each execution.

The local file adapter canonicalizes paths, acquires nonblocking OS locks, and retains a process lease. On Unix it uses `fcntl` locks compatible with DuckDB. Separate database instances for the same file are rejected within one process; share a `Database` handle instead. Read-only access advertises no mutation capability. Writable hard-linked files and orphan WALs are rejected. Both open modes can select recovery. `.wal.checkpoint` and `.wal.recovery` sidecars are rejected until concurrent checkpoint reconciliation is implemented.

Ordinary publication writes a uniquely named sibling file, syncs it, renames it atomically, retains the lock on the new inode, and syncs the parent directory. Existing file permission bits are preserved. Direct replacement rejects an active WAL so it cannot discard unrecovered commits. Failed temporary-file creation never removes a preexisting file. Ordinary errors clean up owned temporary files; abrupt process exit can leave unreferenced temporary files. The local adapter has been exercised on macOS; other operating systems are not claimed verified.

## Sorting and projection

`NativePhysicalPlanner::with_sorting` selects an ordinary `SortAlgorithm` adapter.
Both implementations consume the same validated input stream, evaluate each key
once per row, retain payload/key association and return owned rows. NULL placement
is independent of direction; ties retain input order. This stable-tie contract is
stronger than SQL's ordering guarantee for unresolved ties. It does not establish
an upstream insertion-order configuration or parallel ordering implementation.

`ComparisonSort` uses fallible stable merge sorting over a row permutation.
Every key passes its retained type validator before sorting, including singleton
inputs that need no comparisons. Comparator failures propagate directly without
turning errors into an inconsistent infallible comparator.

`RadixSort` retains input chunks, typed integer key columns and row addresses.
It performs stable counting passes over integer offsets and a separate NULL
category, then gathers owned output rows. Offsets cover the full signed 128-bit
domain without signed subtraction overflow. Each key requires both a pure/total
expression proof and its type adapter's `OrderingRepresentation::SignedInteger`
capability. Other signatures use comparison sorting. Equality capabilities do
not imply ordering capabilities; an integer type may retain a custom comparator
while advertising native integer equality. Bound types retain both selections.

Pure, total projections use the selected expression evaluator's checked batch
interface. A single-root projection can also use that interface, whose contract
preserves logical row order and the first error. Multiple roots without a
totality proof retain inter-column row evaluation. Restricted pure one-column
trees over small dictionaries can reuse complete root results at their first
logical occurrence; scalar callbacks and relational dependencies are excluded.
Only unsigned/decimal whole-partition window outputs use dictionary compaction;
signed outputs retain flat delivery and floats are excluded. Scalar and batch result boundaries retain type,
cardinality and cancellation checks.

Sorting currently blocks and materializes its result. Cancellation is checked
during input, key construction, radix passes, comparisons and output gathering;
retained cardinality obeys the query row limit. Byte accounting, bounded Top-N,
spill, sorted runs and parallel output remain unfinished. The
[shared sorting contracts and measured scope](settings/README.md) do not imply
full sorting or resource parity.

## Configuration and contextual functions

`DatabaseBuilder::configuration` selects the provider. Its instance defines the
global setting domain; the default builder creates a fresh provider, while
explicitly sharing one provider shares that domain. Each connection owns a
configuration session. Every statement retains an immutable `SettingsSnapshot`
with its global generation, session overrides and registered defaults. Global
changes become visible to other sessions on their next statement. Session
overrides survive transaction rollback and disappear on RESET or disconnect.
Resetting a session override reveals the current global value.

`SettingRegistry` validates names, aliases, declared types, scopes and defaults.
Ordinary registered `Setting` adapters normalize values without effects. Binding
uses the selected cast and expression adapters, then produces an owned
`SettingChange` tied to that registry. Publication validates the change and
cancellation before changing state. Failed normalization, incompatible registries
or scheduler failure cannot publish a partial configuration change. The runtime
publishes only after the scheduler successfully executes its task exactly once.
Configuration changes are separate from transactional catalog/data publication.

`SnapshotConfiguration` retains shared immutable global maps and copies on write;
`LockedConfiguration` copies a mutex-protected map when taking a statement view.
Both preserve retained views and use the same registration and session contracts.
`default_order`, `default_null_order` and its `null_order` alias are registered
built-ins. SELECT and set-operation ordering resolve defaults through the same
snapshot. `ORDER BY ALL` expands to output ordinals after wildcard expansion,
so it never reevaluates projected values to produce sort keys.

`ScalarFunction::bind` can request typed, constant arguments through the
language-owned `ScalarBindArguments` interface. Only explicitly requested closed,
effect-free expressions are evaluated, using the selected evaluator and logical
type validation. Ordinary functions retain their existing lazy/error behavior.
`current_setting` uses this interface to capture a typed value and return type;
there is no function-name branch in its callers. Prepared statements rebind the
value for each execution. [Settings verification](settings/README.md) records
the supported behavior and differences between the two C++ references.

The same interface exposes SQL string-literal identity and signed integer
literal values for contextual overload selection. Defaults grant neither hint
to other frontends. Explicit casts, unary plus and CASE expressions do not
inherit a child's literal identity; early CASE dependency pruning retains a
CASE node even when no branches remain. Typed API parameters use a distinct
bound node. They remain constants for evaluation, vectorization, aggregation,
optimizer folding and index lookup without becoming SQL literal hints.

## Table alteration

`CatalogMut::alter_table` publishes metadata and affected rows atomically in its
transaction. `TableAlteration` carries names, typed literal defaults and the
operation, independently of parser ASTs or physical operators. Renames,
add/drop column, literal default changes and SET/DROP NOT NULL are implemented.
Unknown adapters reject before effects. No general ALTER or dependency parity
is implied; the [SQL/catalog worklist](sql-catalog-parity.md#table-alteration)
records the remaining forms and independent reference discrepancies.

Published column vectors and indexes survive metadata-only changes. Added
columns use constant vectors and removed columns retain the other vectors.
The existing native restriction on indexed-column ordinals is checked before
changes. Row identities, old snapshots and owned query results remain usable.
The transaction bundle retains a catalog basis over its starting rows for
constraint validation; normal DML affects only its current snapshot. Both
versions apply catalog changes and disappear on rollback.

Native WAL records express these operations directly. Pending DML follows the
final table version, with row shapes and names translated in transaction order.
Validity updates are settled before schema changes during recovery. Whole-WAL
prefix tests verify that no partial ALTER transaction becomes visible.

## WAL recovery

`FileCheckpoint::with_recovery` composes a `Recovery` adapter with a checkpoint
codec. `FormatId` declares their checkpoint family and rejects incompatible
composition before loading. `CheckpointStorage::read_log` supplies owned bytes
under the retained database lease; format and recovery code never access paths.
`Database::open`, `Database::open_read_only` and both native shell modes select
`DuckDbWalRecovery`. A format-only composition rejects a nonempty log; writable
recovery also requires the file adapter's declared publication capability.

The v2 adapter checks each complete record's size, checksum and envelope before
replaying any transaction. A short frame header or payload at the end excludes
its unflushed transaction. A complete frame with a bad checksum is corruption,
even in an uncommitted tail. Truncated version headers are rejected. A complete
checkpoint marker may end the log or precede its final flush, including a torn
flush. Matching the checkpoint root means the preceding work is already
published; a different root requires replay. Root offsets zero and eight are
accepted, including the native metadata writer's explicit eight-byte offset.
Multiple markers or data after the marker's flush are rejected.

Supported records create/drop schemas and tables, apply supported table
alterations, select a table, insert rows, delete physical row IDs, and update
primitive columns and their validity. The
shared native catalog decoder preserves supported typed defaults and constraints.
Serialized chunks support flat, constant, dictionary and integer sequence
vectors. Nested column types, bulk block appends, index/view/sequence/macro
records, encrypted or unframed logs, and concurrent checkpoint reconciliation
remain unsupported. Headers with database identifiers and checkpoint iterations
are checked. A tagged log may lag the checkpoint by one generation only when its
marker matches the published root. Legacy headers carry no identity and cannot
establish a file/log pairing. Tagged headers have source-derived contract tests, but the current
v1.3 independent writer emits the legacy v2 header even for v65 checkpoints.

`RecoveryTarget::apply_committed` receives ordered logical changes and exposes
one atomic visibility boundary. The snapshot target preserves restored row IDs,
defers uniqueness and NOT NULL checks until the complete transaction, reconciles
separate value/validity records, and rebuilds derived indexes before returning.
Failed replay or cancellation leaves the target's previous state intact. Normal
SQL mutation continues to use its existing statement constraints.

Recovery code performs no I/O. Repeated read-only opens reconstruct the same
state without changing either file. Writable opens call `Recovery::prepare` to
validate and encode an owned `PreparedRecovery` before any durable change.
`SnapshotFormat::encode_successor` preserves the native database identifier,
advances its generation, and allocates a root different from the previous root.
This last condition prevents an old checkpoint from being mistaken for the
successor when a same-shaped rewrite would reuse its root address.

`RecoveryPublication::Replace` contains the successor checkpoint and a bridge
log. The bridge preserves committed records and adds the successor's checkpoint
marker, so either checkpoint paired with it recovers the same logical state.
The file publisher compares the exact prepared input bytes under its publication
mutex, rejecting stale plans before mutation. It then writes and syncs both
temporary files, renames the bridge onto `.wal` and syncs the directory, installs
and syncs the successor checkpoint, and removes the log and syncs the directory.
The lease is retained throughout. A failure after checkpoint replacement or log
removal reports `CommitUnknown`; reopening reconciles the durable state.

`RecoveryPublication::RetireLog` handles a matching checkpoint marker or a log
with no committed work. It first syncs the current checkpoint and parent
directory, completing a possibly interrupted publication, then removes and
durably retires the log. Both protocols accept a `FileFaultInjector` at named
boundaries before actual I/O. Injection cannot reenter the storage adapter;
process-exit hooks live only in the test worker. Preparation returns the
published checkpoint's physical row identities, including compaction, so a
subsequent logger addresses the new file correctly. Read-only recovery retains
the original physical identities.

Input is capped at 512 MiB per file and one million log entries; chunks are
limited to 65,536 rows, 16,384 columns and 16,777,216 cells, with at most 64
nested vector encodings. Existing 16 MiB serialized string/blob limits apply.
Frame checksumming, record processing, vector decoding and transaction replay
check cancellation; synchronous checkpoint decoding and filesystem calls retain
their existing limits. These bounds do not establish a global memory budget or
streaming recovery. A second recovery/target implementation and recovery
throughput measurements remain open.

## Transaction logging

`FileWal` composes `FileCheckpoint`, `TransactionLog` and the file adapter's
logging operations. `Database::open_logged` and the shell's `--durability wal`
select this bundle. Composition checks the checkpoint family, writable file
append/publication capabilities, successor encoding and writable recovery
preparation before use. The existing checkpoint and memory adapters use the
same transaction publication interface. Logging is currently opt-in.

`Durability::requires_journal` requests capture when a transaction starts.
Successful catalog and row mutations enter an owned `TransactionChange` journal;
failed mutations and no-op catalog operations do not. Journals from transactions
that later abort or roll back are discarded without publication.
`Commit` borrows the acknowledged snapshot, validated candidate snapshot and
optional ordered journal. Its acknowledged state permits maintenance before an
incoming append without making that candidate durable prematurely.
The default memory/checkpoint adapters do not allocate journals. Snapshot
transactions retain their conflict and visibility rules and expose committed
state only after durability succeeds.

`TransactionLog::start` produces a header and owned `LogSession` from the
checkpoint's physical row identities. `LogSession::prepare` is pure and returns
encoded bytes and a successor session; the caller installs that session only
after append succeeds. Native encoding owns schema metadata, logical/physical
append high-water marks and row-ID translations. It reduces each transaction's
row changes to deletes of previously committed rows and surviving new row values,
then emits inserts after deletes. This preserves key updates and transient
inserts while accommodating native readers that stage inserts until the commit
flush. Row-ID translation remains private to the encoder and persists across
commits. Catalog records retain their transaction order.

The native encoder shares binary, type and table/default serialization with the
checkpoint writer. It emits unencrypted v2 logs with the legacy version header,
schema/table records, selected tables, batched inserts/deletes and one commit
flush. Every supported primitive type has checked physical values and validity
masks; strings retain embedded NULs. Updates use delete/insert records, including
indexed columns. The legacy header cannot establish an independent database/log
identity pairing. No encrypted or concurrent-checkpoint log writing is implied.

The file adapter initializes the header through a synced temporary file, rename
and directory sync, before appending transaction data. Appends check the expected
length under the publication mutex and retained writer lease, then append and
sync. A write/sync error truncates to the previous length and syncs that rollback.
Successful rollback is a definite failure with the old session retained; failed
rollback reports `CommitUnknown` and disables further writes until recovery.
No-op journals write no bytes. Abrupt exits are recoverable from complete commit
flushes. Successful return is the acknowledgment boundary.

Reopening first checkpoints recovered work and resets the encoder to that
checkpoint's physical identities. A logging session is limited to 512 MiB and
one million records; exceeding either fails before appending. Scheduling does
not override encoding limits: a single large transaction or an encoder limit
reached during preparation can still return a resource error. Journals, pending
rows and cloned translation maps consume additional memory; this is not a global
byte-budget or scalable MVCC implementation. Serialization checks its context
cooperatively, but automatic maintenance currently uses a background context
and synchronous filesystem calls. Second log and filesystem implementations,
group commit, commit-latency measurements and broader fault campaigns remain
required.

## Online checkpointing

`CheckpointPolicy` requests publication of acknowledged work from committed log
bytes, successful nonempty appends and pending append size. It has no I/O or
clock dependency. `FileWal::with_checkpoint_policy` selects a log-size policy
(16 MiB by default), a commit-count policy, another conforming adapter, or
`None` for explicit maintenance only. The shell exposes positive, mutually
exclusive `--checkpoint-bytes` and `--checkpoint-commits` options with WAL
durability. Size thresholds may be exceeded by one incoming transaction; they
are scheduling decisions, not hard limits.

`Connection::checkpoint()` and bare SQL `CHECKPOINT` call the same transaction
maintenance contract. Explicit transactions reject the operation while
retaining their state. Memory and full-checkpoint durability have no pending
log to publish; read-only durability rejects maintenance. The parser represents
CHECKPOINT as a distinct syntax statement using the upstream token stream, so
comments and string literals retain their grammar. Named and forced checkpoints
remain unsupported. Prepared statements and the typed statement API use the
same operation.

The transaction manager holds its publication lock through preparation and
publication. It does not replace the live snapshot or advance its logical
generation. Existing readers retain owned snapshots, and a writer begun before
maintenance can commit afterward if no intervening logical write conflicts.
Automatic maintenance checkpoints `Commit::before`, then prepares the incoming
append again against the successor log session. New commits wait during this
work; it is not background or concurrent checkpoint reconciliation.

`CheckpointImage` pairs successor bytes with a complete `CheckpointLayout`.
Recovery validates every table definition, live source/destination row mapping,
append high-water mark and value, including exact floating-point bits. Missing
rows, aliased destinations or changed payloads fail before I/O. Native encoding
compacts physical rows while live transactions retain logical IDs.
`LogSession::rebase` composes the old translation with this layout and validates
the result against the acknowledged logical snapshot. It returns a new header
and session; they are installed only after the existing bridge-log publication
protocol succeeds. A header-only log uses the retirement protocol without
rewriting the checkpoint.

Preparation errors, including cancellation and invalid adapter layouts, leave
the session usable. Once maintenance publication returns an error, the writer
requires close/reopen and reports `RecoveryRequired`. The incoming transaction
has not been appended, so it is definitely uncommitted. Header-initialization
uncertainty has the same classification. Failed rollback after an actual append
instead reports `CommitUnknown`. New transaction starts are blocked in both
cases. Explicit maintenance checks its caller's context before publication;
synchronous filesystem operations cannot be interrupted cooperatively.

## Subqueries

`BoundSubquery` owns an immutable logical plan and its scalar, existence or
membership operation. `OuterColumn` identifies a typed position at a positive
lexical query depth. Binding resolves the nearest matching scope, preserves
ambiguity errors and checks grouped outer columns. Inlined CTEs retain their
defining scope by shifting only references outside the CTE's own nested queries.
Validation checks the full nested catalog, schema, comparison types, column
positions and combined plan/expression depth. Optimizer passes visit nested
plans through the same pipeline and validate the complete result after each pass.

`EvaluationContext` separates resource-only constant evaluation from execution
that can access outer rows and nested plans. `PreparedExpression` enumerates
relational dependencies once for an operator or mutation expression. They are
evaluated for the current row before scalar short-circuiting, then retained in a
local frame; membership needles are evaluated once. Scalar subquery cardinality
errors therefore remain errors inside COALESCE and TRY_CAST. Binding validates
all CASE branches, then removes statically unreachable dependencies. EXISTS
removes unused projections, sorts and aggregate values while retaining input
filters, row counts, DISTINCT/OFFSET semantics and their required errors.

`PreparedSubqueries` belongs to one statement and one selected physical planner.
Each immutable logical subplan compiles once on first demand. Cache keys retain
their Arc identities; physical output types must match the logical plan before
opening a stream. Uncorrelated scalar and EXISTS values initialize once within
that statement. Correlated evaluations use fresh streams and borrowed outer-row
frames, and membership requests are evaluated for their current needle. Nothing
is retained across statements or prepared-statement rebinding. Concurrent
connections have independent local state, and the enclosing transaction supplies
all catalog and table reads. Mutations collect their changes before applying
them, preserving statement atomicity and the pre-mutation snapshot.

`DatabaseBuilder::subqueries` and the shell's `--subqueries streaming|materializing`
select the consumer independently of expression evaluation, physical planning
and the outer executor. The streaming adapter requests only enough rows for
scalar cardinality or existence, and can stop at a membership match. The eager
adapter collects the relation before reduction. Both enforce one-column scalar
and membership inputs, NULL-aware comparisons, empty-relation semantics and
owned results. Eager consumption can observe later errors and exceed a row limit
that streaming consumption can satisfy. Neither adapter commits data. Invalid
logical payloads and late cancellation remain errors at the expression boundary;
EXISTS must return a non-NULL Boolean.

`DecorrelateExists` can replace a direct EXISTS/NOT EXISTS filter with a semi/anti
join. Both relations must be plain table scans; the inner predicate must equate
an inner column to a pure, total expression of the immediately enclosing row.
The proof comes from `BoundExpr` and the retained operator adapters. Casts,
potential overflow, effects, deeper captures, local residuals and inner
LIMIT/OFFSET/DISTINCT retain dependent execution. Optional `TableStorage::row_count`
metadata must establish that both visible inputs fit the row budget; unavailable
metadata and single-row outer inputs also retain dependent execution. Metadata
reads cannot scan rows and must include the transaction's own changes. Each
prepared execution rebinds against its current snapshot.

Other shapes still repeat dependent scans per outer row. There is no correlated
result cache, IN membership hash build, spill or parallel subquery scheduling.
General LATERAL references, ANY/ALL and row-valued
comparisons, correlated LIMIT/range arguments, compound grouped captures and
aggregates that bind entirely to an outer query scope remain unfinished. The
latter are rejected explicitly rather than evaluated in the wrong scope. Broader
volatile/external-effect histories and DuckDB planner equivalence require further
conformance work. The local caches are not a global memory budget.

## Execution limits

`QueryResult::rows` owns a fully materialized `RowCollection`, with row-major
values in one contiguous allocation. Indexing and borrowed iteration return row
slices; they allocate no row vectors and perform no query work. Consuming
iteration transfers owned row vectors, and `into_rows` explicitly collects them
when a caller needs `Vec<Row>`. Zero-column rows retain their cardinality. This
keeps result ownership and complete materialization without allocating a separate
vector for every returned row. Batch streaming remains a separate API.

Physical operators open independent local streams. Each next call specifies a maximum batch size, and the boundary validates cardinality and schema. Empty batches are forbidden; exhaustion and errors are permanent. Scans, values, range, filters, projections, limits, distinct and unions advance on demand. Limits reduce upstream demand and avoid opening unused inputs. Filter selection retains immutable vector storage. Pure column projections select owned vector views directly, preserving cardinality even for zero output columns; flat and dictionary payloads remain shared. Identity projections forward chunks through the same checked stream boundaries. Computed projections use the selected expression evaluator. Stream setup validates every declared type and retains only the additional logical validators requested by the type's capability.

`NativePhysicalPlanner::with_scan_filters` selects `ScanFilterStrategy::Fused`
(the default) or `Separate`, recorded in adapter metadata. Fusion applies only
to a filter directly above a logical table scan. `TableScan` delivers an owned
`ScanBatch` of stable row identities and values. Single-row requests retain a
row representation; bulk requests share views of the published columns and
row identities. `ScanBatch::shared` validates an identity range and retains its
backing allocation, so later writes or cursor destruction cannot invalidate it.
Adapters can also supply owned identity vectors through `ScanBatch::new`.
Conversions occur when execution requests columns or collection requests rows.
Fusion validates input schema and
logical payloads before evaluating predicates; physical types are checked by
vector construction. Selected columns retain the input storage. It
retains the separate operators' input demand, ordering, cancellation and terminal
errors; it does not prefetch beyond demand or require a concrete storage adapter.
Other input shapes retain the ordinary filter operator.

`Aggregation` carries canonical group ordinals, an ordered list of grouping
sets, and function or GROUPING outputs. Repeated sets remain independent;
repeated ordinals within a set collapse. Missing group values become NULL,
while GROUPING bits describe set membership independently of stored NULLs.
Each empty set emits a row even for empty input. Frontends and optimizers use
the same metadata and expression validation before physical planning.

`NativePhysicalPlanner::with_aggregation` selects an `AggregationAlgorithm`.
`HashAggregation` uses hashed canonical keys and an optional integer column
algorithm; `OrderedAggregation` uses an ordered index with the same type-adapter
key semantics. Both consume the input
once and retain independent aggregate and DISTINCT state per set and group.
Grouping expressions and each function's arguments/filter are evaluated once
per input row. FILTER controls updates, not group existence. Updates retain
input order; output order is unspecified. Cancellation or failure discards
state before output publication. The row budget bounds retained groups and
each DISTINCT set; byte accounting, parallel combination and spill remain open.
The SQL binder expands GROUPING SETS, ROLLUP and CUBE with bounded nesting and
set counts. A dialect extension produces nested grouping AST nodes through the
parser interface, without rewriting SQL text. See the [grouping evidence](grouping/README.md).

`AggregateFunction::create_grouped_state` optionally supplies an independently
owned `GroupedAggregateState` with contiguous group ordinals, checked growth,
column updates and one owned result per group. The default declines before
input is consumed. Opting in promises total updates without effects for
logically validated arguments and at most `usize::MAX` updates per group;
the adapter must enforce that count before relying on it. Work may interleave
differently between groups and functions while retaining each group's input
order. This permits contiguous state arrays without requiring the executor to
identify concrete functions. A replacement backed by ordinary scalar aggregate
states runs the same interface and SQL callers in the conformance tests.

The integer column algorithm requires total expressions, at most two keys per
set, explicit integer key capabilities from the selected type adapters, and
opt-in functions without DISTINCT/FILTER. Other signatures retain the ordered
row driver. Each set owns a bounded adaptive integer index, with a sparse map
for keys outside the initial dense domain. Argument and key columns are
evaluated once per input batch. `GroupSelection` retains validated destinations
in input order and an optional bounded histogram; functions can count without
permuting argument rows. Constant destinations use the existing global SUM
kernel. Signed SUM inputs through 64 bits fit every i128 prefix under the
checked update-count bound. NULLs, empty states, overflow rejection for broader
inputs, cancellation and output validation remain explicit contracts.

`AggregateState::update_batch` accepts argument columns with explicit cardinality,
including zero-column input for count(*). The default adapter retains scalar
update order; integer sum and count consume columns directly. A single global
aggregate with literal/column arguments uses this contract. Other expressions,
filters, DISTINCT and interleaved aggregates retain their row evaluation order.
Failed states are discarded, and each adapter must preserve NULLs, overflow and
cancellation. These changes address measured C++ regressions; acceptance still
depends on the recorded comparisons.

The integer SUM kernel uses independent checked 64-bit partial accumulators for
flat, non-NULL narrow integers. A partial overflow requests a 128-bit block
reduction; it is not a SQL error. A conservative bound on every accumulator
prefix is checked before using either reduction. HUGEINT inputs, nullable or
selected encodings, and states near the 128-bit boundary keep ordered checked
updates. Cancellation is checked between bounded blocks. No table statistics or
query results are cached by this kernel.

`JoinAlgorithm::open` accepts a validated `JoinPlan` and opens an independent
cursor. Its default collects both inputs and invokes the materialized algorithm.
`HashJoin` accepts equality between pure, total expressions local to each input.
For semi/anti joins it builds right-side equality keys once and probes left
batches through the selected expression evaluator. It retains duplicates and
order on the left, excludes NULL equality matches, and does not open the right
input until the first left batch exists. Build cardinality is checked; owned
selected chunks survive cursor destruction. The retained equality type owns key
semantics. Other hash joins and the nested-loop adapter retain explicit
materialization. Join build work remains visible as blocking delivery in EXPLAIN.

`TypeAdapter::key_representation` defaults to canonical bytes. An adapter may
explicitly promise total integer-identity keys after logical validation;
`BoundType` retains this capability and rejects incompatible physical types at
binding. The semi/anti membership builder uses a compact bitmap for bounded,
dense integer domains, with typed hashing for sparse or wide domains. Bitmaps
are limited to 128 KiB and eight bytes per distinct key. Other adapters retain
their canonical byte keys, normalization, validation, and errors. Build state
is sealed before probing and owned by one cursor; no concrete adapter downcast
or query-result cache is involved.

The exact numeric adapter separately opts into `NumericCoefficient` equality:
unsigned bits and same-type decimal coefficients are injective keys, not signed
SQL order. Compact grouping/join/membership/set/window consumers accept that
capability while alternative adapters retain byte keys. Hash equality joins
also retain immutable right payload chunks, inline singleton matches and bounded
dense indices, falling back to sparse indices for wide domains. Duplicate output
resumes within a row; outer matches and NULL padding retain ordinary semantics.
Whole-batch filter proofs belong to selected expression/type adapters and require
validation before rejection. Physical numeric ordering metadata cannot override
a selected comparator's semantics.

Aggregation consumes batches into group states, and sorting collects its input.
No spilling is implemented. The default pull executor drives chunks into an
explicit result sink; the eager alternative collects before delivery. Ordinary
query results use a collecting sink, while the batch API can consume more total
rows than the intermediate row limit. A sink can finish early, releasing the
cursor and retained state. The eager alternative can discover later errors before
its first delivery.

Row limits cover batches, explicit collections, groups, and distinct sets; they are not a global memory budget. Index construction and table access check cancellation cooperatively. The configured batch size is a default demand, which operators can reduce. These paths still evaluate scalar Value objects and perform representation copies; they do not establish vector-kernel efficiency. Strings, parser work, checkpoint encoding and allocation are not charged to a shared byte budget. Cancellation is cooperative at execution checks, not guaranteed to interrupt parsing, filesystem calls or durable publication.

Function signatures and effects are explicit; boolean operations, CASE and coalesce preserve short-circuit behavior. Arithmetic uses checked integer operations and validates the declared physical result type. Nulls do not match join or unique keys; NaNs compare equal to NaNs and above other floating-point values. Distinct and hash keys normalize NaNs and signed zero.

FLOAT has a distinct `Value::Float(f32)` representation and physical type. FLOAT arithmetic rounds at each operation, FLOAT division returns FLOAT, and sum/average accumulate into DOUBLE. Mixed FLOAT/DOUBLE expressions use DOUBLE. FLOAT precision declarations 1–24 select FLOAT and 25–53 select DOUBLE; invalid precision is rejected. Casts, literal defaults, native storage, ART keys and the private format preserve the declared width. Registered logical types use the separate type registry described below; broader overload rules and the remaining DuckDB type families are still unfinished.

## DATE values and native storage

`Date` owns a checked signed day count from 1970-01-01, with astronomical year
zero representing 1 BC. The finite range is 5877642-06-25 BC through
5881580-07-10. Negative and positive infinity retain DuckDB's INT32 sentinels;
INT32_MIN is reserved for physical NULL slots and rejected by public
construction and private-format deserialization. Calendar conversion uses
400-year Gregorian cycles with signed Euclidean division. Display preserves
BCE and infinities, and shell JSON emits calendar strings.

`DateType` and `DateCast` are ordinary selected type/cast adapters. Comparisons,
canonical equality keys, sorting, grouping, min/max, joins and indexes use the
existing type contracts. SQL typed literals retain explicit bound casts;
assignments and literal defaults use the selected conversion registry. Identity
and NULL casts are registered independently. DATE remains distinct from
integers. DATE +/- INTEGER returns DATE, INTEGER + DATE is symmetric, and
DATE - DATE returns a BIGINT day difference. Finite offset results cannot
overflow into an infinity/NULL sentinel; offsetting an infinity preserves it.
Differences use the stored day counts, including infinity sentinels, matching
the reference engine. Integer-to-DATE conversion and interval/timestamp
arithmetic remain unimplemented.

The text grammar currently supports calendar dates with matching hyphen,
slash, backslash or space separators, one/two-digit months and days, signed
years or a `(BC)` suffix, surrounding ASCII whitespace, `epoch`, and
infinities. It rejects trailing non-calendar text, including timestamp
suffixes. This is a subset of DuckDB's date cast grammar; timestamp-to-date
conversion and release-specific permissive suffix handling remain open.
Long whitespace/year scans check query cancellation in the cast adapter.

Native metadata uses logical type ID 15 and signed 32-bit days. Uncompressed,
constant, RLE and both bitpacking readers restore DATE values before applying
column validity; a physical NULL sentinel in a valid row is corruption.
Checkpoint statistics and defaults preserve the logical type. DATE ART keys
use sign-adjusted big-endian day counts, independently checked by traversing
Rust-written indexes in DuckDB. DATE payloads also round-trip through the
private snapshot format. This expands the supported native file subset; it
does not establish compatibility for other temporal types or arbitrary files.

## Cast selection and typed boundaries

The composition root accepts a `CastRegistry`. Each registered `CastSpec` identifies a source type, target type and conversion mode: implicit coercion, assignment, or explicit conversion. Selection is exact, with no fallback outside the registry. Built-in implicit coercions permit NULL, identity and numeric widening; assignment and explicit modes allow the implemented primitive conversions. Conversion syntax remains a subset of DuckDB: for example, the integer text parsers currently accept trimmed signed digits, but do not yet implement decimal, exponent, hexadecimal or separator syntax. Complete overload resolution and coercion policy remain unfinished.

Binding retains a `BoundCast` in each cast expression. It retains the selected conversion function and bound source/target type adapters, and validates source/target agreement with the typed plan. Immutable bound casts and binary/IN type selections are shared through `Arc` handles. Replacing a registry entry affects subsequently bound expressions only. Adapters promise pure deterministic conversion, own retained configuration, and permit concurrent use. The bound input-NULL policy and output-nullability capability are independent: `Call` can inject a typed NULL into an active UNION member; `may_return_null` can extract an active NULL child into SQL NULL. Both default to ordinary validity-preserving casts. Scalar and batch boundaries still validate physical/logical output and cancellation; integer comparison fusion declines casts that change validity.

`CastFunction::cast_attempt` returns `CastResult`, retaining invalid-input versus
fatal failure origin separately from the public `Error` category. The default
leaf contract classifies only Conversion as invalid input; a family may classify
its own local InvalidInput or OutOfRange explicitly. Source/output validation,
inner-expression errors, cancellation and infrastructure failures remain fatal.
`BoundCast::apply` exposes the original error; `apply_try` suppresses only a
classified invalid-input failure. Composite adapters use child `attempt` calls
without flattening failure provenance. A foreign composite that discards this
origin does not satisfy the contract.

`CastSourceContext` retains extracted-source provenance independently of
`CastBehavior` and `CastMode`. Ordinary attempts keep the original selected
`cast_attempt` entry point; VARIANT leaves use `attempt_with_context` on their
retained child binding. The default `cast_attempt_with_context` delegates to the
existing adapter, so ordinary-only replacements retain their behavior. An
opt-in adapter may distinguish VARIANT inputs without changing source/target
registration, overload ranking, output validation or TRY failure recovery.
Custom-adapter tests exercise both defaults and opt-ins through scalar/batched,
prepared, nested and mutation paths, including fatal resource/validator failures
and whole-VARIANT rather than partial-child TRY results.

The runtime Strict/Try behavior is independent of implicit/assignment/explicit
coercion mode. LIST, ARRAY, STRUCT and UNION propagate Try to children, retaining
partial child NULLs; failed MAP keys or duplicate converted keys reject the whole
MAP. VARIANT extracts through strict children and rejects the enclosing value on
failure. These differences follow the development probes rather than a generic
container rule. Existing ordinary conversion diagnostics remain unchanged except
that invalid converted MAP keys now report Conversion instead of malformed output.

Cast batch defaults preserve scalar row/error order. The selected adapter may
prove total conversion and exact integer preservation for its bound signature;
unknown casts decline those capabilities. Parameterized cast-family lookup uses
borrowed source/target names without allocating lookup strings. Exact pair/mode
registrations still override family selection. Operator candidate scoring uses
fixed-size unary/binary mode storage and retains the same ambiguity rules.

SQL assignments now have explicit conversion expressions before they reach storage. This includes INSERT, UPDATE, CREATE TABLE AS and literal defaults. Vectors and the storage contract reject incorrectly typed values. Snapshot validation checks rows directly without cloning them for conversion, and index replacement validates rows then builds the selected indexes once. Native metadata decoding still uses the documented context-free default conversion helper for the format's primitive constant representation; stored, already typed defaults do not replay configurable SQL casts on reopen.

The query context is created before binding and retained through execution, so literal-default conversions, optimizer constant evaluation and execution share cancellation/deadline state. Binding failures preserve an existing explicit transaction; execution failures abort it. Parsing, file opening and publication still have the limits stated above.

`PrimitiveCast` and `DigitIntegerCast` are ordinary `CastFunction` adapters. The former uses Rust's integer parser and the latter checked digit accumulation, including the asymmetric i128 minimum. To select the latter, replace each desired VARCHAR-to-integer `CastSpec` in `CastRegistry::builtins()` and pass the registry to `DatabaseBuilder::casts`. `Database::adapters()` reports the selected implementations, and `BoundCast::spec()`/`adapter()` expose a retained selection. The same callers exercise both algorithms. Cast specifications can also refer to registered types, including their parameters. `register_type` adds ordinary structural conversions for one supported type instance. Other conversions use explicitly registered pairs; general constructor-based cast rules and overload resolution remain open.

## Scalar operator binding and execution

`DatabaseBuilder::operators` selects an `OperatorRegistry`. Signatures declare
an operator, exact argument types, result type and whether non-NULL arguments
may produce NULL. Duplicate signatures and incompatible replacements fail
atomically. The registry validates arity and bounded metadata at registration;
binding validates every type against the selected type registry and retains
owned adapters. No lookup or default implementation is hidden in execution.

Resolution selects an exact signature first, otherwise minimizes the costs of
available registered casts and rejects ties. `CastFunction::coercion_cost`
exposes ranking independently of cast availability. Built-in target costs
follow DuckDB's priorities for the supported types; a replacement can declare
its own ranking policy. SQL integer literals within i32 range carry INTEGER
metadata; larger signed literals retain BIGINT/HUGEINT metadata. Fitting integer
literals at every supported width can select narrower integer overloads.
That literal information is a binding input, not an extra physical value type.
Prepared API values retain their declared/inferred types; explicit casts remain
available. String-literal context is shared by comparisons and scalar binding;
broader contextual coercion and polymorphic function constructors remain incomplete.

Binding writes the chosen casts into the plan, then stores an immutable
`BoundOperator` with the exact signature and function effects. The evaluator
uses stack argument arrays for unary/binary calls. The bound boundary validates
all arguments before NULL propagation, checks cancellation before and after
the callback, and checks physical/logical output. Malformed output is Internal;
arithmetic overflow is Execution. Conversion, resource and interruption errors
remain distinct. A surrounding TRY_CAST cannot suppress an operand's error.

Numeric arithmetic, unary signs, `/`, `//`, `%`, date arithmetic, concatenation,
and LIKE are ordinary adapters. Integer operations enforce their declared
widths, including division/remainder overflow at minimum/-1. Floating `/`
retains IEEE results; `//` and `%` return NULL for a zero divisor. Comparisons
continue through retained `BoundType` semantics, while Boolean expressions
retain their shared short-circuit rules. Extension types can register operator
signatures without editing the parser, binder, evaluator or optimizer.

The two LIKE adapters match Unicode scalar values with `%` and `_` wildcards.
`DynamicLike` maintains two state rows; `GreedyLike` retains only suffix retry
positions. Both have worst-case quadratic time and cooperate with cancellation
inside their loops. String operator inputs and concatenation output are capped
at 16 MiB; allocations are fallible. These local limits are not a global memory
budget. Explicit LIKE ESCAPE and ILIKE are still unsupported.

The optimizer folds successful literal operator calls only when their retained
effects permit it. Failures remain executable expressions, and volatile or
external operations are not folded. Pure constant defaults also use the
selected operator and expression evaluator, preserving operand widths before
assignment. They serialize as typed constants; dynamic/default-function
persistence and native parsed arithmetic-default decoding remain unfinished.
This replaces the old separate negation/arithmetic/LIKE execution branches.

## Registered type semantics

`DataType` describes logical identity. Extension metadata contains a registered family name and integer, string or child-type parameters. `Value::Extension` holds an owning shared payload and its complete declared type. Metadata and payloads are immutable through shared references; validation occurs when externally supplied values enter a bound plan, table, conversion or operator result. Both use shared allocations so the 64-bit primitive `Value` remains 32 bytes and `DataType` stays at most 16 bytes. A regression test enforces those representation budgets.

A `TypeRegistry` selects an ordinary `TypeAdapter` for each family, including all primitive families. `BoundType` retains the adapter and complete metadata. Its contract checks physical identity, logical validity where required, cancellation, total comparison and canonical equality keys. Adapters declare whether physical representation establishes validity or a logical validator is required. Canonical components are framed, including NULLs, so composite keys are unambiguous. Equal values must have identical keys; key bytes need not sort in SQL order. Resource/error results remain errors. Metadata is limited to depth 64, 4,096 nodes and 16 MiB; individual returned keys and extension payloads are also bounded to 16 MiB. These checks do not constitute global allocation accounting.

`TypeAdapter::write_key` appends through a bounded `KeyWriter` into its caller's
reusable allocation. Adapters cannot inspect or modify earlier components.
`BoundType::append_key` owns NULL framing and lengths, and rolls back the complete
component on errors or cancellation. A failed write remains failed even if an
adapter discards the error. Primitive, DATE and both ASCII implementations use
this same contract. Join probes reuse a scratch key without allocating per row.

`BoundType::for_each_key` validates a complete column before invoking its
consumer, then visits canonical keys in row order with NULLs represented
explicitly. The consumer borrows each key only for that callback and copies keys
it retains. Vector construction supplies the physical-validity proof, avoiding
repeated scalar validation during a join probe. Logical validation, selected
type semantics, resource errors and cancellation remain part of the boundary.

Snapshot transactions own the selected type registry. Custom transaction bundles must supply their registry, and the database builder rejects an independently supplied registry with such a bundle. Binding and execution receive the bundle's selection through the query context. Binary/IN expressions and indexes retain bound type behavior; grouping, DISTINCT, joins and sorting obtain behavior through the same registry. Index lookup retains its construction-time semantics even if the supplied resource context carries another registry. Adapter value semantics may use the context for cancellation/resources, and must not change based on other adapters in that context.

Checkpoint decoding receives the selected registry explicitly. The private format serializes type identity/parameters and payload bytes, then validates them and rebuilds indexes during reopen. Missing families fail explicitly before publication. A different conforming adapter may reopen the same metadata; preserving the family's semantics and payload meaning is its compatibility obligation. The native DuckDB writer rejects extension types because they have no native encoding in this implementation. No live replacement, binary extension ABI, or custom native-file compatibility is implied.

The `ascii_ci(max_bytes)` example preserves ASCII spelling while comparing and grouping without letter case. `MaterializedAscii` normalizes temporary buffers; `StreamingAscii` compares folded bytes incrementally. Both implement `TypeAdapter`, use the same casts, and pass the same SQL/storage callers, including indexed uniqueness and private-format restart with a different implementation. See [the embedding example](../examples/registered_type.rs). It is opt-in and does not change VARCHAR or DuckDB collation semantics. CLI output of extension values currently requires an explicit cast to a supported output type.

This is not the complete DuckDB type system. The [numeric foundation](numeric-port.md)
adds unsigned and decimal domains; the [numeric batch follow-up](numeric-batches.md)
documents their scoped kernels and still-open acceptance checks. Remaining work
includes broader numeric coercion and operator/function resolution, remaining
temporal and nested families, catalog-scoped type DDL and aliases, native
extension metadata and richer client interchange. The SQL type parser currently
accepts integer and string literal parameters for registered families;
programmatic child-type metadata does not establish nested value execution.

## DuckDB file compatibility

The native decoder implements checked block access, checksums, dual checkpoint headers, metadata chains, primitive table catalog entries, row groups, column segments, validity and ten compression families: uncompressed, constant, RLE, bitpacking, dictionary, FSST, ALP, ALP-RD, Chimp, and Patas. The four floating-point codecs support FLOAT and DOUBLE. It accepts the supported subset of storage versions 64–67; version acceptance does not imply support for every feature in that version. Independent fixtures exercise v1.3.0 and historical Chimp/Patas databases from the source tree, including both stored floating-point widths. Unsupported metadata must fail before mutation, including views/macros, nonempty catalog comments, unsupported types and codecs, and unsupported WAL records or checkpoint transitions.

Native segment readers are ordinary adapters selected by a format-owned `DecoderRegistry`. The registry checks supported physical types, exact output cardinality, validity Booleans, cancellation and row limits. Wire IDs belong to the containing format; replacement must preserve that encoding's meaning. Codecs borrow validated block payloads through `BlockSource` and return independent values. Reader replacement does not change checkpoint encoding. Unknown codecs and unsupported type/codec pairs return explicit errors.

`DuckDbFormat::default()` selects the built-in readers. To choose another implementation, construct the registry with `duckdb::compression::decoders()`, call `replace(Arc::new(ScalarBitPackingDecoder))`, and pass `DuckDbFormat::new(registry)` to `FileCheckpoint`. Format and decoder selections appear in `Database::adapters()`. Word and scalar bitpacking differ in bit extraction and use the same file and SQL callers. Shared packed-value validation also serves dictionary, FSST, ALP and ALP-RD readers.

ALP decoding validates reversed group offsets, headers, packed integer bounds and ordered exception positions. It uses the format's fixed 1,024-value groups, decimal constants and multiplication order, then restores exception bits. ALP-RD reconstructs IEEE bits from a dictionary of high bits and packed low bits; exceptions resolve before dictionary access. Patas validates backward references and byte-aligned XOR residuals. Chimp maintains its MSB-first bitstream across group boundaries while resetting the reference ring; legacy rounded byte offsets do not supply bit positions. Both reject references to unavailable prior values. Shared checked metadata views handle reversed offsets and alignment. Integer bitpacking retains signed width wrapping, delta modes and 32-value padding. The current codec contract is full-segment reading only: analysis, encoding selection, partial scans, point fetch, pruning, block ownership/reclamation and append interfaces remain open. File opening still uses a background context, so direct decoder cancellation does not establish cancellable database opening.

The writer emits storage version 64 checkpoints, 256 KiB blocks, uncompressed primitive columns, validity masks, statistics, metadata allocation state, and overflow strings. Primary and unique constraints serialize with their ART trees and allocator state, including composite keys, NULLs, escaped string keys and floating-point canonicalization. Committed deletion masks are applied during decoding while preserving physical row IDs and the append high-water mark, so WAL changes address the correct rows. New checkpoints compact live rows and may assign different IDs. Literal defaults and casts of supported primitive constants decode to typed values and serialize as constant expressions. General expressions and constants with unsupported logical types remain rejected. This preserves the supported defaults’ evaluation, without retaining their original SQL spelling. The private Rust format uses the same catalog model and stores both floating-point widths by IEEE bits with a payload checksum.

Both readers currently materialize the complete checkpoint and table contents. The local file limit is 512 MiB; native overflow strings are limited to 16 MiB. Checkpoint allocation is deliberately simple and wasteful, with many partially filled blocks. Full-checkpoint durability rewrites the file per commit; WAL durability appends changes and checkpoints automatically, explicitly and on reopen. Existing files outside the supported subset are not yet usable in this engine.

## Remaining rewrite requirements

The full source system remains substantially larger than this implementation. Required work includes broader SQL and catalog semantics; complete numeric coercion and persistence, remaining temporal and nested types; index DDL, range and mutation planning; all DuckDB compression and version formats; remaining WAL records, background/concurrent checkpointing, group commit and broader crash testing; streaming join algorithms, asynchronous and parallel execution; memory and spill policies; richer optimizer algorithms; filesystem and interchange adapters including Parquet/Vortex; extension and embedding compatibility; graph/AI interfaces; observability; fuzzing; and workload measurements with agreed acceptance budgets.

No OLAP preservation, OLTP throughput, graph, mixed-workload, extension compatibility, production durability or performance parity claim has been established. See the [workload conformance specification](../specs/testing/rewrite-workloads.md) for the acceptance requirements that remain open.
