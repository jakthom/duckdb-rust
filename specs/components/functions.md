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
