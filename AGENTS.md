# Development feedback

Use ordinary `cargo check`, `cargo test` and `cargo clippy` for each edit/test pass.

## Stage validation with Kani

Run `python3 scripts/verify_kani.py` before declaring each substantial rewrite
chunk or planned implementation stage complete. This includes a feature slice,
subsystem/adapter implementation, cross-module refactor, or change to an important
data, ownership, arithmetic, or state-transition invariant. Several small commits
can form one chunk; do not defer validation until the whole PR is finished.

Keep Kani out of the routine edit/check/test loop. Documentation-only changes,
formatting, and isolated low-impact edits do not require a run. Focused
`cargo kani --harness ...` runs are useful when debugging a proof, but stage
completion requires the full maintained suite through the command above.

At the stage boundary, add or update bounded proofs for the changed invariants
and retain existing proofs. Verify production functions, document input/loop
bounds and assumptions, and leave safety and unwinding checks enabled. A failed,
timed-out, unsupported, missing-tool, or empty run is an incomplete gate. Fix or
isolate the cause and rerun; report unresolved limitations without claiming the
stage passed. Record commands, version, source revision, proof scope and results
in the stage's validation summary. Kani supplements the required ordinary,
compatibility and performance checks. See [the policy](specs/testing/kani.md).

## Development tracing

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
