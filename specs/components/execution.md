# Physical operators and execution engine

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Operator interface

The [PhysicalOperator interface](../../../duckdb/src/include/duckdb/execution/physical_operator.hpp) defines three roles. An operator can implement more than one role: a hash aggregate, for example, consumes into aggregate state as a sink and later emits grouped rows as a source.

| Role | Main methods | State | Data-flow contract |
| --- | --- | --- | --- |
| Source | `GetGlobalSourceState`, `GetLocalSourceState`, `GetData`/`GetDataInternal` | Shared source state and per-task local state | Produce chunks until finished or blocked |
| Intermediate operator | `GetGlobalOperatorState`, `GetOperatorState`, `Execute`, optional `FinalExecute`/`OperatorFinalize` | Global and local operator state | Consume input; emit zero, one, or multiple output chunks |
| Sink | `GetGlobalSinkState`, `GetLocalSinkState`, `Sink`, `Combine`, `PrepareFinalize`, `Finalize` | Shared sink state and per-task sink state | Accumulate input; combine local state; publish finalized state |
| Partition/order support | `RequiredPartitionInfo`, `GetPartitionData`, `NextBatch`, order/parallelism methods | Partition and batch metadata | Preserve only the ordering and partitioning explicitly advertised |
| Graph construction | `BuildPipelines`, `GetSources` | Pipeline build state | Establish source/operator/sink roles and dependency edges |

`Sink` and `Combine` can execute concurrently against shared state; their implementation must synchronize shared mutations. A local sink state is finished after its `Combine`. `Finalize` runs after contributors finish, and an operator can arrange additional event/task work as part of finalization.

## Operator result protocol

| Channel | Status | Meaning |
| --- | --- | --- |
| Intermediate | `NEED_MORE_INPUT` | Current input consumed |
| Intermediate | `HAVE_MORE_OUTPUT` | Call again with the same input |
| Intermediate | `FINISHED` | Pipeline can stop |
| Source | `HAVE_MORE_OUTPUT` | Output available; source not exhausted |
| Source | `FINISHED` | Source exhausted |
| Source/sink and supported asynchronous boundaries | `BLOCKED` | Resume through the interrupt/notifier protocol after work becomes ready |
| Sink | `NEED_MORE_INPUT` / `FINISHED` | Continue consuming / no further input needed |
| Sink finalization | `READY` / `NO_OUTPUT_POSSIBLE` / `BLOCKED` | Publish output capability, prune dependent work, or await readiness |

The intermediate enum includes `BLOCKED`, but its header explicitly says intermediate operators should currently not emit it. An empty source chunk is not interchangeable with a blocked source or a successful nonempty output. Correct resumption must preserve input and local state and avoid double-consuming chunks.

Source: [operator_result_type.hpp](../../../duckdb/src/include/duckdb/common/enums/operator_result_type.hpp).

## Operator families

| Family | Implementations and concerns |
| --- | --- |
| Scans | [scan](../../../duckdb/src/execution/operator/scan/): native tables, table functions, values, collections and other sources |
| Filter/projection | [filter](../../../duckdb/src/execution/operator/filter/), [projection](../../../duckdb/src/execution/operator/projection/): selection, expression evaluation, unnesting |
| Joins | [join](../../../duckdb/src/execution/operator/join/): hash, nested-loop, range/merge, ASOF, positional, delim and related variants; build/probe and unmatched-row handling |
| Aggregates/windows | [aggregate](../../../duckdb/src/execution/operator/aggregate/): grouped/ungrouped aggregation, distinct handling, windows; state partitioning and finalization |
| Ordering | [order](../../../duckdb/src/execution/operator/order/): ordering, Top-N and supporting sort behavior |
| Set operations | [set](../../../duckdb/src/execution/operator/set/): unions and recursive/materialized CTE execution machinery |
| DML and data output | [persistent](../../../duckdb/src/execution/operator/persistent/): insert/update/delete/merge and copy-related persistence |
| Schema operations | [schema](../../../duckdb/src/execution/operator/schema/): DDL and metadata mutation |
| Coordination/helpers | [helper](../../../duckdb/src/execution/operator/helper/): result collectors, limits, explain/settings, verification and auxiliary execution |
| CSV ingestion | [csv_scanner](../../../duckdb/src/execution/operator/csv_scanner/): sniffing, buffering, token state machine, parallel scan and conversion |

## Chunk-flow state machine

`PipelineExecutor` owns the active per-task traversal through a pipeline. It obtains source output, pushes it through intermediate operators, and submits produced chunks to the sink. If an intermediate returns `HAVE_MORE_OUTPUT`, its input must remain available until repeated execution consumes it. Filters may produce no rows; expanding operators may produce several output chunks from one input. Neither case permits the driver to lose the current position in the operator chain.

Final execution is a separate stage for operators that buffer residual output. Sink-local work is combined once, followed by finalization coordinated through events. `FINISHED` can stop work early, for example after a limit; that does not eliminate required cleanup of task state, outstanding work, or the result lifecycle. Error capture belongs to the executor/query machinery rather than permitting exceptions to escape arbitrary scheduler threads.

Sources: [pipeline_executor.cpp](../../../duckdb/src/parallel/pipeline_executor.cpp), [executor.hpp](../../../duckdb/src/include/duckdb/execution/executor.hpp), [physical_operator.hpp](../../../duckdb/src/include/duckdb/execution/physical_operator.hpp).

## Worked pipeline decomposition

For a scan/filter/group-by/order query, the scan and filter can feed the aggregate sink. Aggregation publishes grouped results only when its prerequisite build work is ready. An aggregate source then feeds an order sink; the order source eventually feeds the result collector. This is an illustrative dependency shape, not a promise that the optimizer always chooses these particular implementations. Streaming aggregation/window specializations and Top-N can change the shape.

For a hash join, build-side ingestion prepares shared lookup state before the dependent probe work can use it. Some join modes need a subsequent source phase to emit unmatched build rows. Recursive CTEs, delim joins, and materialized CTE exchanges add dependencies that a simple depth-first operator-tree iterator cannot express. See [physical planning](physical-planner.md) for graph construction and [scheduling](scheduler.md) for readiness.

## State and concurrency contract

Global state coordinates all workers participating in one operator execution. Local state belongs to a task's contribution and can contain expression executors, input/output chunks, local hash structures, and progress cursors. Sharing a physical operator does not mean its local state is shared. Global methods must make synchronization, partition ownership, and publication explicit.

An asynchronous `BLOCKED` result must install the appropriate notification/resumption relationship without dropping input or reporting completion. A wakeup can race with the transition into blocking; the notifier protocol, not periodic polling invented by each operator, must resolve readiness. Query cancellation must also break waits and allow resources to be reclaimed.

## Verification requirements

Exercise single- and multi-thread execution, empty build/input, multi-output operators, early termination, forced spilling, cancellation, and asynchronous readiness. Check result cardinality and duplicates as well as value contents: losing or replaying one chunk can pass aggregate-only assertions. Verify ordering only where the plan promises it. Compare materialized and streaming consumption, including abandoned results. Relevant harnesses are [configuration variants](../testing/configuration.md), [component/API tests](../testing/component-api.md), and [stress tests](../testing/stress.md).
