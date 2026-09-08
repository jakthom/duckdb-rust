# Join planning, hash tables, and join execution

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This documents the C++ implementation, not an existing Rust implementation. Engineering requirements below are derived from the interfaces; test coverage is not a claim of executed tests.

## Responsibility and implementation map

Join planning chooses a physical algorithm from predicates, join semantics, estimated cardinalities, settings, and special relational forms. Join execution must preserve row multiplicity, NULL semantics, residual predicates, and unmatched-row behavior. These are separate responsibilities from the optimizer's choice of join order.

| Component | Responsibility | Source |
| --- | --- | --- |
| Comparison-join planning | Eligibility and algorithm selection | [plan_comparison_join.cpp](../../../duckdb/src/execution/physical_plan/plan_comparison_join.cpp) |
| Hash join | Build, probe, external/remaining output, projections | [physical_hash_join.hpp](../../../duckdb/src/include/duckdb/execution/operator/join/physical_hash_join.hpp) |
| Join hash table | Key/payload storage, lookup and partitioned state | [join_hashtable.hpp](../../../duckdb/src/include/duckdb/execution/join_hashtable.hpp) |
| Other algorithms | Nested-loop, piecewise merge, IEJoin, ASOF, positional, cross-product | [join operators](../../../duckdb/src/execution/operator/join/) |
| Correlated/recursive coordination | Delim joins and recursive CTE key joins | [join interfaces](../../../duckdb/src/include/duckdb/execution/operator/join/) |

## Algorithm selection contract

Equality predicates can enable hashing; range predicates can enable merge/range algorithms. The planner additionally constrains eligibility by join type and cardinality thresholds. Small-input thresholds can favor nested-loop execution. ASOF and positional semantics are specialized contracts, not synonyms for an ordinary inner equijoin. The selected algorithm must implement all remaining predicates, not merely the convenient equality subset.

Join-order optimization and build/probe-side selection can change input orientation. Projection maps, output ordering of columns, and outer/semi/anti semantics must be remapped consistently. A rewrite that swaps inputs without translating these contracts can return plausible values in the wrong columns or lose unmatched rows.

## Hash-join state and lifecycle

`PhysicalHashJoin` records condition types, build payload columns, left/right output projection columns, delim types, and residual-predicate mappings. Residual evaluation can require columns not present in the final output; `lhs_probe_columns`, output-to-probe mapping, and build-layout mappings preserve them. Removing those columns solely because they are not projected is incorrect.

The operator is a parallel sink for build input, an intermediate probe operator, and a source for applicable later work. Build ingestion evaluates/retains keys and payloads; combine/finalization prepares shared lookup state; probe execution can emit multiple chunks for one input because duplicate keys can create many matches. Outer and related modes need match bookkeeping and later output. External joins partition work and use source phases under memory pressure.

`InitializeHashTable` takes an initial radix-bit count. Memory policy and data distribution affect partitioning; the existence of the hash table does not guarantee the build side fits in memory. Recursive-reuse hooks can preserve build state where explicitly supported, so destruction assumptions must be checked before making scans destructive.

## Semantic edge cases

NULL equality and NULL-safe comparison are distinct predicates. Semi/anti joins return membership-related rows without ordinary duplicate multiplication; mark joins additionally carry Boolean/NULL information required by SQL subquery semantics. Outer joins must distinguish no matching row from a matching row whose payload is NULL. A residual predicate that rejects an equality candidate must not incorrectly mark it matched.

Heavy skew, empty inputs, all-NULL keys, duplicate keys, and wide nested/string payloads stress different state paths. Row output should be treated as unordered unless a higher-level contract explicitly guarantees order.

## Verification and performance requirements

Compare algorithms using equivalent SQL results, including multiplicity and NULLs. Include all supported join kinds, residual predicates, projected-away keys, correlated subqueries, empty build/probe, spilling, early limit, and cancellation. Single-thread equivalence does not cover parallel match-marker races or finalization dependencies. Use [component/API tests](../testing/component-api.md), [configuration variants](../testing/configuration.md), and [stress](../testing/stress.md).

Performance reports should vary input cardinality, selectivity, skew, key width, payload width, and memory budget. Record physical plans to distinguish algorithm-selection changes from implementation changes. The [benchmark harness](../testing/benchmarks.md) provides measurement infrastructure; this specification records no measured performance results.
