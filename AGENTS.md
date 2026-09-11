# Development feedback

## Project context

Use `docs/parity-backlog.md` for current functionality gaps, goal groups and
dependencies, and `docs/architecture.md` to locate implementations. Read only
the assigned goal and relevant `specs/`/source files; do not bulk-load historical
reports. The specs describe requirements and the pinned C++ source, not a claim
that the Rust counterpart exists. Update the backlog in place when behavior
changes; do not create another progress/checkpoint summary document.

## Edit loop

While a chunk is in progress, use the narrowest useful feedback: ordinary
`cargo check` plus tests directly affected by the edit. Do not launch the full
workspace test suite, all-target Clippy, or Kani after each edit, file, commit,
or tool call.

## Chunk completion sweep

A chunk is an explicit goal group, feature slice, subsystem or adapter
implementation, cross-module refactor, or other unit of work identified in the
task plan. Several edits and commits can form one chunk. Do not infer a chunk
boundary merely because an intermediate edit or commit is complete.

Before declaring each chunk complete, delegate one full sweep to the project
`verifier` agent. The primary agent must not run or babysit the full sweep. The
verifier uses `gpt-5.6-terra` with low reasoning effort, as configured in
`.codex/agents/verifier.toml`. Do not silently substitute a higher-priced model
for verification. The verifier runs this single command from the repository root:

```sh
python3 scripts/verify_chunk.py
```

The runner is progressive and fail-fast. It reports elapsed time for each stage
and performs formatting, all-target checking, all-target Clippy, the full test
suite except the exhaustive recovery truncation test, that exhaustive test in a
visible final test stage, and the maintained Kani checkpoint. A chunk is not
complete until every ordinary stage has passed against its final source tree and
Kani has been run and reported. If an edit is made after the sweep begins, rerun
the complete sweep against the new final tree. Use
`python3 scripts/verify_chunk.py --list` to inspect the stages without executing
them.

## Exploratory checkpoints with Kani

The chunk completion runner invokes `python3 scripts/verify_kani.py` after all
ordinary stages. Keep Kani out of the routine edit/check/test loop. Focused
`cargo kani --harness ...` runs are useful when debugging a proof, but stage
completion requires the full maintained suite through the command above.

During the exploratory rewrite, the requirement is to run Kani and report what
was learned. A passing proof suite is not required to complete the chunk. The
runner's nonzero status reports unsuccessful or incomplete verification; it is
not a stage-completion verdict. Investigate counterexamples against the intended
behavior and handle confirmed bugs through the ordinary correctness process.
Record timeouts, unsupported code, setup failures and unproved behavior as limits;
after a reasonable setup/retry attempt, continue exploration with those limits
explicit. Never describe an unsuccessful or unexecuted proof as passing.

Add or adapt proofs when they help clarify the emerging design. Do not reshape
production code, restrict design choices, or require a proof for every invariant
to satisfy Kani. Internal architecture remains provisional; revise or retire
outdated harnesses with a reason when intended contracts change. Keep assumptions
and bounds honest and safety checks enabled. A short checkpoint summary of the
command, outcome, findings and limits is sufficient. Formal coverage requirements
and proof acceptance gates can be established later. See [the policy](specs/testing/kani.md).

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

## Validation artifacts

Keep raw JSON, JSONL, logs, profiles and validation dumps under the Git-ignored
`target/` directory, not under `docs/`. Keep docs to the maintained backlog,
architecture, durable implementation notes and runbooks. Record only concise
revision-specific outcomes in the appropriate backlog entry. Historical reports
remain recoverable from Git history.

The root README must remain exactly as on main. Do not edit README files.
