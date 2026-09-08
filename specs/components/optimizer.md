# Logical optimizer

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Optimization

`Optimizer` owns a sequence of ordered transformations, with per-pass profiling and verification. The ordering is part of the implementation: passes can create opportunities or alter binding/statistics information needed by later passes. Global optimizer enablement and individual disabled-optimizer settings affect which transformations run.

| Pass family | Examples in this checkout | Required semantic property |
| --- | --- | --- |
| Expression simplification | Constant folding, arithmetic/CASE/conjunction simplification, regex and string rewrites | Preserve NULL, overflow, type, and function semantics |
| Predicate movement | Filter pullup/pushdown, CTE filters, join filters, scalar-function pushdown | Do not change outer-join, side-effect, or security semantics |
| Join transformations | Decorrelation cleanup, outer-join simplification, join ordering/elimination, build/probe choice | Preserve multiplicity, correlation, and unmatched-row behavior |
| Aggregation/window transformations | Aggregate reuse, partial aggregation pushdown, grouping sets, common aggregates, window rewrites | Preserve grouping, ordering, frame and aggregate-state semantics |
| Projection/materialization | Unused columns, common subexpressions/subplans, column lifetime, CTE inlining, late materialization | Keep output bindings and dependencies consistent |
| Limits and scans | Limit pushdown, Top-N, sampling, row-group pruning, partitioned execution | Respect order, cardinality, and scan capability contracts |
| Statistics | Filter statistics and propagation | Statistics must remain valid for the data visible to the statement |
| Extension/remote hooks | Statement remote pushdown and extension optimizers | Honor the extension catalog and execution contracts |

Some statistics-dependent work is explicitly guarded when a CTE contains DML, because earlier writes can invalidate assumptions about later reads. Function repeatability and volatility matter when sharing or moving computation. Optimizer testing therefore needs both result equivalence and targeted plan-shape assertions.

Source: [optimizer.cpp](../../../duckdb/src/optimizer/optimizer.cpp), [join_order](../../../duckdb/src/optimizer/join_order/), [rules](../../../duckdb/src/optimizer/rule/), [logical plan verifier](../../../duckdb/src/planner/logical_plan_verifier.cpp).

## Optimizer interfaces and pass composition

Optimize takes ownership of a logical plan and returns the transformed plan. RunOptimizer controls per-pass disablement, interruption, profiling and verification. Extension hooks can surround built-in optimization. OptimizeStatement also supports statement-level remote pushdown before the ordinary logical plan pipeline; it is a different interface from replacing a logical subtree.

The main state is a context, binder and current plan, together with rewrite helpers. A transformation can replace nodes, move expressions, alter projection maps and update cardinality/statistics. Its output must remain a valid input to every subsequent enabled pass.

Source: [optimizer.cpp](../../../duckdb/src/optimizer/optimizer.cpp).

## Join-order subsystem

[JoinOrderOptimizer](../../../duckdb/src/include/duckdb/optimizer/join_order/join_order_optimizer.hpp) separates relation statistics, a query graph, extracted predicates and their filter metadata. Expression equivalence sets can derive implied edges: A=B and B=C can justify an A=C relationship for planning. Materialized-CTE and delim-scan statistics have explicit storage, and recursive CTE indexes constrain interpretation.

This is a semantic graph, not simply a permutation of every join in the syntax tree. Outer joins, correlations, effects and incompatible predicates can limit reorderability. Cardinality estimates influence costs and build/probe choice, but inaccurate estimates must affect performance rather than query truth.

## Transformation contracts

| Change | Information that must be preserved or reconstructed |
| --- | --- |
| Push a filter below a join | Join type, NULL introduction, predicate scope and residual filtering |
| Drop unused columns | Every parent binding, projection map and hidden consumer |
| Inline/share a CTE | Side effects, volatility, recursive dependencies and number of evaluations |
| Combine aggregate work | Distinct/filter/order semantics, grouping and state compatibility |
| Replace ORDER + LIMIT with Top-N | Exact order keys, NULL order, limit/offset and tie semantics required by SQL |
| Prune a row group | Conservative statistics valid for the visible data and cast semantics |
| Move a function into a scan | Source capabilities and the function's errors/effects/repeatability |

Removing unused columns can renumber bindings. The implementation explicitly clears/remaps projection information around relevant transformations instead of assuming stale column numbers remain meaningful. DML inside CTEs can invalidate statistics during a single statement, and the built-in sequence guards some statistics-dependent transformations accordingly.

## Worked reasoning example

For an inner join with `WHERE left_table.k = 7`, filter propagation can restrict the left scan and, if a valid equality join relates both keys, help restrict the right scan. The same move across a left outer join requires analysis of which side is preserved and whether the predicate can reject introduced NULL rows. A superficially identical expression is not enough to justify the rewrite.

A regression must therefore capture both positive and negative cases: a rewrite that is beneficial and legal, and a nearby case where the optimizer must retain the original semantics.

## Performance and observability

Per-pass timing distinguishes planning overhead from execution cost. Plan text/EXPLAIN and plan-cost tools help observe chosen structure, but matching one printed operator is a weaker oracle than result equivalence. Benchmarking should compare the baseline/current engines under identical data, build, extension and memory/thread settings.

No optimizer pass should assume every other pass is enabled: configuration tests explicitly disable the optimizer or change related behavior. When a transformation depends on normalization from an earlier pass, that precondition must be established in code or the transformation must safely decline.

## Verification links

Use [SQLLogicTest](../testing/sqllogictest.md) for semantic cases, [configuration](../testing/configuration.md) for equivalent alternate execution, [benchmarks](../testing/benchmarks.md) for planning/runtime regressions, and [fuzzer](../testing/fuzzer.md) for combinations humans are unlikely to write. Target tests should assert NULL, duplicates, empty inputs, correlated/outer joins, function effects and invalid expressions where relevant.
