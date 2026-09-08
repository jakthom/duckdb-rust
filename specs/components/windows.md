# Window planning, frames, and execution

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This documents the C++ implementation, not an existing Rust implementation. Engineering requirements below are derived from the interfaces; test coverage is not a claim of executed tests.

## Planning and operator decomposition

Window functions preserve input rows while adding values computed over partitions, orderings, and frames. They are not ordinary GROUP BY aggregation. Physical planning groups compatible windows, selects blocking, streaming, or partition-aware execution where eligible, and restores the requested output column order with projection when necessary.

`PhysicalWindow` assumes its functions have compatible common partitioning/ordering. It owns the bound expression list, an order-expression index, partition information, and an order-dependence flag. It is a sink/source operator with parallel source tasks. Sink parallelism is disabled for its order-dependent case; this flag is not equivalent to whether the SQL text contains any ORDER BY anywhere.

Sources: [plan_window.cpp](../../../duckdb/src/execution/physical_plan/plan_window.cpp), [physical_window.hpp](../../../duckdb/src/include/duckdb/execution/operator/aggregate/physical_window.hpp), [physical_streaming_window.hpp](../../../duckdb/src/include/duckdb/execution/operator/aggregate/physical_streaming_window.hpp).

## Function registration and binding

This revision has catalog-registered window functions with binding, blocking, streaming, serialization, and registration interfaces. The source README explains that built-ins use this API while retaining legacy serialization handling for compatibility. New window functions are intended for behavior that cannot be expressed simply as a windowed aggregate.

Binding capability flags govern DISTINCT, FILTER, argument ORDER BY, frame exclusion, and IGNORE/RESPECT NULLS. A function can reject unsupported modifiers before execution. Its bind input can include both frame-order expressions and argument-order expressions; these are different orderings and may require different validation. Streaming callbacks are an all-or-nothing capability rather than a single flag that magically converts a blocking implementation.

Source: [window function API description](../../../duckdb/src/function/window/README.md).

## Blocking execution and frame state

Blocking execution collects/partitions input, establishes the necessary ordering, prepares shared expressions and collections, and evaluates window functions over appropriate row ranges. The implementation separates window executors, boundary state, collection/shared-expression handling, and aggregation strategies such as constant, segment-tree, distinct, custom, and naive paths.

A function requests the boundary locations it needs: partition begin/end, peer begin/end, valid-order begin/end, and frame begin/end. Computing peer boundaries or searching order values can be expensive, so requesting only needed boundaries matters. Correctness requires preserving the distinction among a partition, a peer group, and the current frame.

ROWS-based bounds refer to row positions; RANGE and other supported frame forms use different semantics. Frame exclusion, NULL handling, argument ordering, and DISTINCT can change which values are actually fed to an aggregate/value function. Reusing a cached result across rows is valid only if the relevant frame and arguments are equivalent.

Sources: [window implementations](../../../duckdb/src/function/window/), [physical_window.cpp](../../../duckdb/src/execution/operator/aggregate/physical_window.cpp).

## Streaming and ownership requirements

`PhysicalStreamingWindow::IsStreamingFunction` participates in eligibility checks. A function needing arbitrary future rows or an unbounded retained partition cannot be treated as a simple per-chunk map. Streaming state must carry precisely the history/lookahead required by its supported frame; input chunk boundaries are not SQL partition boundaries.

Blocking state can retain columns, evaluated expressions, ordering structures, and aggregate states beyond a task's input call. Those owners must survive parallel evaluation and be reclaimed on normal completion, error, and cancellation. Partition-aware scheduling must not split a logical dependency without transferring its required state.

## Verification requirements

Test partition and peer boundaries across chunks, ties, NULL ordering, empty/single-row partitions, frame endpoints, exclusion, DISTINCT/FILTER, argument ordering, IGNORE NULLS, LEAD/LAG offsets, ranking, and windowed aggregates. Compare optimized/streaming paths with an eligible general path or explicit reference query. Include large partitions with low memory and multiple incompatible windows in one SELECT.

Use [SQLLogicTest](../testing/sqllogictest.md), [configuration variants](../testing/configuration.md), and [component/API harnesses](../testing/component-api.md). Extension-defined windows additionally require binding-capability, serialization, and callback-lifecycle tests. [Aggregation](aggregation.md) and [sorting](sorting.md) cover shared machinery but do not replace frame-semantic tests.
