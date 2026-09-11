# Expression execution

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Expressions and allocation

`ExpressionExecutor` evaluates bound expressions against chunks and maintains expression/local-function state. It supports vectorized computation and selection paths. Join hash tables, aggregate hash tables, row operations and sorting have specialized representations because their access patterns differ from projection-style vector evaluation.

Memory is divided among general allocators, arenas, buffer-managed blocks, operator temporary allocations, and retained result/collection storage. Buffer-managed memory can be pinned, unpinned, evicted, or spilled according to its owner and backing storage. A configured buffer/memory limit should not be interpreted as an unconditional bound on every allocation in the host process.

Sources: [expression execution](../../../duckdb/src/execution/expression_executor/), [allocators](../../../duckdb/src/common/allocator/), [arena allocator](../../../duckdb/src/storage/arena_allocator.cpp), [temporary memory manager](../../../duckdb/src/storage/temporary_memory_manager.cpp), [column collections](../../../duckdb/src/common/types/column/), [row operations](../../../duckdb/src/common/row_operations/).

## Bound-expression execution boundary

`ExpressionExecutor` accepts bound `Expression` objects rather than SQL syntax trees. Physical binding resolves column references into input positions before execution. The executor keeps borrowed expression pointers, a borrowed current `DataChunk`, and owned per-expression execution states. It cannot outlive the expressions it evaluates. Its interface deletes move construction, so relocating an enclosing object must respect that constraint.

`Execute(input, result)` evaluates a set of expressions into output columns. The one-expression overloads write into a `Vector`. A selected `ExecuteExpression(input, result, sel, count)` evaluates only selected rows and writes to those same row indexes in the result: the output is not implicitly compacted to indexes zero through count minus one. `SelectExpression` instead creates true and optionally false selection vectors for filtering.

Source: [expression_executor.hpp](../../../duckdb/src/include/duckdb/execution/expression_executor.hpp), [expression executors](../../../duckdb/src/execution/expression_executor/).

## Evaluation state and semantics

Initialization creates the state tree needed by constants, input references, parameters, functions, CASE, conjunctions, operators, and lambdas. Function execution can retain bind data and local execution state through the bound expression/state machinery. The current input supplies row cardinality and the columns addressed by references; scalar evaluation with no input is a different use case.

CASE and Boolean selection must preserve SQL NULL behavior and only evaluate the rows required by their selected branches. This matters for expressions that throw on some inputs and functions with side effects or volatility. Selection is an execution mechanism, not permission for the optimizer to change which semantically required evaluations occur. The function's metadata and bound expression properties constrain such rewrites.

`EvaluateScalar` folds an eligible expression into a `Value`; `TryEvaluateScalar` reports failure without propagating the evaluation exception. The `allow_unfoldable` parameter is explicit. A caller cannot infer that an arbitrary expression is safe to execute during planning simply because it has scalar output. Prepared parameters and context-dependent functions also complicate reuse of previously evaluated values.

Development's scalar-function binder selects an overload before examining
constant NULL arguments and before argument coercion. For default NULL handling,
it replaces such a call with a constant: the complete declared return type when
available, otherwise SQL NULL. BLOB and VARCHAR concatenation use the latter
template behavior. Release resolves these selected cases to INTEGER instead;
development governs correctness. SQL NULL's type spelling is `"NULL"` (distinct
from the value spelling `NULL`). Source:
[function binder](../../../duckdb/src/function/function_binder.cpp).

The provisional Rust operator contract exposes selected constant-NULL result
metadata independently of the runtime signature and preserves it on replacement.
Only closed, effect-free input evaluation can establish NULL; unsuccessful data
conversions do not establish it, and infrastructure/adapter errors remain errors.
The binder does not discard expressions declaring volatility or external access.
This is not a general nullability inference or permission to change ordinary
fixed-type arithmetic results. CTAS still normalizes SQL NULL at storage boundaries.

## Ordered collection literal binding

Development lowers `MAP {key: value, ...}` to the selected `map` function with
ordered key/value `list_value` arguments. Template inference visits arguments
left to right, skips later untyped NULLs, and retains a binding when the next
template type is exactly equal. String-literal identity is therefore observable:
`['1', NULL, '2', 3]` infers INTEGER[], but `[NULL, '1', 3]` rejects concrete
VARCHAR/INTEGER combination. Explicit VARCHAR casts, columns and parameters
must not gain this literal privilege. Common-type proposals remain selected
registry behavior; combination casts do not widen ordinary function overloads.

Integer-literal identity also includes the literal value. `[1, 2::TINYINT]` and
`[1, 1, 3::TINYINT]` infer TINYINT[], while `[1, 2, 3::TINYINT]` infers INTEGER[].
These source observations require a selected literal-aware inference contract;
they are not permission to bypass a registered type adapter with a hardcoded
width rule. The initial Rust MAP/string-literal slice does not implement this
integer-literal distinction.

The follow-up Rust sequence binder now carries the existing literal-provenance
hint through ordered inference and calls the selected integer-literal common-type
hook. Repeated equal values with equal underlying types retain the hint; unequal
literals, casts, computed expressions, parameters and actual type combinations
do not. Later untyped NULLs preserve the hint, whereas an initial NULL combines
with the first integer literal to produce a concrete type. No expression is
evaluated to discover literal identity. Assignment and ordinary scalar-function
coercion policies remain separate from this collection-template context.

CASE result inference has a different ordered policy: children bind in source
order, then the ELSE type combines with each THEN type in source order. Every
pair invokes normal selected combination, so repeated integer/string literals
and later NULLs normalize their pseudo-type identity. Collection shortcuts must
not leak into this path. The provisional rewrite shares ordered inference with
an explicit context, preserving literal provenance and selected type/cast
services without evaluating result branches. Pruned CASE expressions still
retain CASE identity for enclosing function overloads. Source:
`src/planner/binder/expression/bind_case_expression.cpp` and the selected
combination rules referenced below.

MAP constructor NULL/duplicate keys are invalid input. Converted MAP keys also
need validation, but the cast records its own rejection provenance so TRY_CAST
can NULL the entire result for an invalid or duplicate converted key without
swallowing child validation, cancellation or infrastructure failures.

Sources: [MAP lowering](../../../duckdb/src/parser/peg/transformer/transform_expression.cpp),
[template inference](../../../duckdb/src/function/function_binder.cpp),
[combination rules](../../../duckdb/src/function/combine_types_rules.cpp),
[MAP validation](../../../duckdb/src/common/vector/map_vector.cpp),
[MAP casts](../../../duckdb/src/function/cast/map_cast.cpp).

## Rewrite ordinary-comparison constant NULL boundary

Development binds the six ordinary comparisons as scalar functions with default
NULL handling. Its function executor visits children in order and returns a
constant NULL when an executed child has CONSTANT_VECTOR encoding and is NULL,
before evaluating later children. This also governs the equality inside NULLIF's
CASE expansion. It is not a rule for DISTINCT comparisons, AND/OR, observed equal
values in flat/dictionary columns, or bind-time closedness. Sources:
[comparison registration](../../../duckdb/src/function/scalar/comparison/comparison.cpp),
[function execution](../../../duckdb/src/execution/expression_executor/execute_function.cpp).

The rewrite consumes the selected evaluator's actual execution provenance in
scalar, column, dictionary-cache and predicate paths. A right constant NULL may
produce a constant result only after the left operand's required evaluation and
validation. The selected operand and output metadata remain checked; malformed
logical output is Internal while infrastructure and cancellation failures remain
fatal. NULL payload validity itself is universal at the BoundType boundary.
An ordinary replacement evaluator keeps Unknown unless it explicitly supplies
provenance. Dictionary caching preserves the complete root result's provenance
and validates every produced value before constant normalization; equal values
do not establish the claim. No NULLIF-only shortcut or private comparator exists.

## Ownership, errors, and performance

Result vectors may reference input data when an expression is a simple reference. Callers must retain underlying buffers if results escape input reuse. Expression executors typically belong to local operator/task state; sharing mutable evaluation state between workers is not implied by sharing an immutable physical expression tree. Scratch vectors and selections should be reused across chunks while their sizes and validity are reset correctly.

Errors must retain the difference between invalid SQL input, conversion/arithmetic failure, interruption, and internal invariant failure. Catching an evaluation exception and silently producing NULL changes SQL semantics except where the SQL operation explicitly requests that behavior.

## Verification requirements

Test scalar and vectorized paths, repeated prepared execution with new parameters, nullable inputs, selected row indexes, CASE branches containing error-producing expressions, and volatile functions. Re-run representation-sensitive cases with vector verification modes. The [optimizer](optimizer.md) requires equivalence under its rewrites, while [SQLLogicTest](../testing/sqllogictest.md) and native [component tests](../testing/component-api.md) provide result and invariant oracles.
