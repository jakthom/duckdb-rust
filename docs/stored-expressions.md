# Stored-expression integration in progress

The value-and-expression milestone includes retained DEFAULT expressions through
selected binding, mutation and native recovery. This work is not complete. The
first increment supplies an owned tree and a defaulted binder capability; the
catalog still stores evaluated `Value` defaults and the native reader still
rejects function defaults. No compatibility gain is claimed from this internal
prerequisite alone.

`catalog::expression::StoredExpression` preserves declared literal DataType and
physical Value separately, typed NULLs, explicit/TRY_CAST nodes, function
qualification, aliases, named arguments, operator identity and legacy-versus-
modern argument provenance. It is not diagnostic SQL text, a bound adapter, or a
placeholder for a failed decode. Its current closed-scalar subset remains
revisable as native and SQL integration expose missing semantics.

Complete-tree validation precedes binding callbacks. The current limits are 64
edges of nesting, 16,384 expression nodes and 16 MiB of identifier bytes; selected
type adapters additionally validate literal metadata/payloads and their resource
limits. Validation checks cancellation and bounds the worklist before growing it
for a function's children. It does not establish function availability or effects.

`Binder::bind_stored_expression` uses the supplied statement services. Ordinary
replacement binders default to explicit Unsupported, without a built-in fallback.
SqlBinder retains typed literals and selected explicit casts, then reuses the
ordinary selected scalar binding path. Same-type explicit casts remain cast nodes
instead of becoming literals. Function lookup precedes child function binding.
Binding checks closedness and selected volatile/external effects; execution is
separate. Contextual scalar bind requests can still explicitly evaluate eligible
constant arguments under their existing contract. General default evaluation is
not performed during ordinary tree binding.

The first SQL-binder subset accepts positional, unqualified scalar calls. Named
arguments, qualified calls and stored operator nodes are retained but explicitly
unsupported by that subset, not stripped or silently redirected. Their binding
and SQL capture remain implementation work, along with broader expression forms.

Three contract tests currently exercise both evaluators, serde tree retention,
full-width unsigned and typed NULL literals, Base64 and numeric calls, same-type
cast provenance, lazy COALESCE, deferred conversion failures, selected function
and cast retention after registry replacement, fatal TRY_CAST failures, effects,
missing binder capabilities, malformed payloads, cancellation and tree limits.
They validate bound plans before evaluation. An initial assertion used the wrong
catalog-error spelling; the corrected assertion matches the existing exact error
and verifies outer-function lookup precedence. These are internal contracts, not
end-to-end DEFAULT/persistence evidence. Full integrated checks and exploratory
Kani pass at the [eleventh validation checkpoint](value-expression-progress.md).
The selected-expansion total-tree resource repair identified during that review
is integrated at checkpoint twelve, with dedicated regression tests; the
maintained proofs do not cover that path or this stored tree.

## Remaining connected path

The next internal increment adds `StoredExpressionEvaluator` and the explicitly
composed `SelectedStoredExpressions` bundle. It validates replacement-binder
output as closed/effect-free, applies the selected assignment cast, executes the
selected evaluator once, and validates the physical result against the declared
target. Query contexts can retain this capability; missing composition returns
Unsupported rather than constructing built-ins. Two additional contracts exercise
selected assignment/function/evaluator retention, invalid replacement trees and
results, effects, missing capabilities and cancellation. All five stored-tree
contracts, ordinary workspace check, all-target clippy and coverage pass
(349 files, 3,317 functions, 229 interface methods, no missing instrumentation).
Initial test-only failures were a missing Debug implementation and an attempted
duplicate built-in cast registration; the probe now supplies its own registry.
This increment alone does not change native DEFAULT support. The full Kani
suite will run at the next substantial integrated checkpoint, not be inferred
from the preceding checkpoint's result.

The following integration composes that service in DatabaseBuilder before loading
the default transaction manager, together with selected types and validated
initial settings. Connections retain it with their session settings. Defaulted
contextual durability/format entry points preserve replacement callbacks;
FileCheckpoint, FileWal and native recovery forward the caller context. Four
additional contracts cover startup/session/prepared settings, explicit missing
binder support, unchanged files during read-only recovery, cancellation, startup
configuration failure before durability, and the retained WAL context across
commit, rollback, manual checkpoint and reopen. Nine stored-expression/context
contracts pass, along with check/clippy and instrumentation coverage (354 files,
3,399 functions, 231 interface methods, no missing entries). One initial test
expected `ASC` for an unset setting; the existing default is `ASCENDING`, while an
explicit SET retains `ASC`. The assertion was corrected without engine changes.
The full workspace, tracing and six maintained Kani harnesses now pass at the
[twelfth integrated checkpoint](value-expression-progress.md); none of those
proofs establishes this new context/evaluation path. Native parsed DEFAULT codecs
and internal background decoder/encoder work remain
open; contextual adapter plumbing is not evidence that those paths are finished.

- Retain expressions in column defaults instead of eagerly evaluating SQL or
  native parsed expressions; preserve backward decoding of existing private
  snapshots. Binding and assignment use the selected statement services.
- Add bounded native parsed-expression codecs using the
  [verified wire inventory](native-function-default-inventory.md), including
  independent release/development fixtures and malformed-field tests. Nested
  constant Value metadata is a separately owned family codec, not function SQL.
- Bind omitted columns and DEFAULT VALUES during statement preparation/rebinding,
  preserving required evaluation order, errors and selected result validation.
- Carry the composed expression context into the remaining native codec internals.
  Native decoding must not instantiate registries or execute functions itself.
- Resolve ALTER ADD backfill once for the affected operation and retain it through
  transaction catalog/data snapshots and the logger. Re-evaluating a default at
  each layer can change errors or values. Native WAL must preserve both the
  already computed fill and the retained default expression; independent C++ WAL
  replay also needs the selected closed-expression path.
- Extend exact checkpoint comparison to expression identity and literal bits,
  without SQL re-evaluation or replacing identity with SQL equality. Test empty
  tables, mutations, rollback, recovery publication failures and reopen.

The last items are a provisional implementation direction, not implemented
behavior. The raw unrenderable timestamp DEFAULT witness and independently
produced Base64/calendar function-default failures remain open. Performance and
full-upstream acceptance must be refreshed on the eventual integrated source.

## Raw timestamp DEFAULT: planning boundary investigation

On unchanged pinned development `99063af2bd`, these in-memory CLI probes were
run during the eleventh integrated checkpoint:

```sql
CREATE TABLE t(ts TIMESTAMP DEFAULT make_timestamp(-9223372036854775806));
INSERT INTO t DEFAULT VALUES;
SELECT epoch_us(ts) FROM t;
```

Normal execution fails with `Conversion Error: Date out of range in timestamp
conversion`. Each of the following independent prefixes makes the same complete
workload succeed, returning `-9223372036854775806`:

- `PRAGMA disable_optimizer;`
- `SET disabled_optimizers='expression_rewriter';`
- `SET disabled_optimizers='join_order';`

Explicit `INSERT INTO t VALUES(make_timestamp(-9223372036854775806))` also
succeeds with ordinary optimization. This distinguishes the recorded default
failure from physical timestamp validity and from a universally invalid default
cast. It is not a waiver of the normal development result.

The source-backed explanation is an optimizer/planner-name interaction:
[constant folding](../../duckdb/src/optimizer/rule/constant_folding.cpp) replaces
the call with a BoundConstantExpression; [projection statistics](../../duckdb/src/optimizer/relation_statistics/relation_statistics_helper.cpp)
call `expression.GetName()`, including from the
[join-order relation manager](../../duckdb/src/optimizer/join_order/relation_manager.cpp).
[BaseExpression](../../duckdb/src/parser/base_expression.cpp) uses ToString when
there is no alias, and [BoundConstantExpression](../../duckdb/src/planner/expression/bound_constant_expression.cpp)
renders its Value with ToSQLString. The disabled-pass probes and this call chain
support that explanation; no instrumented C++ stack trace was captured.

The future repair must respect the selected planning/evaluation boundary and
retain the valid physical instant, including explicit inserts and disabled-
optimizer execution. A timestamp-domain restriction or an eager native-decoder
cast would fix a different behavior. The existing default differential remains
failing until the connected implementation addresses it.

## Default demand: creation, backfill and deleted physical rows

Additional in-memory CLI probes on the unchanged release/development pins during
checkpoint twelve distinguish binding from evaluation. Use the deliberately
failing expression `CAST('bad' AS INTEGER)` in each case:

| Operation | Both pinned results unless noted |
| --- | --- |
| CREATE TABLE with that default, then count rows | Success, zero rows |
| ADD COLUMN with that default to a never-populated table | Success, zero rows |
| ADD COLUMN with that default to a table containing one row | Conversion error |
| SET DEFAULT on an existing empty or one-row table | Success, existing rows unchanged |
| CREATE with that default, then an explicit INSERT SELECT producing no rows | Success, zero rows |

Rust at `6f5e6ac` still rejects both the CREATE and never-populated ADD probes
with `Conversion Error: cannot cast "bad" to INTEGER`. These are confirmed
existing eager-default gaps, not failures of the new context transport tests.
The connected implementation must bind/check metadata without eagerly demanding
these casts during CREATE/SET, and demand ADD backfill only when its storage
operation actually requires values. The selected closed-expression service is
an evaluation capability, not permission to call it unconditionally on DDL.

Visible cardinality alone is insufficient. The following complete probes all
produce a conversion error in development:

```sql
CREATE TABLE t(i INTEGER);
INSERT INTO t VALUES(1);
DELETE FROM t;
ALTER TABLE t ADD COLUMN j INTEGER DEFAULT CAST('bad' AS INTEGER);
SELECT count(*) FROM t;
```

```sql
CREATE TABLE t(i INTEGER);
BEGIN;
INSERT INTO t VALUES(1);
DELETE FROM t;
ALTER TABLE t ADD COLUMN j INTEGER DEFAULT CAST('bad' AS INTEGER);
COMMIT;
SELECT count(*) FROM t;
```

```sql
CREATE TABLE t(i INTEGER);
INSERT INTO t VALUES(1);
BEGIN;
DELETE FROM t;
ALTER TABLE t ADD COLUMN j INTEGER DEFAULT CAST('bad' AS INTEGER);
COMMIT;
SELECT count(*) FROM t;
```

Release succeeds with zero rows in the first two cases. In the third it reaches
a commit-time TransactionContext error about another transaction altering the
table. Development remains authoritative; none of these release outcomes is a
reason to skip the demanded default or to change the correctness comparator.

The development source retains a copied parsed default for serialization and
binds a separate expression in
[BindDefaultValues](../../duckdb/src/planner/binder/statement/bind_create_table.cpp).
[AddColumn](../../duckdb/src/catalog/catalog_entry/duck_table_entry.cpp) also binds
the added default during WAL replay. Its
[row-group implementation](../../duckdb/src/storage/table/row_group.cpp) executes
the expression for physical `count` rows, not a scan of visible rows, explaining
why deleted slots can still demand it. The current Rust snapshot retains row-ID
high-water metadata, but how that should preserve backfill demand through live
snapshots, compaction and independent recovery remains implementation work.
Do not treat a passing zero-visible-row shortcut as parity for this path.
