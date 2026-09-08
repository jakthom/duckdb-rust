# Pipelines, events, and task scheduling

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Pipelines, tasks, and events

A pipeline typically has one source, a sequence of intermediate operators, and an optional sink. Blocking operations introduce dependencies between pipelines. `MetaPipeline` organizes related pipelines and dependency decisions, including recursive and external-input/dataflow cases.

`Executor` builds and coordinates the graph. `PipelineExecutor` drives chunk flow, cached output, source/sink state, task budgets, finishing, and resumption. `TaskScheduler` provides worker scheduling; events express prerequisites and completion. This checkout includes asynchronous work and externally supplied pipeline input, so a purely synchronous pull-iterator model is insufficient.

| Execution state | Typical owner | Lifetime |
| --- | --- | --- |
| Physical plan and prepared metadata | Prepared/query state | Until plan/result ownership is released |
| Pipeline graph and events | Executor | Active query execution |
| Global source/operator/sink state | Operator/pipeline execution | Shared by participating tasks |
| Local state and expression executors | Task/pipeline executor | One task's execution contribution |
| Thread context | Worker execution | Worker/task execution scope |
| Result buffers | Collector/result manager/result object | Until consumed or released |

Sources: [executor.cpp](../../../duckdb/src/parallel/executor.cpp), [pipeline.cpp](../../../duckdb/src/parallel/pipeline.cpp), [pipeline_executor.cpp](../../../duckdb/src/parallel/pipeline_executor.cpp), [meta_pipeline.hpp](../../../duckdb/src/include/duckdb/parallel/meta_pipeline.hpp), [event.hpp](../../../duckdb/src/include/duckdb/parallel/event.hpp), [task_scheduler.hpp](../../../duckdb/src/include/duckdb/parallel/task_scheduler.hpp).

## Queue and producer interfaces

`TaskScheduler` is database-wide. `CreateProducer` supplies a producer token used to enqueue and retrieve work associated with an executor. `ScheduleTask` and `ScheduleTasks` accept an explicit scheduler pool type or use the regular-task helper. Separate pools/queues support ordinary execution and asynchronous work. Producer-scoped retrieval lets a client driving pending work contribute to its own execution rather than requiring every step to run on a background thread.

`SetThreads(total_threads, external_threads)` allocates `total_threads - external_threads` background workers; externally participating threads count toward the total. `SetAsyncThreads` controls the asynchronous pool separately. Thus the configured total is not automatically the number of newly created background threads. A thread-count mutex protects reconfiguration, and thread relaunching is a managed lifecycle operation.

`ExecuteTasks` runs until a task limit, marker change, or lack of available tasks, depending on the overload. `ExecuteForever` requires its marker to remain valid until thread join. `GetEstimatedCPUId` is a scheduling hint with an intentionally permissive fallback, not an affinity or uniqueness guarantee.

## Dependency publication and resumable tasks

Events encode prerequisites between initialization, execution, finishing, and completion work. A pipeline cannot read a sink's finalized state merely because its input scan has exhausted; the relevant combine/finalization dependencies must also complete. Recursive and shared subplans need explicit dependency relationships to avoid scheduling consumers against unfinished data.

Task execution is budgeted so pending-query APIs can return control without completing the entire query. A partially executed task retains its progress; a blocked task waits for readiness; a finished task contributes to event completion. These outcomes are not interchangeable with an empty queue. An empty producer queue can coexist with blocked work that may become runnable later.

Sources: [task.hpp](../../../duckdb/src/include/duckdb/parallel/task.hpp), [pipeline_executor.hpp](../../../duckdb/src/include/duckdb/parallel/pipeline_executor.hpp), [event.cpp](../../../duckdb/src/parallel/event.cpp), [task_scheduler.cpp](../../../duckdb/src/parallel/task_scheduler.cpp).

## Failure and shutdown requirements

Interrupts and task errors must propagate to the owning executor and wake any relevant waiters. Cancellation has to retire unfinished work without running completion callbacks twice or freeing state while a worker still accesses it. Database shutdown additionally must stop/join worker pools before their referenced services disappear. Holding a session lock while waiting on a callback that needs the same lock is a potential deadlock and requires review at asynchronous boundaries.

## Verification requirements

Scheduler-sensitive tests need more than a large row count: vary thread budgets, drive pending execution from the caller, cancel blocked work, reconfigure threads, and close contexts with outstanding tasks. Repeated concurrency tests and TSan builds target races; deterministic result checks target lost/replayed contributions. [Stress](../testing/stress.md), [configuration](../testing/configuration.md), and [CI](../testing/ci.md) describe the corresponding harnesses. Do not equate a successful single-thread SQLLogicTest run with validation of event races.
