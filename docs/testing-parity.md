# Test and performance parity

**Full test parity and zero performance regressions are not achieved.** The
[accepted requirement](../specs/testing/parity.md) uses C++ DuckDB revision
`99063af2bd7092aff02e14184a20e24699d34d71`. All previous 1.25 allowances are
superseded. Faster workloads cannot compensate for slower workloads.

## Retained upstream inputs

[`test/upstream/duckdb`](../test/upstream/duckdb/README.md) contains the complete
pinned source archive and a checked manifest: all 15,646 tracked files and
symlinks, including assertions, harnesses, generators, build configurations and
fixtures. Verification checks archive and individual asset hashes, modes,
symlink targets and the source-level inventory.

| Source inventory | Count | Execution status |
| --- | ---: | --- |
| SQLLogicTest files, including slow/coverage files | 5,638 | All selected by default; current failures recorded individually |
| Native test declarations | 1,506 | Rust assertion mappings remain unported |
| Python test declarations | 561 | Client/tooling parity remains unported |
| Swift test declarations | 132 | Client parity remains unported |
| Benchmark workloads | 1,160 | Full lifecycle/performance mapping remains unported |

These are source declarations, not compiled registrations or expanded test
instances. Conditional compilation, parameterization, configuration/platform
matrices and external suites still need independent accounting. Retaining 100%
of the tracked source inputs does not establish 100% execution or test parity.

## Running the original SQL assertions

[`run_upstream.py`](../scripts/run_upstream.py) extracts the checked archive,
creates an isolated directory and Rust process per file, and drives the public
Rust API through [`worker.rs`](../test/runner/worker.rs). SQL and expected outputs
are read unchanged. The interpreter supports ordering, literal/regex/hash
oracles, labels, basic loops/variables, named connections and database lifecycle
commands. Harness self-tests deliberately supply incorrect answers.

Unsupported SQL, unknown harness controls, conditional omissions, timeouts,
errors and unmapped assertions remain gaps. Skips never count as passes. The
full gate requires exact case identities and every scope obligation; a selected
passing subset cannot satisfy it. A JSON-lines journal preserves each completed
file even if the larger campaign stops before the final report.

The [latest full campaign](upstream-parity-final.json) records 197 passed files,
1,197 failures, 4,234 unsupported files, seven timeouts and three incomplete
files. All 5,638 source identities have exactly one outcome. Its 14,583 passed
SQL instances include prefixes of files that subsequently failed; they do not
establish that those files passed. No native/client declarations are mapped yet.

The [initial full campaign](upstream-parity-initial.json) executed all 5,638 files:
161 passed, 1,611 failed, 3,861 were unsupported, three timed out and two were
incomplete. This predates corrections to path substitution and lifecycle
semantics. Its 12,012 passed SQL instances before subsequent file failures are
not 12,012 passing files. The report is preserved, not replaced by a passing
selection.

Harness work still includes requirements/extensions, concurrency controls,
fixture path/layout semantics, external result files, imported numeric conversion
rules, full RE2-compatible matching and remaining configuration directives.
The interpreter's current output comparison is not yet full C++ harness parity.

```sh
python3 scripts/upstream_suite.py
python3 scripts/run_upstream.py --report target/upstream-new.json
```

The full runner currently exits nonzero because these obligations are open.
`--path-prefix` records an explicit subset and cannot turn full acceptance green.

## C++ performance baseline

[`compare_native.py`](../scripts/compare_native.py) builds a verification-only
C++ worker against the pinned release library and a separate Rust worker. Both
prepare the same queries over the same logical data. Timed execution includes
complete result materialization and checksum validation. The driver alternates
engine order, warms each case three times, and retains at least nine paired
samples. Every median ratio above **1.0** fails.

Reports identify source, workload, compiler, library, executable, configuration
and host. They retain failed measurements. The C++ worker is an independent
oracle and is not linked into the Rust engine.

The [initial comparison](native-regressions-initial.json) failed scans, filtering,
aggregation and correlated EXISTS. Their measured Rust/C++ ratios were 2.679,
7.109, 28.505 and 76.419. Those failures are acceptance failures, even though
other cases passed. Subsequent changes must be checked against the same C++
baseline; unrelated performance improvement is not the objective.

The [latest comparison](native-regressions-final.json) still fails those four
cases. The changes remove intermediate scan row copies, preserve single-row
delivery, feed simple global aggregates through a column batch interface, and
avoid repeated correlated-query metadata/binding allocations.

| Still-regressing workload | Latest Rust/C++ median ratio | Required maximum |
| --- | ---: | ---: |
| Scan | 1.197 | 1.0 |
| Filter | 5.856 | 1.0 |
| Aggregate | 3.287 | 1.0 |
| Correlated EXISTS | 62.517 | 1.0 |

Every intermediate failed measurement is retained, including the column-only
scan trial that made small correlated requests slower. The final representation
keeps single-row input as a row and shares columns for bulk input. Narrowing
these gaps does not satisfy the zero-regression requirement.

```sh
python3 scripts/compare_native.py --report target/native-new.json
```

This currently requires the sibling checkout's release build at
`build/engine-walkthrough`. The six cases cover warm, serial, prepared-query
execution only. Startup, binding/planning, cold storage, durability/recovery,
concurrency, other APIs, clients/tooling, CPU, memory and I/O costs still need
comparable measurements. `complete_performance_parity` remains false while
these scopes are unmeasured. Rust-to-Rust adapter benchmarks are diagnostics,
not substitutes for the C++ acceptance baseline.

The [SQLite review](sqlite-testing-review.md) describes the additional tests and
the remaining allocator, crash simulation, concurrency and coverage work.

## Local validation

The [validation record](parity-validation.json) records 144 passing Cargo tests,
13 Python harness tests, 18 release execution/adversarial tests, formatting and
Clippy. The [final mutation campaign](mutation-report-isolated.json) starts with an
unchanged passing baseline and detects all three injected semantic faults.
The C++ performance and full upstream parity gates remain failed, as recorded
above. Local conformance success cannot override those acceptance failures.

The [independent native-file check](reference-parity-batches.json) also passed
37 top-level compatibility checks against DuckDB v1.3.0 after the batch changes,
including continued reads and writes in both engines. This is a file/behavior
oracle for the supported subset; the performance baseline remains the separately
pinned C++ v2.0 development checkout.
