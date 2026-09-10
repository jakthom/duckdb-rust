# Development operation tracing

The `dev` Cargo feature selects the optional Rust package in `dev/`. The default
feature set is empty. Production uses `--release --no-default-features`; enabling
`dev` without debug assertions is a compile error. The `dev-trace` profile provides
optimized diagnostic execution; `cargo dev trace` selects instrumentation. The production engine has no runtime
trace toggle, recorder, background flushing thread, or telemetry dependency.

## When to record and what persists

Use ordinary Cargo checks and tests on each development pass. `cargo dev test`,
`cargo dev run`, `cargo dev check`, `cargo dev build`, and `cargo dev clippy` also
run without tracing by default. Exhaustive tracing adds substantial recording,
serialization, synchronization and I/O work, changes execution timing, and can
generate far more data than an investigation needs. Enable it for a focused
reproduction of a failure or suspected bottleneck. Performance acceptance always
uses untraced production builds.

`cargo dev trace run|test|check|build|clippy ...` records a diagnostic run, prints
an operation summary, and deletes its temporary telemetry at command completion,
including on failure. `cargo dev trace --keep ...` explicitly retains one run for
inspection. The next execution through `cargo dev`, or `cargo dev clean`, removes
it. All managed telemetry stays in Git-ignored `target/dev-traces/`; do not commit
or archive it. `--keep` requires cleanup when the investigation ends. Source
documentation should describe behavior, without preserving generated trace dumps.

Recording has a cumulative 128 MiB write budget per process, shared across its
trace files and statement metadata. Exceeding the budget fails the diagnostic
with an explicit error; it never silently drops events and reports success.
Narrow the reproduction first. `DUCKDB_DEV_MAX_BYTES` is an explicit positive
byte-count override for an exceptional investigation. A retained workspace over
256 MiB is deleted at completion. These are a per-process recording budget and
a completed-workspace retention limit, not a global disk quota for every child
process or an analytical import.

OS file locks prevent cleanup while managed writers or readers are active.
Forced termination can leave temporary files; the next execution or cleanup
reclaims them after their writers exit. No retention daemon is needed. Compiling
the `dev` feature directly does not create an automatic log directory: recording
requires the runner's output configuration or an explicit subscriber.

## Run feedback

A traced run
contains `run.json` with its command, revision, worktree state, SHA-256 of Rust and
Cargo sources, profile, completion status, source-unchanged check, and duration;
captured `stdout.log` and `stderr.log`; a separate JSONL file for each instrumented process; and
`summary.json` with operation counts, cumulative/max durations, errors, panics,
source locations, and unfinished operations. SQL statements have separate files
under `statements/EXECUTION_ID/`. `cargo dev log` reads the latest run
and prints a bounded overview of errors, expensive operations and frequent calls.
`cargo dev log TRACE_DIRECTORY_OR_EXECUTION_ID OPERATION_SUBSTRING` selects an operation family.
`cargo dev span PROCESS_LOG SPAN_ID` prints one operation and its descendants,
including values and source locations. Raw JSONL can be read while execution is
still active. Summaries are accumulated while recording and published atomically
every 500 ms and at root completion. They report the byte prefix covered and any
bytes written afterward. Reading a summary never scans the complete raw log.

## SQL identity and inspection

When recording is enabled, every execution has a unique timestamp/process/counter ID. SHA-256 of the
parser-rendered statement provides a stable SQL content key across repetitions,
including prepared executions. It is a text identity, not proof of semantic
equivalence; database state, settings and parameters can change the result.
Parameters are recorded separately without changing the SQL hash. Each statement
in a multi-statement request gets its own execution directory. SQL delimiters in
strings and comments remain the parser's responsibility.

The request/prepare trace keeps the original text and parsing operations, including
syntax failures. An execution references its containing request ID and covers
binding, optimization, execution and transaction completion. Prepared executions
have their own files and canonical SQL even when there is no active request.
No statement text or trace field is added to the production engine's contracts.

Each directory contains:

- `statement.json`: execution ID, SQL hash/text, phase, parent request, run/source,
  profile, status, duration, error if any, and the direct trace path.
- `trace.jsonl`: self-contained site metadata, parented spans, durations and values
  for that execution. Operations are routed to this file, not copied out of a
  multi-gigabyte process log afterward.
- `trace.summary.json`: a small incremental summary and its covered byte length.
- `parameters.json`: typed parameter values for an execution.
- `result.json`: schema, row/affected-row counts and the first 20 materialized rows,
  with the omitted row count. Typed values preserve integer widths and float bits.
  Streaming execution records delivered-row count and early-stop status; it does
  not materialize the stream to create a preview. Failed executions have error
  metadata instead of a successful result preview.

`cargo dev statements` lists identities and paths without reading trace records.
For one SQL command:

```sh
cargo dev trace --keep run --profile dev-trace --bin duckdb-rust -- -c 'SELECT 42 AS answer'
cargo dev statements
cargo dev log EXECUTION_ID
cargo dev sql EXECUTION_ID 'SELECT module, operation, calls, errors, max_ns FROM operation_stats ORDER BY total_ns DESC LIMIT 20'
cargo dev clean
```

The execution ID is printed as soon as recording starts. An explicit trace/run
directory also works while the run is retained. Starting another execution
replaces the previous run; retaining a run does not create a history archive.

## DuckDB analysis of large logs

`cargo dev sql` uses the external DuckDB **v1.5.5** CLI; every invocation checks and
prints its version. `DUCKDB_DEV_DUCKDB` can select its executable. All orchestration
is Rust. The recorder runs without this executable, and neither the development
package nor the production engine links the C++ library.

Raw analysis uses an explicit NDJSON schema and exposes `events`, `spans` and
`operation_stats`. Span/site keys include the filename to distinguish processes
and statement files. Result rows are JSON for direct agent consumption. Examples:

```sh
cargo dev sql EXECUTION_ID "SELECT span, parent, operation, error FROM spans WHERE outcome IN ('error', 'panic') LIMIT 20"
cargo dev sql EXECUTION_ID "SELECT span, fields FROM events WHERE kind = 'value' LIMIT 20"
cargo dev index EXECUTION_ID
cargo dev sql EXECUTION_ID 'SELECT * FROM operation_stats ORDER BY max_ns DESC LIMIT 20'
```

Import explicitly creates `trace.duckdb` with columnar events, joined spans and
aggregated operation statistics. Repeated queries use that cache in read-only
mode. Import is bounded to 2 GB of DuckDB memory with spill support; raw scanning
still costs a full JSON read for aggregations. Import checks record sequences,
does not skip malformed JSON, and rejects files that change while being imported.
Cached queries reject changes in source file size or modification time. A raw
query against a growing file can fail on a partial tail or report a changing
prefix; inspect the live summary or wait for statement completion. Missing ends
remain open spans, never invented successful completions.

The cache also records the analyzer's source hash and requires rebuilding after
the analyzer changes. A single process-wide flushing thread serves all active
recorders; opening another SQL scope does not create another background thread.

The optional external integration tests run with
`cargo test --profile dev-trace --package duckdb-dev --test analytics -- --ignored` and require
the verified v1.5.5 executable. Ordinary recorder tests do not require DuckDB.

## Instrumentation contract

The development-only attribute instruments free functions, implementation methods,
and interface default methods. It also instruments nested named functions and
implementations, and explicit function/method calls inside their bodies, including
calls inside closures. Interface calls remain visible when an external adapter
has no internal instrumentation. The
source coverage command checks `src/`, `tools/`, `test/`, and `benchmark/`, and
requires explicit instrumentation of macro-generated implementations. New code
cannot pass `cargo dev trace` or `cargo dev coverage` with missing attributes. The current source
inventory is 189 Rust files, 1,560 non-const function bodies, 167 interface method
declarations and 14,187 call expressions, plus four explicitly instrumented
function-generating macro definitions. These are source counts, not a claim that
every operation executes in every test.

While recording, every executed instrumented function creates a start record and an end record with
nanosecond wall duration. Normal return, Result success/error, and panic unwinding
are distinguished. Primitive arguments/returns, strings, and slice lengths are
value events. Large plans and object graphs require explicit, relevant values
at decision boundaries; they are not formatted automatically. No Debug bounds
are imposed on engine interfaces. Events preserve source file, line, module,
operation identity, parent, process, and thread. Call-expression spans include
argument evaluation; named function spans identify entry into the callee itself.
`returned` means control returned, while `ok` and `error` describe a function's
declared Result. Error values propagate unchanged. Arbitrary handles, iterators,
and custom values impose no formatting bounds.

Runtime tracing cannot run in constant evaluation. `Date::days` currently retains
its const contract and is listed explicitly in the coverage report; runtime calls
to it still get call-expression spans. Arithmetic instructions, constructors,
compiler-generated derives, and opaque macro/library internals are attributed to
their enclosing operation. External adapters can
use the same public attribute/layer; their internal source is outside this repo's
coverage audit. Async function instrumentation is rejected until a poll-scoped
implementation exists, so a synchronous entered guard cannot accidentally cross
an await.

## Recording and interpretation

The recorder is a standard `tracing-subscriber` layer. `FileLog` owns concurrent
recording, `TraceLayer` consumes standard span/event callbacks, `Operation` owns
the synchronous entered scope, and `TraceContext` carries the span and subscriber
across thread boundaries. A caller can select another conforming subscriber
without changing instrumented engine operations. Local
recording needs no collector, network connection, Python, or C++ component.

Each SQL scope owns an append-only JSONL file, with a process file for operations
outside SQL. Nested statements use isolated subscribers and retain a parent
execution ID; `TraceContext` carries the subscriber and SQL identity across thread
boundaries. Records have monotonic sequence
numbers and timestamps. Operation metadata is emitted once per site. Writes are
buffered without sampling or dropping; producers block on recorder contention.
A thread flushes every 25 ms, including while the engine stalls. Closing a root
operation flushes its entire trace. Explicit process exit/abort calls flush their
prefix before terminating. A recording failure terminates the dev process
with a diagnostic instead of silently returning an incomplete successful run.
Abrupt process termination can lose buffered records; the runner records failure
and the reader reports still-open spans. Crash-injection tests can deliberately
leave child spans open while their parent test succeeds. `run.json` keeps test
status and `trace_complete` separate; these incomplete spans are never silently
counted as complete. This is diagnostic logging, not a durable
transaction log.

Durations include tracing overhead, child work, scheduling and waits. Summing
nested operation durations double-counts work. Error counts include propagation
through multiple callers and expected errors in tests. Trace comparisons guide
investigation; unchanged correctness assertions and measurements without tracing
against both pinned C++ builds still decide performance acceptance.
