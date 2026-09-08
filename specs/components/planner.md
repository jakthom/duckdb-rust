# Binder and logical planner

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Statement processing, binding, and logical planning

The [statement preprocessor](../../../duckdb/src/planner/statement_preprocessor.cpp), [Planner](../../../duckdb/src/planner/planner.cpp), and [Binder](../../../duckdb/src/planner/binder.cpp) turn syntax into typed operations. Binding resolves database/schema/table/column names, overloads, casts, aliases, parameters, correlations, CTE references, constraints, and statement permissions/properties.

| Representation | Meaning | Next consumer |
| --- | --- | --- |
| `SQLStatement` and parsed objects | SQL structure and source positions | Preprocessor/binder |
| `BoundStatement` | Bound names, result types, logical plan | Planner/session preparation |
| Bound `Expression` | Resolved type and expression semantics | Optimizer and expression execution |
| `ColumnBinding` | Logical column identity, based on table and column indexes | Plan transformations and binding resolver |
| `LogicalOperator` tree | Relational operations and output bindings | Optimizer and physical planner |
| `StatementProperties` / parameter map | Transaction, result, rebind, and parameter requirements | Prepared execution and client context |

Binding can leave unresolved prepared-parameter types for later rebinding. Planner extensions can run after binding or provide alternate binding behavior. The planner also rewrites trigger dependencies and dependent joins, performs recursive dependency planning and decorrelation, and verifies that executable plans no longer contain unresolved correlated expressions or dependent joins.

Sources: [planner.cpp](../../../duckdb/src/planner/planner.cpp), [binder](../../../duckdb/src/planner/binder/), [expression_binder](../../../duckdb/src/planner/expression_binder/), [subquery](../../../duckdb/src/planner/subquery/), [logical_operator.hpp](../../../duckdb/src/include/duckdb/planner/logical_operator.hpp).

## Responsibilities and representation ownership

The planner bridges syntax to executable meaning. Binder owns scope resolution and semantic construction; Planner coordinates the completed bound statement, statement properties, parameters and validation. Its public result fields include plan, names, types, properties and value_map. A successful plan therefore has both an operator structure and a result/transaction contract.

| Object | Carries | Critical distinction |
| --- | --- | --- |
| BindContext and table bindings | Names, aliases and available columns in a scope | A textual name is not a stable execution position |
| BoundStatement | Result names/types and an owned logical plan | The consumer must keep metadata consistent with the plan root |
| BoundColumnRefExpression | ColumnBinding, type and correlation depth | The same name can refer to different tables/scopes |
| BoundParameterMap | Parameter values/types and rebind status | Unknown prepared parameters need a deferred resolution path |
| LogicalOperator | Owned children and expressions, output types/cardinality | Logical children use ownership, unlike arena references in a physical plan |
| StatementProperties | Access, result and parameter requirements | Planning success does not authorize skipping transaction setup |

Sources: [planner.hpp](../../../duckdb/src/include/duckdb/planner/planner.hpp), [binder.hpp](../../../duckdb/src/include/duckdb/planner/binder.hpp), [bind_context.cpp](../../../duckdb/src/planner/bind_context.cpp), [logical_operator.hpp](../../../duckdb/src/include/duckdb/planner/logical_operator.hpp).

## Binding flow

1. Establish the statement's scope and parameter map in the current ClientContext/catalog view.
2. Bind table references, including native/extension tables, functions, CTEs and replacement scans.
3. Resolve expressions against those bindings; select function overloads, insert casts, and assign return types.
4. Construct relational operators and output metadata, including hidden/intermediate expressions needed by ordering, grouping or dependent subqueries.
5. Invoke applicable post-bind extension hooks and perform dependency/trigger/decorrelation processing.
6. Extract statement properties and finalized parameter entries. Preserve the explicit unresolved-parameter state when binding must be repeated later.
7. Verify the resulting plan and expose it to the optimizer.

Binder has modes beyond ordinary query execution, including preparing and extracting names or replacement scans. A helper that works in standard binding must not inadvertently execute side effects or require resolved runtime parameters in an inspection mode.

## Correlation and dependent operations

A correlated reference records a column identity and a depth into outer scopes. Decorrelation rewrites those dependencies into relational structure. Delim-related structures track distinct correlated values where the chosen strategy requires them. The planner's postconditions reject remaining dependent joins and unresolved subquery/correlation expressions that physical execution cannot accept directly.

Triggers introduce another dependency boundary: writes and transition data can require ordered dependent work, rather than a simple scalar expression rewrite. The current planner rewrites trigger dependencies before physical planning. A new dependency-bearing statement must define when data is produced, which scope can see it and how it participates in the transaction.

## Prepared statements and catalog dependencies

Preparing a query can succeed before all parameter types are known. Execution with concrete parameters can require rebinding. A prepared plan also depends on catalog objects and function signatures, so object alteration/drop and parameter changes can require different behavior than an unchanged ordinary query.

For `SELECT x + ? FROM t`, the binder must distinguish the type of t.x, admissible overloads of addition, an unresolved parameter and the eventual result type. Reusing a plan specialized for one binding without checking its assumptions risks type or catalog errors at execution.

## Invariants and failure handling

Every output binding consumed above a node must be supplied by that subtree or handled as an explicit dependency. Aliases and projection maps must survive renumbering. Implicit casts cannot silently change required strict/try-cast semantics. Ambiguous names, nonexistent objects, illegal grouping and incompatible types are ordinary binding errors, while inconsistent generated bindings indicate an internal bug.

Bind data that survives planning needs the required copy/serialization behavior. Extension fallback binding must still establish the same names/types/properties contract as native binding; returning a non-null operator alone is insufficient.

## Verification links

[Configuration verification](../testing/configuration.md) covers copy, prepare, serializer and binding checks; [compiled/API tests](../testing/component-api.md) cover prepared state and parameter lifetime. SQL coverage should combine aliases, nested scopes, CTEs, NULL/nested types, invalid usage, catalog changes and parameter variants. The [coverage matrix](../testing/coverage.md) identifies the corresponding corpus directories.
