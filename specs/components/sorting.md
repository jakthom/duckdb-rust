# Sorting, Top-N, and ordered output

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This documents the C++ implementation, not an existing Rust implementation. Engineering requirements below are derived from the interfaces; test coverage is not a claim of executed tests.

## Responsibility and source map

Sorting evaluates ordering expressions and emits rows according to bound direction, NULL placement, and type/collation semantics. Payload projection and output batching must preserve the association between each key and its row. Full ordering, bounded Top-N, and insertion-order preservation are different mechanisms.

| Component | Role | Source |
| --- | --- | --- |
| `PhysicalOrder` | Operator-level sink/source adapter | [physical_order.cpp](../../../duckdb/src/execution/operator/order/physical_order.cpp) |
| `Sort` | Shared sorting execution interface | [sort.hpp](../../../duckdb/src/include/duckdb/common/sorting/sort.hpp) |
| Sort strategies/runs | Full/natural/hashed strategies and run merging | [sorting interfaces](../../../duckdb/src/include/duckdb/common/sorting/) |
| `PhysicalTopN` | Bounded ordering with limit/offset and optional dynamic filter | [physical_top_n.hpp](../../../duckdb/src/include/duckdb/execution/operator/order/physical_top_n.hpp) |

## Full-order lifecycle

`PhysicalOrder` stores bound order nodes, projected payload columns, and an index-sort flag. Its global sink state owns a `Sort` instance and the sort's global state. Local sort sink state is created lazily on first input. A worker receiving no input therefore has no local sort state to combine; the adapter explicitly handles that case.

Sink, combine, finalize, progress, and source operations delegate to the sort abstraction with their corresponding state and interruption context. The source retains a reference to the same `Sort` object held by the sink state, so the sink owner must remain alive until output consumption ends. Sorting work can include forming and merging runs under memory pressure; source readiness follows finalization rather than the last input chunk alone.

`PhysicalOrder` advertises `FIXED_ORDER` while allowing parallel source execution. Order-aware partition/batch metadata is what makes those properties compatible; consumers must not concatenate whichever worker result arrives first and assume it is sorted. The operator rejects partition requirements that demand arbitrary partition columns it cannot provide.

## Top-N contract

`PhysicalTopN` stores order keys, limit, offset, and optional shared dynamic-filter data. It retains the candidate set needed to produce the requested ordered slice and combines local candidates before final output. Offset contributes to the amount of relevant input retained; a large offset can remove much of the benefit of a small limit.

A dynamic boundary filter can reduce upstream work only when its comparisons conservatively preserve every row that could enter the final result. NULL placement, ascending/descending direction, multiple keys, and ties constrain that proof. Top-N must return a result consistent with full ordering plus the same limit/offset, allowing only differences that SQL leaves unspecified among exact ties.

Source: [physical_top_n.cpp](../../../duckdb/src/execution/operator/order/physical_top_n.cpp).

## Ordering and ownership invariants

Sort keys encode comparison behavior, not merely the raw bytes of each logical value. Variable-length and nested payloads must retain backing storage through run creation, spilling, merge, and output. A selected output chunk must not reference a temporary buffer already recycled by the next merge step.

An ORDER BY that leaves ties unresolved does not imply a stable total order. Test cases requiring repeatable row sequence should add sufficient tie-breaking keys. Likewise, a scan that happens to return insertion order is not a replacement for an explicit SQL ordering requirement.

## Verification and measurement

Check ascending/descending combinations, NULLS FIRST/LAST, duplicates, non-ASCII/collated strings, nested keys, empty input, short batches, projected-away sort expressions, large offsets, external sorting, and cancellation. Compare Top-N with full-sort-plus-limit on the same data. Exercise parallel ordered collectors and alternate vector sizes to expose batching errors.

[SQLLogicTest](../testing/sqllogictest.md) must use order-sensitive expectations when testing sorting itself; sorting expected/actual values would erase the feature being tested. [Configuration](../testing/configuration.md) covers memory/parallel variants, while [benchmarks](../testing/benchmarks.md) separates latency and memory/I/O effects. No complexity or throughput guarantee is inferred from a strategy's name alone.
