# Physical plan generation

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Physical planning

[PhysicalPlanGenerator](../../../duckdb/src/execution/physical_plan_generator.cpp) resolves output types and column references, estimates cardinality, and dispatches each logical operator to its physical implementation. Per-operator planning lives under [physical_plan](../../../duckdb/src/execution/physical_plan/).

The generator owns a `PhysicalPlan`; operator children use references within that plan's ownership domain. This differs from assuming that every physical node exclusively owns its children. `ColumnBindingResolver` changes logical references into executable positions. Physical planning also records repeatability and can insert vector-verification operators when configured.

The output is a physical operator structure, not an already scheduled task graph. Pipeline construction happens at execution initialization.

## Ownership and interface boundary

The input is an owned LogicalOperator tree; the output is an owned PhysicalPlan. PhysicalPlan holds an arena, references to all constructed operators and a root reference. Make<T> constructs operators in that arena and registers them for destruction. Child edges are references within the plan's lifetime, which allows shared dependencies without making each parent own its child.

Source: [physical_plan_generator.hpp](../../../duckdb/src/include/duckdb/execution/physical_plan_generator.hpp).

## Planning phases

| Phase | Effect | Failure to guard against |
| --- | --- | --- |
| ResolveOperatorTypes | Compute concrete output types through the logical structure | Unknown/inconsistent types reaching vector allocation |
| ColumnBindingResolver | Resolve symbolic column bindings to executable references | Reading the wrong input position after a rewrite |
| CreatePlan dispatch | Select a physical implementation for each logical operation | Unsupported node silently bypassing semantics |
| Plan state/dependencies | Retain CTE collections/exchanges, dependency and repeatability information | Shared producer state dying before consumers |
| Verification/wrapping | Verify the root and optionally insert vector verification | Invalid graph/state surviving until worker execution |

CreatePlanInternal dispatches on LogicalOperatorType. A logical trigger reaching this stage is explicitly an internal error because earlier planning must rewrite it. This is an example of a phase invariant, not a missing physical trigger implementation to fill in locally.

## Algorithm and ordering decisions

Physical planning converts relational intent into algorithms: scan paths, join implementations, grouped/ungrouped aggregation, sorting/Top-N, DML and result collection. Selection depends on predicates, types, cardinality, available indexes, ordering and configured capabilities. It must not select an implementation solely because the logical node name resembles its name.

UseBatchIndex, PreserveInsertionOrder, OrderPreservationRecursive and partition helpers determine which ordering/partition protocol an operator can use. An algorithm that runs in parallel must advertise the actual guarantees it provides; preserving one task's order does not establish a global order across tasks.

## Shared and recursive state

The generator tracks recursive working tables, recurring tables, recursive-state scan planning, materialized CTE operators and streaming exchanges. A CTE read is therefore not necessarily a second independent scan of its syntax. Consumers can refer to state produced by another branch, and [scheduler](scheduler.md) must respect the dependency that makes that state ready.

Repeatability classification also matters when physical work could be shared or driven more than once. Side-effecting or volatile operations cannot automatically inherit the treatment of an ordinary deterministic scan.

## Example and failure boundaries

For an aggregate over a filtered table, the generator creates a scan, filter and aggregate implementation, wires their references and records result types. It does not execute the aggregate or launch workers. [Execution](execution.md) builds pipelines around the selected roles; [scheduler](scheduler.md) supplies tasks and dependency completion.

An operator addition needs logical dispatch, argument/type mapping, ownership, source/sink role selection, correct ordering metadata and diagnostic representation. It also needs a defined path for unsupported combinations. Once operator memory has been allocated in the plan arena, consumers must never retain references after the plan is destroyed.

## Verification links

Use plan/API cases in [compiled tests](../testing/component-api.md), the corresponding SQL family, forced vector modes and ordering tests from [configuration](../testing/configuration.md), and [benchmarks](../testing/benchmarks.md) for algorithm regressions. A plan that prints correctly still needs empty-input, cancellation, spill and restart checks appropriate to the selected operators.
