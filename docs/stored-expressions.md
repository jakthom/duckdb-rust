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
The selected-expansion total-tree resource repair identified during review is
still open; the maintained proofs do not cover that path or this stored tree.

## Remaining connected path

- Retain expressions in column defaults instead of eagerly evaluating SQL or
  native parsed expressions; preserve backward decoding of existing private
  snapshots. Binding and assignment use the selected statement services.
- Add bounded native parsed-expression codecs using the
  [verified wire inventory](native-function-default-inventory.md), including
  independent release/development fixtures and malformed-field tests. Nested
  constant Value metadata is a separately owned family codec, not function SQL.
- Bind omitted columns and DEFAULT VALUES during statement preparation/rebinding,
  preserving required evaluation order, errors and selected result validation.
- Compose selected expression services explicitly into maintenance/recovery.
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
