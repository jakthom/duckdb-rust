# Function binding and execution interfaces

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Function extension interfaces

| Function class | Inputs, callbacks, and outputs | Engineering contract |
| --- | --- | --- |
| Scalar | `scalar_function_t(DataChunk &, ExpressionState &, Vector &)`; bind/local-state/statistics hooks | Result type/cardinality and NULL semantics must match the bound declaration |
| Aggregate | State size/initialize, update, combine, finalize, destructor; bind/statistics/serialization hooks | State layout, parallel combination, empty inputs, destruction and finalization must agree |
| Table | Bind → result names/types and `FunctionData`; global/local initialization; scan → output `DataChunk` | Advertise actual parallelism, projection/filter pushdown, cardinality, partitioning and repeatability |
| Table in/out | Input chunk → output chunks and operator result status | Respect repeated-output/finalization protocol |
| COPY | Bind and global/local state; sink/combine/finalize; optional batch preparation/flush | Own output lifecycle and report failures consistently, including partial writes |
| Cast | Typed conversion implementation and bind data | Implement strict/try conversion and error/NULL semantics expected by the caller |
| Pragma/macro/type constructor/window support | SQL-facing registration and binding/execution support | Match expansion, typing and catalog behavior |
| Compression | Analyze/compress/scan/fetch/serialize callbacks | Round-trip persisted data and metadata under the declared format |

Sources: [scalar_function.hpp](../../../duckdb/src/include/duckdb/function/scalar_function.hpp), [aggregate_function.hpp](../../../duckdb/src/include/duckdb/function/aggregate_function.hpp), [table_function.hpp](../../../duckdb/src/include/duckdb/function/table_function.hpp), [copy_function.hpp](../../../duckdb/src/include/duckdb/function/copy_function.hpp).

Function binding resolves overloads, implicit casts, named parameters and return types. Bound data must support any required copy/serialization lifecycle. Volatility, side effects, error behavior and statistics callbacks constrain optimization; incorrect metadata can cause wrong results even when the evaluation callback itself is correct.

The `core_functions` extension contains a substantial part of the standard scalar/aggregate library. The core `src/function` tree supplies infrastructure and engine-integrated functions. Treat both as part of the SQL implementation when tracing a built-in function.

## Binding versus execution state

Registration publishes function metadata and overloads into the catalog. Binding chooses the overload, resolves argument/return types, and creates `FunctionData` when needed. Global execution initialization creates state shared across workers; local initialization creates worker/task-specific state. These stages have different lifetimes. A bind callback must not assume it is called exactly once per SQL text, because preparation, rebind, serialization verification, and optimizer work can revisit it.

For scalar functions, the bound declaration includes NULL handling and stability/error properties used by the optimizer and expression executor. For aggregates, state initialization, update, combine, finalization, and destruction must agree on allocation and ownership. An aggregate that owns heap memory cannot omit destruction merely because its final SQL value is scalar.

## Table-function capability negotiation

`TableFunctionBindInput` includes parameters, named parameters, input-table shape, binder/context information, and the table reference. Binding returns output names/types and owned bind data. `TableFunctionInitInput` carries projected columns, nested column indexes, projection IDs, and filters. Execution receives bind data plus global/local state and writes an output chunk.

The interface also supports bind replacement by a `TableRef`, direct logical-operator binding, extended statistics, virtual columns, row-ID columns, partition information, repeatability, progress, metrics, and multiple pushdown hooks. Advertising a capability changes what the planner is allowed to remove or rearrange. For example, accepted filter pushdown must either enforce the predicate with SQL-equivalent semantics or retain whatever residual evaluation is required by that hook's contract.

Serialization support requires both serializer and deserializer callbacks. A copied function descriptor does not automatically make its external resource handles or bind data reconstructible. Dependencies should be registered so prepared execution does not continue against released catalog/external state.

Source: [table_function.hpp](../../../duckdb/src/include/duckdb/function/table_function.hpp), [FunctionBinder](../../../duckdb/src/include/duckdb/function/function_binder.hpp).

## COPY and table-in/out boundaries

COPY functions are sinks with output lifecycle responsibilities. Global/local initialization, chunk ingestion, combine, and finalize can be augmented by batch preparation/flush. File creation, successful finalization, and transaction commit are distinct events. A callback failure must propagate with enough information to avoid reporting a partially written output as complete.

Table-in/out functions can produce multiple outputs for one input and can require final output after ordinary input ends. Their status values belong to the physical operator protocol described in [execution](execution.md), not the simpler convention of a stateless scalar callback.

## Verification requirements

Test binding errors separately from execution errors; vary argument types, NULL patterns, constant/dictionary vectors, parallelism, repeated preparation, and serialization. Aggregate tests need empty input, partial-state combination, DISTINCT/FILTER where supported, and cleanup after failure. Table functions need projection/filter pushdown equivalence and accurate end-of-stream behavior. Native registration tests, C v1/v2 callback tests, and extension ABI checks are separate families in [component/API testing](../testing/component-api.md) and [compatibility](../testing/compatibility.md).

## Rewrite selected typed-constant requests

This section is a provisional rewrite interface contract, separate from the
source-system description above. A scalar specialization may request
`ScalarBindArguments::constant_as(index, target, mode)` when closed argument
values determine result metadata, such as DECIMAL rounding precision.

The language frontend owns that request. It must reject missing positions and
arguments with relational dependencies or declared effects before evaluation.
It must use the selected cast registry and expression evaluator, including the
ordinary argument-cast mode and literal-identity policy, and validate the returned
logical value. SQL string literals and fitting integer literals may receive the
SQL frontend's explicit-cast privilege only for an Implicit request. Explicit
and Assignment requests retain their selected mode; typed API parameters are
not SQL literals. Identity conversion follows the same ordinary expression path.

The returned Value is owned, including owned/shared immutable child payloads;
it does not borrow a binder, registry or evaluator. NULL remains a typed request
result that the selected function must interpret explicitly. This is not an
ambient default cast, expression serialization format, or permission to evaluate
volatile functions during catalog loading. Prepared execution must bind against
its supplied parameter values and selected statement services.

Cast, validation, evaluator, resource and cancellation failures propagate.
Malformed adapter/evaluator output is an Internal contract failure, not a
legitimate NULL or recoverable conversion. No fallback may call `Value::cast`
or a private built-in registry. Frontends that do not implement typed constant
requests explicitly return Unsupported; existing metadata and untyped constant
requests remain separate capabilities. The ordinary scalar call path and typed
constant path share one cast-mode selection helper so their policies cannot
silently diverge.

`constant_if_closed(index)` is a separate optional request. SQL returns None
without evaluating row-dependent or effectful expressions, and Some(value) for
closed/effect-free expressions evaluated and validated through selected services.
It does not turn evaluation errors into None. Required and optional requests
share the same dependency/effect classification. Other frontends validate the
index then explicitly reject this capability if they cannot provide it. Known
NULL values remain distinct from unavailable constants and from failed evaluation.

`is_provably_null(index)` is an explicitly speculative NULL-template probe,
separate from either constant request. The SQL frontend shares the operator
template probe, first checks cancellation and closed/effect-free eligibility,
then uses its selected evaluator. Conversion, Execution, OutOfRange and
InvalidInput evaluation failures make this probe unsuccessful; Resource,
Interrupted and Internal failures propagate. Logical output validation occurs
outside this speculative error boundary and remains fatal. The default validates
the argument index then reports Unsupported. This capability does not weaken
required constant evaluation or permit a scalar adapter to swallow failures.

## Rewrite execution argument provenance

`ArgumentProvenance` describes already executed input in its actual batch scope.
Constant is a physical encoding claim, not bound-expression closedness, equal
values, or a one-row relation. Unknown includes ordinary flat/dictionary inputs
and selected frontends that cannot supply this metadata.

The selected scalar callback `evaluate_with_provenance` receives one metadata
entry per already evaluated value. Its default checks cardinality and delegates
the existing callback. The selected expression evaluator has a separate owned
result callback; its default evaluates once through that adapter and reports
Unknown. Replacement evaluators therefore do not inherit another evaluator's
encoding claims. Query contexts default input/subquery provenance to Unknown.
No hook authorizes child reevaluation, changed lazy-branch demand, lost errors
or effects, bypassed logical validation, or a private adapter lookup.

Development's expression executor explicitly converts a non-volatile function
result to CONSTANT_VECTOR when all executed arguments are constant. This is
execution metadata, distinct from the binder's foldability classification.
Source: [function execution](../../../duckdb/src/execution/expression_executor/execute_function.cpp).

`is_closed(index)` is metadata only. SQL uses the same selected bound-expression
dependency/effect classification as the constant requests, with cancellation and
index validation, but does not invoke an evaluator, cast, or function. A closed
expression can still fail when later evaluated; this request cannot establish a
NULL value or discard that failure. Typed parameters are closed in the current
binding, whereas columns, subqueries and expressions with declared volatile or
external effects are not. The returned Boolean is owned statement-local metadata,
not a claim about a later plan's constant-vector encoding. Frontends without this
capability validate the index and return Unsupported. This keeps lazy branches
lazy when a selected function needs provenance rather than a computed constant.

## Rewrite selected combination requests

`ScalarBindArguments::combination(indices)` is a provisional metadata-only
request for a nonempty, strictly increasing list of argument positions. SQL
infers a common type from those retained expressions in source order, with
pairwise normalization and exact full-width literal provenance, then chooses
each mode through the same selected Implicit-if-available, otherwise Explicit
rule used by CASE combination casts. It does not evaluate any child, infer
constants from values, or broaden ordinary implicit conversion.

The owned `ArgumentCombination` contains one result DataType and one CastMode
per requested position. Its validation rejects malformed metadata, a changed
cardinality or Assignment modes. Selected function specializations must validate
and retain the proposal, then expose its modes through ordinary argument binding.
Other frontends validate indices and explicitly return Unsupported unless they
implement this capability; no built-in registry or evaluator fallback exists.

`ScalarFunction::argument_literal_coercion(index)` defaults to true, preserving
ordinary SQL literal privilege. False means the selected argument mode is final,
not that literal inputs are forbidden. Combination specializations disable the
additional rewrite so a selected Implicit cast is not replaced by an Explicit
cast merely because the source is a fitting literal. Existing Explicit and
Assignment policies remain unchanged. A future single typed cast-policy object
can replace these provisional hooks if shared use warrants it.

## Rewrite physical provenance propagation

The built-in evaluators now propagate this provenance while evaluating each
requested row and child once in ordinary order. Literal/parameter projections
and already materialized statement-local subquery reductions supply Constant;
flat VALUES inputs do not, even for one row or repeated equal values. Unknown
and correlated subquery results are not promoted. Selection preserves an
existing constant encoding; selection of equal flat rows stays dictionary.
Materializing operators can erase constant encoding. A result's Constant claim
is a selected-evaluator invariant, like declared effects; all produced physical
and logical values are validated before normalization can discard later payloads.
No SQL equality scan creates or checks the claim.

`ArgumentEvaluation::NullOnConstant` is a separate opt-in execution policy.
Evaluate children in order until an executed Constant NULL establishes the typed
NULL result; validate that child's selected type and the selected output type,
then skip later children and the callback. Already executed children keep their
effects/errors. Unknown/flat NULL does not short-circuit. Eager and FirstNonNull
adapters retain their existing behavior. This policy currently serves the
development date-difference overloads; it does not infer generic NULL handling
from a function name or weaken conversion/validation failure provenance.

Development's projected constant-NULL date-difference calls return NULL before
parsing a bad specifier or evaluating later failing children. Release retains
those errors. Development governs these disagreements. Ordinary VALUES NULL
arguments remain nonconstant and preserve the function's existing dispatch/error
order. The family differential retains both outcomes rather than weakening its
exact result/error comparator.

## Rewrite selected scalar expansion

The provisional `ScalarFunction::expansion` capability returns an optional owned
expression template before ordinary scalar specialization. Default None keeps
the selected ordinary function unchanged. SQL lowers a returned template through
its selected comparison and CASE/type/cast services; the scalar adapter does not
evaluate children or select private casts. An adapter that requires expansion
must explicitly reject ordinary bind/evaluate in a frontend that does not
support expansion. This is not general macro or catalog completeness.

Templates contain a child-before-parent node list with Argument, Null, Equal and
Case nodes; the final node is the root. Reusing a node is another expression
occurrence, not a cached value. Argument occurrences retain the bound expression,
including its literal/parameter identity, selected adapters and effects. CASE
inference remains ELSE-first while child lowering follows source order. Required
comparison/cast failures and lazy branch behavior follow those ordinary paths.

The complete template is validated before lowering or pruning: nonempty shape,
argument bounds, child order and acyclicity, at most 1,024 nodes, depth 64 and
4,096 expanded occurrences. These provisional limits bound metadata expansion,
not scalar-value domains. Validation checks cancellation and rejects malformed
graphs explicitly. A flat owned representation avoids recursively owned foreign
template drop. SQL additionally preflights the actual retained argument subtrees
before cloning, lowering or pruning: a bound-tree budget of 4,096 nodes and depth
128 includes each occurrence's full argument subtree, with one possible ordinary
cast reserved per template child edge. Thus nested selected expansions cannot
amplify already-expanded arguments past the budget. Traversal borrows children,
checks cancellation and bounds its own pending stack. Shared relational plans
are not cloned; their scalar needle is conservatively counted. These provisional
limits do not bound payload bytes or general query memory.
Adapters with their own declared volatile/external effects cannot use this pure
expansion seam. Effects of referenced argument expressions remain intact.

## Rewrite core calendar grids

The provisional temporal adapters register `date_trunc`/`datetrunc` and
`time_bucket` through the ordinary selected function catalog. Truncation keeps
DATE/TIMESTAMP results as microsecond TIMESTAMP and INTERVAL results as INTERVAL;
unit timestamp arguments use selected casts. Calendar periods, fixed timestamp
units and signed interval components retain their distinct algorithms. A closed
DATE/TIMESTAMP truncation specifier is validated during binding, matching the
reference statistics callback; the INTERVAL overload has no such callback.
Actual Constant/Unknown input provenance still governs execution dispatch and
the selected constant-NULL policy. Neither equal VALUES nor one row establishes
constant encoding.

Bucketing separates positive fixed-duration widths from positive pure-month
widths and rejects mixed month/day/time widths. Default origins are Monday
2000-01-03 for fixed widths and 2000-01-01 for month widths. Interval offsets and
temporal origins are distinct overloads with retained bound metadata, including
their different classification/NULL/infinity demand order. Development adds
TIME overloads with midnight wrapping; release lacks those overloads. Calendar
and clock arithmetic is deterministic and does not consult the host timezone.
ICU timezone behavior, statistics/planning parity, diagnostics, native defaults
and performance remain independently measured obligations.

Sources: [truncation](../../../duckdb/extension/core_functions/scalar/date/date_trunc.cpp)
and [bucketing](../../../duckdb/extension/core_functions/scalar/date/time_bucket.cpp).
The pinned development fixed-unit truncation kernel uses unchecked multiplication
at the lower timestamp boundary. The rewrite explicitly models the observed
modular result without Rust overflow or undefined behavior; retained release
errors remain disagreements, not a reason to change correctness authority.
