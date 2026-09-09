# Implemented architecture and remaining work

This describes the implementation, separately from the accepted [rewrite principles](../specs/rewrite-principles.md) and the specifications of the C++ source system. The implementation is an operational, incomplete v0. Contracts remain editable; there is no stable Rust ABI or private snapshot format promise.

## Composition and boundaries

[`DatabaseBuilder`](../src/main/database.rs) is the composition root. Callers select trait implementations through owned `Arc` handles. Adapters are fixed for a database instance. There are no downcasts, hidden calls into C++ DuckDB, or runtime subprocesses. The independent DuckDB executable is used only by verification scripts.

| Boundary | Contract and implementation | Replacement evidence |
| --- | --- | --- |
| Catalog | `Catalog` / `CatalogMut`; transaction-local schemas and `TableDefinition` | Snapshot implementation only |
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
| Optimization | `Optimizer`; owned `ValidatedPlan` transformation | Identity and configurable pass pipeline; expression simplification and index selection, with validation after each pass; no cost model |
| Physical planning | `PhysicalPlanner` / `PhysicalOperator` | Native compiler only; immutable shared plans open independent `BatchStream` state; delivery modes are visible in EXPLAIN |
| Join algorithms | `JoinAlgorithm`; supported predicates and SQL join semantics | Hash and nested-loop algorithms run the same tests |
| Casts | `CastFunction` / `CastRegistry` / `BoundCast`; exact source/target/mode selection, retained adapters, checked physical values and errors | Standard-library and checked-digit integer parsers share conformance and SQL/storage paths |
| Scalar operators | `OperatorRegistry` / `OperatorFunction` / `BoundOperator`; overload selection, explicit casts, retained effects and checked results | Numeric/date arithmetic and concatenation; dynamic-programming and greedy LIKE implementations share contracts and SQL callers |
| Expressions | `ExpressionEvaluator` / `EvaluationContext`; typed values, explicit outer rows and relational dependencies, NULL/error and evaluation behavior | Scalar evaluator only; resource-only contexts reject nested query execution |
| Subquery execution | `SubqueryExecutor`; scalar cardinality, existence and typed membership over fresh physical streams | Streaming and materializing adapters share SQL, scope, type, mutation, cancellation and independent file checks |
| Functions | `ScalarFunction`, `AggregateFunction`, aggregate states; registration and signatures | Ordinary registration used by built-ins and test function; no binary extension loader |
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

The optimizer obtains catalog and access capabilities from the same statement snapshot. Its independently selected passes own plan transformations; built-in traversal consumes inputs without copying subtrees. Equality lookup currently accepts conjunctions of column/constant comparisons and retains the complete predicate. It declines expressions whose skipped evaluation could suppress errors or effects. SQL UPDATE and DELETE still scan, and indexes rebuild after every table mutation. Range seeks, expression indexes, index DDL, incremental maintenance, and costing remain open.

`ValidatedPlan` owns an immutable logical plan and borrows its statement's optimizer context. Its public constructor checks the complete plan; there is no unchecked constructor or mutable plan accessor. Optimizers use `rewrite` to consume a plan and validate its replacement. An unchanged plan can retain its validation, while each pipeline pass validates its full output. The runtime accepts the result only for the exact context object that supplied the input, preventing transfer of validation from another snapshot. This removes duplicate runtime/optimizer checks without trusting an adapter's claim that its output is valid. It checks structural and type invariants; preserving the query's meaning and effects remains an optimizer conformance obligation.

The default expression-simplification pass folds successful constant casts and operators whose declared effects permit folding through their retained adapters and removes filters known to be true. Failed attempts remain expressions, preserving error timing for short-circuit branches and empty inputs. Folding preserves declared logical metadata and floating-point bits. Functions with effects are never evaluated by this pass. Expression-child and local-plan-expression traversal preserve ownership and selected adapters; each completed pass still undergoes full plan validation.

Read-only transactions retain their snapshots. A writer conflicts with every intervening successful writer, including disjoint writes. The result is a conservative serializable model with no automatic retries. A committed snapshot is published before becoming visible. Dropping a transaction discards it. Definite publication failure preserves committed state; `CommitUnknown` makes subsequent transaction starts fail until the database is reopened and recovered.

Explicit transaction execution errors abort the transaction and require `ROLLBACK`. Binding errors retain it. Autocommit operates per statement: earlier statements remain committed if a later statement in the submitted SQL fails. Prepared statements retain syntax and rebind catalog and parameters for each execution.

The local file adapter canonicalizes paths, acquires nonblocking OS locks, and retains a process lease. On Unix it uses `fcntl` locks compatible with DuckDB. Separate database instances for the same file are rejected within one process; share a `Database` handle instead. Read-only access advertises no mutation capability. Writable hard-linked files and orphan WALs are rejected. Both open modes can select recovery. `.wal.checkpoint` and `.wal.recovery` sidecars are rejected until concurrent checkpoint reconciliation is implemented.

Ordinary publication writes a uniquely named sibling file, syncs it, renames it atomically, retains the lock on the new inode, and syncs the parent directory. Existing file permission bits are preserved. Direct replacement rejects an active WAL so it cannot discard unrecovered commits. Failed temporary-file creation never removes a preexisting file. Ordinary errors clean up owned temporary files; abrupt process exit can leave unreferenced temporary files. The local adapter has been exercised on macOS; other operating systems are not claimed verified.

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

Supported records create/drop schemas and tables, select a table, insert rows,
delete physical row IDs, and update primitive columns and their validity. The
shared native catalog decoder preserves supported typed defaults and constraints.
Serialized chunks support flat, constant, dictionary and integer sequence
vectors. Nested column types, bulk block appends, index/ALTER/view/sequence/macro
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

This is dependent execution, without decorrelation, correlated-key result reuse,
membership hash builds, spill or parallel subquery scheduling. Work can still
repeat scans per outer row. General LATERAL references, ANY/ALL and row-valued
comparisons, correlated LIMIT/range arguments, compound grouped captures and
aggregates that bind entirely to an outer query scope remain unfinished. The
latter are rejected explicitly rather than evaluated in the wrong scope. Broader
volatile/external-effect histories and DuckDB planner equivalence require further
conformance work. The local caches are not a global memory budget.

## Execution limits

Physical operators open independent local streams. Each next call specifies a maximum batch size, and the boundary validates cardinality and schema. Empty batches are forbidden; exhaustion and errors are permanent. Scans, values, range, filters, projections, limits, distinct and unions advance on demand. Limits reduce upstream demand and avoid opening unused inputs. Filter selection retains immutable vector storage. Pure column projections select owned vector views directly, preserving cardinality even for zero output columns; flat and dictionary payloads remain shared. Identity projections forward chunks through the same checked stream boundaries. Computed projections use the selected expression evaluator. Stream setup validates every declared type and retains only the additional logical validators requested by the type's capability.

`NativePhysicalPlanner::with_scan_filters` selects `ScanFilterStrategy::Fused`
(the default) or `Separate`, recorded in adapter metadata. Fusion applies only
to a filter directly above a logical table scan. `TableScan` delivers an owned
`ScanBatch` of stable row identities and values. Single-row requests retain a
row representation; bulk requests write columns directly from stored rows.
Conversions occur when execution requests columns or collection requests rows.
Fusion validates input schema and
logical payloads before evaluating predicates; physical types are checked by
vector construction. Selected columns retain the input storage. It
retains the separate operators' input demand, ordering, cancellation and terminal
errors; it does not prefetch beyond demand or require a concrete storage adapter.
Other input shapes retain the ordinary filter operator.

`AggregateState::update_batch` accepts argument columns with explicit cardinality,
including zero-column input for count(*). The default adapter retains scalar
update order; integer sum and count consume columns directly. A single global
aggregate with literal/column arguments uses this contract. Other expressions,
filters, DISTINCT and interleaved aggregates retain their row evaluation order.
Failed states are discarded, and each adapter must preserve NULLs, overflow and
cancellation. These changes address measured C++ regressions; acceptance still
depends on the recorded comparisons.

Aggregation consumes batches into group states. Sorting and joins still collect their inputs, and blocking operators expose that delivery mode in EXPLAIN. No spilling is implemented. The default pull executor drives chunks into an explicit result sink; the eager alternative collects before delivery. Ordinary query results use a collecting sink, while the batch API can consume more total rows than the intermediate row limit. A sink can finish early, releasing the cursor and retained state. The eager alternative can discover later errors before its first delivery.

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

Binding retains a `BoundCast` in each cast expression. It retains the selected conversion function and bound source/target type adapters, and validates source/target agreement with the typed plan. Immutable bound casts and binary/IN type selections are shared through `Arc` handles. Replacing a registry entry affects subsequently bound expressions only. Adapters promise pure deterministic conversion, own retained configuration, permit concurrent use, and return owned non-NULL values for non-NULL inputs. The boundary handles NULL propagation and checks physical input/output types and cancellation. Invalid values return `Conversion`; `TRY_CAST` catches only that category. Resource failures, interruption and malformed adapter output remain errors.

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
metadata, and fitting integer literals can select narrower integer overloads.
That literal information is a binding input, not an extra physical value type.
Prepared API values retain their declared/inferred types; explicit casts remain
available. String-literal special resolution and general polymorphic function
constructors are still incomplete.

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

Snapshot transactions own the selected type registry. Custom transaction bundles must supply their registry, and the database builder rejects an independently supplied registry with such a bundle. Binding and execution receive the bundle's selection through the query context. Binary/IN expressions and indexes retain bound type behavior; grouping, DISTINCT, joins and sorting obtain behavior through the same registry. Index lookup retains its construction-time semantics even if the supplied resource context carries another registry. Adapter value semantics may use the context for cancellation/resources, and must not change based on other adapters in that context.

Checkpoint decoding receives the selected registry explicitly. The private format serializes type identity/parameters and payload bytes, then validates them and rebuilds indexes during reopen. Missing families fail explicitly before publication. A different conforming adapter may reopen the same metadata; preserving the family's semantics and payload meaning is its compatibility obligation. The native DuckDB writer rejects extension types because they have no native encoding in this implementation. No live replacement, binary extension ABI, or custom native-file compatibility is implied.

The `ascii_ci(max_bytes)` example preserves ASCII spelling while comparing and grouping without letter case. `MaterializedAscii` normalizes temporary buffers; `StreamingAscii` compares folded bytes incrementally. Both implement `TypeAdapter`, use the same casts, and pass the same SQL/storage callers, including indexed uniqueness and private-format restart with a different implementation. See [the embedding example](../examples/registered_type.rs). It is opt-in and does not change VARCHAR or DuckDB collation semantics. CLI output of extension values currently requires an explicit cast to a supported output type.

This is not the complete DuckDB type system. Remaining work includes broader operator/function overload resolution, decimal, remaining temporal, unsigned and nested families, catalog-scoped type DDL and aliases, constructor-based cast resolution, native extension metadata and richer client interchange. The SQL type parser currently accepts integer and string literal parameters for registered families; programmatic child-type metadata does not establish nested value execution.

## DuckDB file compatibility

The native decoder implements checked block access, checksums, dual checkpoint headers, metadata chains, primitive table catalog entries, row groups, column segments, validity and ten compression families: uncompressed, constant, RLE, bitpacking, dictionary, FSST, ALP, ALP-RD, Chimp, and Patas. The four floating-point codecs support FLOAT and DOUBLE. It accepts the supported subset of storage versions 64–67; version acceptance does not imply support for every feature in that version. Independent fixtures exercise v1.3.0 and historical Chimp/Patas databases from the source tree, including both stored floating-point widths. Unsupported metadata must fail before mutation, including views/macros, nonempty catalog comments, unsupported types and codecs, and unsupported WAL records or checkpoint transitions.

Native segment readers are ordinary adapters selected by a format-owned `DecoderRegistry`. The registry checks supported physical types, exact output cardinality, validity Booleans, cancellation and row limits. Wire IDs belong to the containing format; replacement must preserve that encoding's meaning. Codecs borrow validated block payloads through `BlockSource` and return independent values. Reader replacement does not change checkpoint encoding. Unknown codecs and unsupported type/codec pairs return explicit errors.

`DuckDbFormat::default()` selects the built-in readers. To choose another implementation, construct the registry with `duckdb::compression::decoders()`, call `replace(Arc::new(ScalarBitPackingDecoder))`, and pass `DuckDbFormat::new(registry)` to `FileCheckpoint`. Format and decoder selections appear in `Database::adapters()`. Word and scalar bitpacking differ in bit extraction and use the same file and SQL callers. Shared packed-value validation also serves dictionary, FSST, ALP and ALP-RD readers.

ALP decoding validates reversed group offsets, headers, packed integer bounds and ordered exception positions. It uses the format's fixed 1,024-value groups, decimal constants and multiplication order, then restores exception bits. ALP-RD reconstructs IEEE bits from a dictionary of high bits and packed low bits; exceptions resolve before dictionary access. Patas validates backward references and byte-aligned XOR residuals. Chimp maintains its MSB-first bitstream across group boundaries while resetting the reference ring; legacy rounded byte offsets do not supply bit positions. Both reject references to unavailable prior values. Shared checked metadata views handle reversed offsets and alignment. Integer bitpacking retains signed width wrapping, delta modes and 32-value padding. The current codec contract is full-segment reading only: analysis, encoding selection, partial scans, point fetch, pruning, block ownership/reclamation and append interfaces remain open. File opening still uses a background context, so direct decoder cancellation does not establish cancellable database opening.

The writer emits storage version 64 checkpoints, 256 KiB blocks, uncompressed primitive columns, validity masks, statistics, metadata allocation state, and overflow strings. Primary and unique constraints serialize with their ART trees and allocator state, including composite keys, NULLs, escaped string keys and floating-point canonicalization. Committed deletion masks are applied during decoding while preserving physical row IDs and the append high-water mark, so WAL changes address the correct rows. New checkpoints compact live rows and may assign different IDs. Literal defaults and casts of supported primitive constants decode to typed values and serialize as constant expressions. General expressions and constants with unsupported logical types remain rejected. This preserves the supported defaults’ evaluation, without retaining their original SQL spelling. The private Rust format uses the same catalog model and stores both floating-point widths by IEEE bits with a payload checksum.

Both readers currently materialize the complete checkpoint and table contents. The local file limit is 512 MiB; native overflow strings are limited to 16 MiB. Checkpoint allocation is deliberately simple and wasteful, with many partially filled blocks. Full-checkpoint durability rewrites the file per commit; WAL durability appends changes and checkpoints automatically, explicitly and on reopen. Existing files outside the supported subset are not yet usable in this engine.

## Remaining rewrite requirements

The full source system remains substantially larger than this implementation. Required work includes broader SQL and catalog semantics; decimal, remaining temporal, unsigned and nested types; index DDL, range and mutation planning; all DuckDB compression and version formats; remaining WAL records, background/concurrent checkpointing, group commit and broader crash testing; streaming join algorithms, asynchronous and parallel execution; memory and spill policies; richer optimizer algorithms; filesystem and interchange adapters including Parquet/Vortex; extension and embedding compatibility; graph/AI interfaces; observability; fuzzing; and workload measurements with agreed acceptance budgets.

No OLAP preservation, OLTP throughput, graph, mixed-workload, extension compatibility, production durability or performance parity claim has been established. See the [workload conformance specification](../specs/testing/rewrite-workloads.md) for the acceptance requirements that remain open.
