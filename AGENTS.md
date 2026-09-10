# Development feedback

Use ordinary `cargo check`, `cargo test` and `cargo clippy` for each edit/test pass.
`cargo dev check|test|run|build|clippy` also runs without tracing by default. Do not
record every operation on every pass: recording can dominate database execution
and changes the timings being investigated.

Use exhaustive tracing for a focused reproduction:

- `cargo dev trace run --profile dev-trace --bin duckdb-rust -- -c 'SELECT 42'`
  prints a summary and deletes temporary telemetry when the command ends.
- Add `--keep` immediately after `trace` for subsequent inspection:
  `cargo dev trace --keep run --profile dev-trace --bin duckdb-rust -- -c 'SELECT 42'`.
- `cargo dev statements` lists SQL execution IDs, hashes and trace paths.
- `cargo dev log EXECUTION_ID` reads a small incremental summary.
- `cargo dev sql EXECUTION_ID 'SELECT * FROM operation_stats ORDER BY total_ns DESC LIMIT 20'`
  queries the trace with the external DuckDB v1.5.5 CLI, whose version is checked.
- `cargo dev span PROCESS_LOG SPAN_ID` prints one operation and its descendants.
- `cargo dev index EXECUTION_ID` imports a completed trace for repeated SQL queries.
- `cargo dev clean` removes retained telemetry. Run it when investigation ends.

Keep at most one completed trace run, under the Git-ignored `target/dev-traces/`.
The next execution command removes the preceding run. Never commit runtime trace
files, statement manifests, result previews or analytical caches. Do not create
permanent telemetry archives or put trace output elsewhere in the repository.
Recording has a 128 MiB write budget per process; larger reproductions must be
narrowed or explicitly use `DUCKDB_DEV_MAX_BYTES`. A retained run exceeding 256 MiB
is deleted. Abandoned runs are reclaimed by the next execution/clean command once
their writers exit; retention needs no background service.

For retained SQL evidence, start in `statements/EXECUTION_ID/statement.json`,
`result.json`, `parameters.json`, and `trace.summary.json`. The SQL hash uses
parser-rendered SQL; execution IDs distinguish repeated statements and parameters.
Original SQL and parsing are in the linked request trace. Materialized results
preview at most 20 rows and report omissions; streaming results report delivered
rows and early termination.

Summaries update every 500 ms and report bytes beyond the snapshot. Error counts
include expected failures and propagation. Open spans, sequence gaps and malformed
records are incomplete evidence. Use test exit status to judge correctness.
Trace durations include instrumentation and nested work; they are unsuitable for
performance acceptance. Compare production builds against both pinned C++ versions.

Use `cargo dev coverage` when changing interfaces or adding Rust files. Missing
attributes can be added with `cargo dev coverage --write` and then reviewed.
Check instrumentation compatibility with `cargo dev trace check --workspace --all-targets`.
Production builds use `cargo build --release --no-default-features` and exclude tracing.

The root README must remain exactly as on main. Do not edit README files.
