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

The [latest full campaign](upstream-parity-final.json), recorded before the
subsequent performance changes, records 197 passed files,
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

The [latest comparison](native-regressions-column-keys.json) passes all six
measured workloads over 21 paired samples against the unchanged C++ release
baseline and 1.0 limit.
The benchmark queries, workers and comparison driver are unchanged. Results are
fully materialized and every returned value is included in timed validation.

| Workload | C++ median (ms) | Rust median (ms) | Rust/C++ ratio | Maximum |
| --- | ---: | ---: | ---: | ---: |
| Scan | 0.978583 | 0.192958 | 0.197 | 1.0 |
| Filter | 0.670292 | 0.534458 | 0.797 | 1.0 |
| Aggregate | 0.138000 | 0.052959 | 0.384 | 1.0 |
| Point lookup | 0.122417 | 0.011250 | 0.092 | 1.0 |
| LIMIT | 4.635709 | 0.012958 | 0.003 | 1.0 |
| Correlated EXISTS | 2.268958 | 1.951458 | 0.860 | 1.0 |

Published snapshot columns eliminate repeated scan transposition; single-row
demand retains row delivery. Checked batch expression and aggregate interfaces
remove repeated scalar dispatch where purity and totality are established.
Materialized results own contiguous row-major values. A conservative optimizer
pass decorrelates eligible EXISTS filters into semi/anti joins, whose hash adapter
builds once and streams probes using reusable keys from the retained type adapter.
Column key visitation validates the input once before consuming any keys.
The [architecture](architecture.md) records contracts and fallback conditions.

All intermediate performance reports remain in `native-regressions-*.json`,
including the column-only scan trial that slowed correlated requests, later SUM
failures, the first semi-join trial with missing transaction metadata, and the
[decorrelated run](native-regressions-decorrelated.json) that still failed by
28.6%. An earlier nine-pair [reusable-key run](native-regressions-reusable-keys.json)
passed, but its [21-pair confirmation](native-regressions-cleared.json) failed
EXISTS by 2.5%; the filename does not indicate acceptance. An
[inlining trial](native-regressions-inlined-keys.json) also failed and its hints
were removed. The [previous checkpoint](native-regressions-final.json) failed scan,
filter, aggregation and correlated EXISTS at 1.197, 5.856, 3.287 and 62.517 times
C++ respectively. Passing the current six cases clears those measured
regressions; it does not establish full performance parity.

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

The [current validation record](regression-validation.json) records 162 passing
Cargo tests, 13 Python harness tests, 52 release execution/subquery/type/adversarial
tests, formatting and Clippy. It checks source and executable hashes against the
performance and file-oracle reports. The six-case C++ performance gate passes;
full upstream test and full performance acceptance remain unmet, as described
above. Local conformance success cannot override those wider gaps.

The [earlier validation](parity-validation.json) and
[mutation campaign](mutation-report-isolated.json) retain their original source
provenance. The latter detected all three injected semantic faults after an
unchanged passing baseline; it was not rerun for this record.

The [current independent native-file check](reference-regressions-verified.json)
also passed 37 top-level compatibility checks against DuckDB v1.3.0,
including continued reads and writes in both engines. This is a file/behavior
oracle for the supported subset; the performance baseline remains the separately
pinned C++ v2.0 development checkout.
