# Binding regression follow-up

This follows the numeric foundation in `760fd9a`. Full DuckDB correctness,
native-file and performance parity remains open. Development is the correctness
authority; performance must match the faster pinned C++ reference per workload.
The [acceptance specification](../specs/testing/parity.md) is unchanged.

## Engine corrections

Direct `INSERT ... VALUES` expressions now receive assignment casts to their
destination columns before any cross-row common-type inference. An integer zero
inserted into VARCHAR alongside a decimal remains `0`, not `0.00`. Reordered
column lists, decimal rounding and atomic failure of multi-row inserts retain
their destination semantics. Parenthesized direct VALUES receives the same
treatment. SELECT inputs, set-operation branches and a source-level WITH clause
retain ordinary query-level common coercion; they do not inherit target types.
The selected cast registry still owns every conversion.

SQL relation namespaces now retain separate schema/name components and physical
column ranges inside the binder. Repeated short names and aliases do not fail
merely because two relations are joined. A column reference must resolve
uniquely; a qualified wildcard must identify exactly one relation. Unqualified
wildcards validate their qualified source references, including the source keys
of a FULL JOIN's merged USING expression. Explicit aliases end the original
table namespace. Quoted dots remain identifier contents, not schema separators.
Three-part schema/table/column references and correlated outer references use
the same scope lookup. No executor or public plan-field interface changed.

These are scoped corrections, not complete name-resolution or INSERT support.
Catalog-qualified/nested-schema names, schema-qualified mutation expressions,
the parser's parenthesized INSERT without a column list, DEFAULT expressions
inside VALUES and complete diagnostic wording remain outside this increment.

## Independent assertions

The final session campaign passes
**92 SQL records against each reference**, with unchanged expected results and
errors. It records production Rust source/binary identities, both pinned C++
identities, selected adapters and every assertion outcome. The initial
90-record campaign is retained; the final
campaign adds the source-level WITH coercion boundary.

| Corpus | Records | Origin |
| --- | ---: | --- |
| `relation-names.test` | 32 | New schema/CTE/alias, wildcard, correlation and quoted-identifier assertions |
| `numeric-values-assignment.test` | 17 | New direct/query-level coercion, rounding and atomic-error assertions |
| `upstream/cte-schema-regression.test` | 4 | Byte-exact `test/sql/cte/cte_schema.test` |
| `numeric-values-regression.test` | 6 | Byte-exact `test/issues/rigger/test_536.test` |
| `using-chain.test` | 19 | Byte-exact upstream USING chain |
| `nested-except.test` | 7 | Byte-exact upstream nested EXCEPT |
| `window-binding.test` | 7 | Byte-exact upstream window binding |

The original CTE fixture declares `query II` but returns VARCHAR values. The
C++-compatible session/upstream runner accepts that original assertion; the
local component runner additionally enforces numeric metadata for `I` and
initially rejected it. The unchanged copy is retained in `test/sql/upstream/`
and run explicitly by the session campaign, not silently rewritten to `TT`.
The new name-resolution corpus asserts the actual text types in local tests.
This remaining difference between harness contracts is not full harness parity.

The numeric component regression runs 12 evaluator/optimizer/batch compositions;
the 32-record namespace contract runs 24 subquery/executor/optimizer/batch
compositions. Existing USING, set and window conformance tests remain in the
workspace suite. The serial full upstream refresh
and journal retain all 5,638
source-file outcomes: **313 passed**, 1,877 failed, 3,437 unsupported, eight
timeouts and three incomplete files. The 17,018 passed records include prefixes
of failed files. Eight files newly pass relative to the numeric checkpoint and
no previously passing file is lost, including the two regressions above.

The initial refresh and its
journal are retained as diagnostic
evidence only. A concurrently running numeric-reference command replaced the
shared worker executable; that report is explicitly marked as having invalid
single-binary provenance. The serial rerun used an otherwise idle build/worker
environment. Two apparent new timeouts in the initial run passed on the serial
rerun. Do not run campaigns that rebuild a shared worker concurrently.

## Verification and remaining performance work

`cargo dev coverage` reports no missing instrumentation.
`python3 scripts/verify_kani.py` with pinned Kani 0.67.0 verifies all three
maintained harnesses, with zero failures. The final run takes approximately
3.2 seconds for each window proof and 88.4 seconds for packed-size arithmetic.
The earlier checkpoint also passed and preceded the source-level WITH boundary
correction. Warnings about foreign functions/caller locations were not reachable
proof failures. These proofs cover packed sizes and window bounds, **not the
new binding/coercion behavior**; no new formal-coverage claim is made.

No performance campaign was run for the binding-only checkpoint. The retained
numeric measurements still identify seven
failing workloads out of eight; they are earlier source evidence, not a current
performance acceptance result. That checkpoint identified batched unsigned/decimal
aggregation, grouped states, filtering, join keys and windows as the next work,
with NULL, overflow, selected-vector and alternative-adapter conformance required.
The subsequent [numeric batch report](numeric-batches.md) now records a passing
34-workload matrix and fresh SQL/native/full-upstream evidence for that later
source. These binding-only source identities remain historical.
