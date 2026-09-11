# Rust implementation map

Source-checked 2026-09-11 at `31a72d8`. This is a navigation map, not a parity
claim. Current work and dependencies live in the [parity backlog](parity-backlog.md);
the [rewrite principles](../specs/rewrite-principles.md) define required replaceability.

## Composition and query flow

[`DatabaseBuilder`](../src/main/database.rs) selects owned adapters for parser,
binder, optimizer, physical planner, execution, expressions, subqueries, scheduling,
configuration, functions/casts/operators/types, transactions, durability and indexes.
The Rust engine does not execute queries by linking to or launching C++ DuckDB;
external reference engines are verification tools.

SQL enters [`DuckDbParser`](../src/parser/mod.rs), which uses the vendored sqlparser
crate, then [`SqlBinder`](../src/planner/binder/mod.rs) resolves a typed statement
against a transaction catalog and selected services. Logical validation precedes
execution and follows optimizer rewrites. [`NativePhysicalPlanner`](../src/execution/physical_plan.rs)
opens per-execution streams. The executor delivers owned batches or materialized
results through the connection, preserving cancellation and statement lifecycle.

| Boundary | Existing implementations / location | Important limit |
| --- | --- | --- |
| Catalog | [schemas/tables and alterations](../src/catalog/mod.rs), [runtime registry](../src/catalog/registry.rs), [runtime identity](../src/catalog/identity.rs), [dependency graph](../src/catalog/dependency.rs), [search path](../src/catalog/search_path.rs) | Snapshots, transaction-local DDL and table plans consume stable identities; current-catalog session search paths resolve tables, while general objects and multiple catalogs remain open |
| Transactions | [copy-on-write optimistic snapshots](../src/transaction/mod.rs) | Every intervening writer conflicts, even disjoint writes |
| Values/types | [type/value definitions](../src/common/types.rs), [registry](../src/common/type_registry.rs) | Broad scalar/temporal/nested foundation; function and consumer coverage incomplete |
| Vectors | [flat/constant/dictionary vectors and chunks](../src/common/vector.rs) | Immutable owned/shared data; not complete native or Arrow vector parity |
| Expressions | [scalar/batched evaluators](../src/execution/expression_executor.rs), [registered functions](../src/function/mod.rs) | Full overload/effect/default semantics remain open |
| Stored expressions | [owned trees](../src/catalog/expression.rs), [capture](../src/planner/binder/capture.rs), [binding](../src/planner/binder/stored.rs), [selected evaluator](../src/planner/stored.rs) | Closed representable default DDL/demand/native lifecycle exists; current-time/named-zone behavior (G04), general catalog objects (G10) and DML constraints/generated columns (G11) remain separate |
| Optimization | [identity/configurable pipeline](../src/optimizer/mod.rs) | Default simplify/equality-lookup/EXISTS passes; no cost model |
| Joins/subqueries | [join operators](../src/execution/operator/join.rs), [subquery adapters](../src/execution/subquery.rs) | Hash/nested-loop and streaming/materializing variants; broader SQL remains open |
| Aggregation/windows | [aggregate](../src/execution/operator/aggregate.rs), [window](../src/execution/operator/window.rs) | Grouping sets and core windows exist; catalog/frame completeness remains open |
| Sorting/recursion | [sorting](../src/execution/operator/order.rs), [recursive streams](../src/execution/operator/recursive.rs) | Alternative algorithms exist; general spill and recursion parity remain open |
| Index/access | [hash/B-tree](../src/execution/index/mod.rs), [storage access](../src/storage/mod.rs) | Equality lookup; indexes rebuild on mutation; SQL DDL/range access unfinished |
| Persistence | [checkpoint](../src/storage/checkpoint.rs), [WAL durability](../src/storage/logged.rs) | Selected native file/log lifecycle, not every object/version/history |
| Formats | [private snapshot](../src/storage/format.rs), [DuckDB native](../src/storage/duckdb/mod.rs) | Versioned values/codecs/identity; general external formats absent |
| Native codecs | [decoder registry](../src/storage/duckdb/compression/mod.rs) | Thirteen readers registered; compressed writer coverage is separate |
| Filesystem | [local publication and locks](../src/storage/filesystem.rs) | No general remote routing, secret manager or encrypted storage |
| Settings | [registry/providers](../src/main/settings/mod.rs), [built-ins](../src/main/settings/builtin.rs) | Four built-in definitions: ordering, NULL ordering, IEEE floating operations and session search path |
| Scheduling/resources | [InlineScheduler/QueryContext](../src/parallel/mod.rs) | Synchronous task execution and row limits; no byte allocator/buffer pool/spill |
| Public consumers | [Rust connection/result](../src/main/connection.rs), [small CLI](../tools/shell/main.rs) | DuckDB C APIs, language clients, Arrow/ADBC and binary extensions incomplete |

## State and failure boundaries

Connections retain transaction snapshots. Results and chunks own their data and
can outlive a connection/database. Prepared statements retain syntax and rebind
on execution; reference-compatible setting/dependency retention still needs work.
Reader snapshots remain alive across publication. Definite commit failure and
unknown/recovery-required outcomes are distinguished by the transaction manager.

Catalog and object IDs are process-local and deliberately absent from durable table
definitions. Object keys stay stable across catalog-version increments; resolved
table bindings retain the observed version separately. Snapshot registries rebuild
fresh identities on reopen, publish versions transactionally, apply schema ownership
through checked dependencies and share one version-bound prepared identity across the
transaction's data and catalog-basis views. Rename-aware identified ALTER/DROP follows
the stable object; drop/recreate makes the old binding stale. Binder, logical and
physical nodes retain table bindings, with exact catalog-version validation for
ordinal-bound reads/mutations and stable identity resolution for DDL. Physical scan
open repeats validation for callers of the public physical boundary, and alternative
frontends obtain handles through `Connection::resolve_table`. The pure search-path
model retains user spelling, duplicates and DuckDB's implicit temporary/default/system
lookup order, but session-setting integration remains open.

Native persistence already has selected versioned scalar/nested checkpoint and
WAL paths, physical row-ID/deletion-mask handling, compatible local locks and
interruption tests. These do not imply incremental storage, universal backward
compatibility, encrypted files, all catalog objects or every concurrent history.
The physical format, catalog/default representation and selected services must
agree before publication. See [implementation notes](implementation-notes.md).

INSERT materialization pulls at most 2,048 source rows at a time and evaluates
omitted default effects column-major over each returned batch. Short natural child
batches remain boundaries, and all rows stay staged until statement success. Snapshot
tables separately retain physical slot order: regular updates remain in place,
while indexed-column and unsupported nested updates leave a deleted slot and append
their replacement. Manual and automatic checkpoint publication encode that order
and reclaim holes only from the successfully acknowledged current snapshot; a
failed checkpoint leaves slots, generation and the durable image unchanged, while
older reader snapshots retain their prior state. ADD classification follows the
development pin when it diverges: one non-TRY cast directly around a constant is
simple in development but not in v1.5.5.

The native default boundary retains absence separately from explicit `DEFAULT NULL`
and carries selected function identity, qualification and argument provenance through
checkpoint and WAL exchange. Unqualified interval syntax retains its VARCHAR-to-
INTERVAL cast, while qualified units retain their cast/function graph. Ambiguous
named-to-legacy conversion is rejected.
Ordinary quoted function identifiers use their identifier value rather than rendered
SQL, and the currently supported `main` qualification maps to the selected built-in
registry. Retained native `main.list_value` uses that same explicit qualification
rule; other schemas/catalogs remain unsupported until routed catalogs exist.
The checked-in development fixture covers FUNCTION checkpoint input. The independent
process gate covers FUNCTION/CASE/predicate/interval checkpoint exchange in both
directions and Rust-origin WAL recovery. The pinned release accepts the checkpoint
cases but its two Rust-origin WAL cases containing FUNCTION nodes retain the upstream
startup limitation.

## Finding the right evidence

Use `test/component/` for feature contracts, `test/contracts/` for shared interfaces,
`test/compatibility/` and `test/data/` for native files, and `scripts/` for independent
reference/harness checks. `test/upstream/duckdb/manifest.json` inventories upstream
inputs; it is not a passing-test report. `specs/components/` describes the C++
source and required behavior, not automatically implemented Rust counterparts.

Run targeted checks during edits and the [chunk sweep](../scripts/verify_chunk.py)
at explicit completion boundaries. Full upstream, API, configuration, platform and
performance acceptance require the additional work in G01/G24 of the backlog.
Use [reference builds](reference-builds.md), [tracing](dev-tracing.md) and
[adversarial testing](sqlite-testing-review.md) for reproduction procedures.
