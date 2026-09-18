# Rewrite test and performance acceptance

Accepted user requirement, 2026-09-09. This supersedes the earlier 1.25
regression allowances and comparisons that used Rust as the acceptance baseline.

The correctness authority is C++ DuckDB development at
`99063af2bd7092aff02e14184a20e24699d34d71`.
The goal is full test parity and no performance regressions in the engine or
supporting code. Faster cases cannot compensate for slower cases. Work added to
remove a measured regression is within scope; unrelated performance improvement
work is not the objective.

The user additionally requires both pinned source references: **DuckDB v1.5.5**
at `d8cdaa33fda8df955cc76ef58a280f68f4cd43fa` for release compatibility, and the
development checkout above for the rewrite's additional upstream baseline.
Resolve and print the executable, exact version and hash before each campaign;
reject a version or revision that differs from the explicitly selected target.
Run SQL/file compatibility and strict performance checks separately against
each, applying the correctness precedence and faster-reference performance rule
below. A pass against one reference cannot hide an untested or failing obligation
against the other. Existing
v1.3.0 fixtures/reports are historical coverage. Preserve the original source/test
inventory and all prior performance provenance. See the [build runbook](../../docs/reference-builds.md).

## Continuous feedback and completion

Accepted user clarification, 2026-09-18: documentation, planning, status and
agent-instruction-only changes must NEVER trigger an engine verifier sweep.
This includes instruction-only agent configuration and comments with no
executable effect. Validate only the changed artifact: diff, links, examples,
syntax and consistency as applicable. No engine build, full tests, recovery,
Kani or benchmarks are required. A changed executable example, generated input,
build setting or test fixture is classified by its actual effect instead.
Unrelated documentation edits do not invalidate prior engine acceptance or
require a fresh tested commit hash. Preserve the actual tested revision/scope.
The implementation workflow below applies only when relevant executable or
validation inputs change; the verifier must reject documentation-only dispatches.

Accepted user clarification, 2026-09-15: use fast, targeted feedback while work
is in progress, then a full independent pass when the agent believes the planned
chunk is 100% complete. That belief changes its status to **ready for verification**;
it is not evidence that the goal has been achieved. A chunk can span many edits
and commits. Do not run completion gates after every edit or intermediate commit.

Declare a validation manifest in the maintained backlog entry/handoff before
implementation: owned paths; fast check commands; affected test targets/filters;
unchanged upstream IDs and negative/boundary cases; full functional acceptance
commands; performance workload IDs, configurations and metrics. Select cases by
the changed contract and its consumers, not only by filename. Expand the manifest
when scope changes and preserve the reason; do not shrink it to hide failures.

| Stage | Required feedback | Not a completion claim |
| --- | --- | --- |
| Documentation/planning/status/instructions only | Relevant diff, link, example, syntax and consistency checks | Never an engine sweep; existing engine evidence remains valid for its tested inputs. |
| Small edit batch | Affected package/target check, e.g. `cargo check -p duckdb-rust --lib` for library edits | Does not execute tests or check unrelated targets. |
| Behavior change | Named test target/filter plus the assigned small upstream set; affected Python tests for Python-only changes | Does not cover the whole goal or establish performance parity. |
| Ready for verification | Frozen integrated tree; delegated full sweep, complete assigned functional population and at-parity or better performance | Completion requires all applicable gates, not the agent's confidence or Cargo alone. |

Keep incremental build caches worktree-local and reuse profile/feature choices.
Do not add an unnecessary `cargo check` immediately before a targeted test that
already compiles the same code. Coalesce edits; run one validation process per
worktree. Any watch orchestration must debounce file changes, label results with
their exact source state, and invalidate results if inputs change during a run.
No zero-test selection, unchecked old binary, changed fixture or stale cached
source may be reported green. Tracing and full Clippy/recovery/Kani/performance
campaigns stay out of the edit loop; focused performance diagnosis is allowed.

Targeted Cargo/Python checks and the upstream debug/cache/watch path are
available. Use `scripts/run_upstream.py --target both --debug-worker --path-list
target/<chunk>-paths.txt --report target/<chunk>-debug-<state>.json` after a
logical behavior batch. Add `--watch --debounce-seconds 0.25` for continuous
settled-edit feedback; stop it before another validation process starts. The
watcher's lock coordinates upstream campaigns, not arbitrary Cargo commands.
Its source fingerprint invalidates an in-flight result when inputs change.
The `--target both` list must exist on both pins; use separate per-pin lists
and reports when paths differ. Preserve each pin's unchanged cases.

The selected cache validates exact pinned source bytes and metadata; it does
not stage external fixture/include dependencies. Such selections use the
ordinary runner. Prebuilt workers require current source/binary/profile
provenance. `--debug-worker --compare-release` checks normalized outcomes
against a freshly built release worker. Final selected acceptance also runs
the ordinary `--path-list` campaign without debug/prebuilt flags. These modes
retain unchanged oracles, pin/source/binary identities and test counts, reject
stale/modified inputs, and label debug results as feedback only. Debug timings
never satisfy the performance gate; a selected run is not a full census.

At assignment, start with one end-to-end case, its negative/boundary counterpart
and an existing shared consumer. Repeat affected tests and exact upstream cases
after each behavior batch. Expand consumer coverage whenever an interface
changes. Diagnose representative performance early, before the whole family is
built, using separate quiet-host runs; final measurements remain mandatory.
An independent review checks semantics, ownership, missing coverage and workload
comparability before the integrated source freezes. The maintained backlog
records the dispatch tracks and exact per-chunk selections.

At readiness, stop edits to the integrated tree. The configured Terra/low verifier
runs exactly `python3 scripts/verify_chunk.py` for the common progressive regression
sweep. The full assigned upstream/API/native/configuration population and the
chunk's performance workloads are additional acceptance gates: the current sweep
does not automatically select or execute them. The integration owner must collect
all three outcomes for the same final tree. Run timing campaigns serially on a
quiet host, not alongside the sweep. Every ordinary sweep stage must pass and
Kani must run and be reported under its exploratory policy. Subsequent changes
to the implementation or relevant validation inputs invalidate completion;
rerun the sweep and refresh affected acceptance evidence for those changes.
Unrelated prose or agent-instruction changes do not invalidate these results.

## Correctness

Accepted user clarification, 2026-09-10: **development wins when release and
development disagree**. Implement the pinned development behavior, including
types, results, errors and externally observable semantics. Preserve both
reference outcomes and identify the precise disagreement; a demonstrated
release-only difference is a compatibility divergence, not a reason to change
the engine away from development behavior. This precedence does not excuse a
Rust failure where the references agree, an unimplemented development feature,
or an unexplained mismatch. Do not rewrite upstream assertions to conceal a
version difference.

Retain every upstream test, harness, configuration, generator and fixture.
Preserve original assertions, expected results, errors, hashes, ordering,
connection/restart behavior, concurrency and configuration semantics. Existing
tests that exercise unavailable features remain failing or unported obligations.
An inventory, a source archive, or a successful C++ run does not establish Rust
test parity. Native API and internal assertions need corresponding Rust contract
tests with an explicit source-case mapping. SQL text alone cannot replace them.

Account separately for source files, declared cases, compiled registrations,
generated/parameterized instances, configuration/platform combinations and
external suites. Include slow tests. Selections and omissions must be recorded;
skips, unsupported operations, timeouts, crashes, missing dependencies and tests
that have not been ported are never counted as passes. Full acceptance requires
complete mappings and execution evidence, not a selected passing percentage.

## Performance

Accepted user clarification, 2026-09-10: benchmark against **whichever pinned
C++ reference is faster for each comparable workload and configuration**. If
release is faster, release is the baseline; if development is faster,
development is the baseline. Do not choose one reference globally or select the
slower reference to obtain a pass.

The maximum latency/cost ratio is **1.0 against the faster C++ reference**:
`Rust median / min(release median, development median) <= 1.0`. Throughput must
be at least `max(release throughput, development throughput)` under the same
measurement protocol. Record both reference measurements, the selected baseline
and the resulting gate for every workload. Other measured resource costs retain
their separate no-regression gates; speed does not compensate for resource use.
This applies to SQL execution,
planning, storage, recovery, transactions, APIs, clients and supporting tooling.
Measure additional CPU, memory and I/O regressions where the workload uses them;
unmeasured areas remain open rather than inheriting another workload's result.

Use release builds, the same logical data and operation semantics, equivalent
resource/concurrency settings and the same requested result. Record the original
C++ build, source and executable identities, workload and fixture hashes, host,
adapter/configuration selections, warmup, every sample and correctness outcomes.
Record setup, preparation, execution, materialization, startup and durability
costs explicitly; include each cost in the workload that exposes it to callers.

Correctness takes precedence over timing: the compared operations must implement
the same semantics and pass result validation. If a release/development semantic
difference prevents a comparable measurement, record that scope as not comparable
and the two-reference performance gate as open. Retain the development-correct
measurement, but do not treat an incorrect, unsupported or missing reference
result as a faster baseline or as permission to claim full performance parity.

Every measured median slowdown fails the gate. Sampling variability is recorded
and investigated, not converted into a permitted percentage slowdown. Preserve
failed runs; do not cherry-pick reruns or move the baseline to a slower Rust run.
The absence of a regression in a small sample does not prove all workloads or
all supported machines are regression-free. A measured scope cannot be promoted
to full parity while unmeasured scopes remain.

### Per-chunk performance acceptance

Every chunk's validation checklist, including child chunks, must contain the
explicit requirement **at-parity or better performance**. This is not restricted
to optimization chunks or deferred to G24. Before implementation, select the
workloads/configurations that expose the changed operation and affected existing
consumers, with relevant sizes/distributions and latency/throughput/resource
metrics. Add missing comparable workloads instead of substituting an unrelated
passing microbenchmark. Shared-path changes require wider consumer coverage.

Completion requires every selected workload to satisfy the faster-reference
rules above on the final integrated source tree. Existing latency campaigns use
`scripts/compare_native.py` for each pin and `scripts/fastest_reference.py` to
gate both retained Rust medians against the faster C++ median. Other interfaces,
throughput and resource costs need corresponding adapters and separate gates;
the latency script is not evidence for those dimensions. Record reproducible
commands, exact inputs/builds, warmups/samples and correctness checks, retaining
failed measurements. Small fast-loop timings do not substitute for this campaign.

Report functional, regression-sweep/Kani and performance outcomes separately.
Use performance **pass**, **fail** or **open**; missing or incomparable evidence
is open, not success. A functional implementation with a measured slowdown is
not a complete implementation chunk, even if it improves on the previous Rust
revision. Known baseline failures are not grandfathered into a performance pass.
Exploratory Kani limits do not relax functional or performance requirements.

For strictly documentation-only changes, record the gate as **not applicable:
documentation-only**, with a reviewed diff establishing no executable/build/
configuration/fixture/workload effect. This limited classification is not a
performance pass. Test harnesses, supporting tooling and runtime changes still
require applicable workload evidence; missing equivalent C++ coverage stays
open rather than being reclassified as documentation. Historical "implemented"
labels describe functional progress, not proof of this completion gate.

## Additional testing

Each large implementation chunk must also run and report a [Kani
checkpoint](kani.md), rather than running it on every edit. During exploration,
proof success and comprehensive proof coverage are not stage-completion
conditions. Investigate findings, record limitations, and let proofs follow the
emerging design. Bounded proof results are separate evidence; they do not
establish upstream test or performance parity.

Review SQLite's independent harnesses and carry over applicable methods:
generated/differential SQL, malformed files paired with SQL, transient and
persistent fault sweeps, compound failures, allocation-failure handling,
resource ownership/leaks, concurrency histories, boundary tests, disabled
optimizations, coverage and mutation testing. See the implementation's
[SQLite testing review](../../docs/sqlite-testing-review.md) for implemented
checks and remaining harness work. Coverage of Rust branches, incorporation of
DuckDB tests, semantic parity, and performance parity are distinct results.
