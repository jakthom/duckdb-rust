# Grouped, ungrouped, and distinct aggregation

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This documents the C++ implementation, not an existing Rust implementation. Engineering requirements below are derived from the interfaces; test coverage is not a claim of executed tests.

## Component boundaries

Aggregation combines physical strategy selection, grouping structures, aggregate callback state, and result emission. The physical planner chooses among ungrouped, general hash, perfect-hash, partition-aware, and other specialized paths when their prerequisites hold. The same SQL aggregate function can execute through different physical structures.

Sources: [plan_aggregate.cpp](../../../duckdb/src/execution/physical_plan/plan_aggregate.cpp), [aggregate operators](../../../duckdb/src/execution/operator/aggregate/), [aggregate callback API](../../../duckdb/src/include/duckdb/function/aggregate_function.hpp).

## Physical selection and grouping data

Ungrouped aggregation maintains aggregate states without ordinary grouping keys. Perfect-hash selection depends on supported group types and bounded value ranges/bit requirements, not merely on a low estimated group count. Partition-aware aggregation requires a provable relationship between grouping expressions and the input's partition metadata. The planner's `HasSingleValuePartitions` walks supported projections/filters toward a scan and checks the relevant capability; arbitrary input order is not sufficient.

`PhysicalHashAggregate` owns read-only grouped-aggregate metadata, grouping sets, one `HashAggregateGroupingData` per grouping set, and shared DISTINCT collection information. Each grouping has a `RadixPartitionedHashTable` plus optional distinct structures. Separate global/local grouping states own the mutable table and distinct states used during execution.

Sources: [physical_hash_aggregate.hpp](../../../duckdb/src/include/duckdb/execution/operator/aggregate/physical_hash_aggregate.hpp), [radix_partitioned_hashtable.hpp](../../../duckdb/src/include/duckdb/execution/radix_partitioned_hashtable.hpp), [aggregate_hashtable.hpp](../../../duckdb/src/include/duckdb/execution/aggregate_hashtable.hpp).

## Sink, combine, finalize, source

The sink evaluates grouping keys, aggregate inputs, and filters, then updates local/shared grouping state according to the selected implementation. Combine merges local contributions. DISTINCT aggregates have an additional deduplication/finalization path; they cannot be implemented by independently deduplicating each worker and then concatenating the results. Finalization can schedule dependency work before grouped output becomes ready.

FILTER applies to an aggregate's inputs, not necessarily to group existence. The hash-aggregate implementation has a groups-only path for groups whose aggregate inputs would otherwise all be filtered away. Grouping sets additionally synthesize the appropriate absent-group values and GROUPING outputs; those values must not be confused with a source row's ordinary NULL key.

The operator advertises unordered source output. `SetMultiScan` explicitly prevents destructive scanning of its hash table; without that mode, result scanning can destroy table state as it proceeds. A consumer planning to reread aggregate output must arrange retention/multiscan rather than reuse a consumed source blindly.

## Aggregate state ownership

The aggregate callback family defines state size, initialization, update, combine, finalize, and destruction. Function-local allocation and pointer-bearing state need correct cleanup on successful output, early termination, and exceptions. Parallel combine must implement the aggregate's semantics; order-sensitive aggregates cannot be treated as commutative simply because a combine callback exists. Floating-point accumulation can also expose order-dependent numerical differences.

Empty ungrouped input, empty grouped input, all-NULL input, and all-filtered input have different result-shape and value rules. COUNT-style zero results and NULL results from other aggregates must be checked per function rather than normalized by the physical driver.

## Memory and verification requirements

Radix partitioning and temporary-memory reservations support large group sets and spillable execution. High group cardinality, large aggregate states, DISTINCT, and grouping sets can have very different working-set demands even with the same input row count.

Test all selected strategies, repeated groups across chunks/workers, NULL/grouping-set distinctions, FILTER/DISTINCT interactions, empty input, nested/string keys, state destruction, spilling, and multiscan consumers. Compare logical results independently of group output order. Use [configuration](../testing/configuration.md), [component/API](../testing/component-api.md), and [benchmark](../testing/benchmarks.md) harnesses. Windowed aggregates share callbacks but have additional frame contracts described in [windows](windows.md).
