# Rewrite test and performance acceptance

Accepted user requirement, 2026-09-09. This supersedes the earlier 1.25
regression allowances and comparisons that used Rust as the acceptance baseline.

The baseline is C++ DuckDB at `99063af2bd7092aff02e14184a20e24699d34d71`.
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
each. Passing one reference cannot compensate for failing the other. Existing
v1.3.0 fixtures/reports are historical coverage. Preserve the original source/test
inventory and all prior performance provenance. See the [build runbook](../../docs/reference-builds.md).

## Correctness

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

The maximum latency/cost ratio is **1.0 against C++**, per comparable workload
and configuration. Throughput must not decrease. This applies to SQL execution,
planning, storage, recovery, transactions, APIs, clients and supporting tooling.
Measure additional CPU, memory and I/O regressions where the workload uses them;
unmeasured areas remain open rather than inheriting another workload's result.

Use release builds, the same logical data and operation semantics, equivalent
resource/concurrency settings and the same requested result. Record the original
C++ build, source and executable identities, workload and fixture hashes, host,
adapter/configuration selections, warmup, every sample and correctness outcomes.
Record setup, preparation, execution, materialization, startup and durability
costs explicitly; include each cost in the workload that exposes it to callers.

Every measured median slowdown fails the gate. Sampling variability is recorded
and investigated, not converted into a permitted percentage slowdown. Preserve
failed runs; do not cherry-pick reruns or move the baseline to a slower Rust run.
The absence of a regression in a small sample does not prove all workloads or
all supported machines are regression-free. A measured scope cannot be promoted
to full parity while unmeasured scopes remain.

## Additional testing

Review SQLite's independent harnesses and carry over applicable methods:
generated/differential SQL, malformed files paired with SQL, transient and
persistent fault sweeps, compound failures, allocation-failure handling,
resource ownership/leaks, concurrency histories, boundary tests, disabled
optimizations, coverage and mutation testing. See the implementation's
[SQLite testing review](../../docs/sqlite-testing-review.md) for implemented
checks and remaining harness work. Coverage of Rust branches, incorporation of
DuckDB tests, semantic parity, and performance parity are distinct results.
