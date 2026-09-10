# Test and performance parity

**Full test and performance parity are not established.** The
[accepted requirement](../specs/testing/parity.md) uses development revision
`99063af2bd7092aff02e14184a20e24699d34d71` as the correctness authority when
release and development disagree. Performance uses the faster pinned C++
reference for each comparable workload. All previous 1.25 allowances are
superseded. Faster workloads cannot compensate for slower workloads.

The [numeric progress report](numeric-port.md) retains the unsigned/decimal
foundation checkpoint. Its seven failing timings are historical source evidence.
The [binding follow-up](binding-regressions.md) fixes the two upstream SQL
regressions; [numeric batch work](numeric-batches.md) records subsequent
performance changes and pre-push checks. No selected campaign establishes full
correctness, performance or native-file parity.

The latest [value-and-expression checkpoint](value-expression-progress.md)
integrates BIGNUM, BIT, temporal boundary repairs, recursive WAL recovery and
typed concat/combination binding. Its latest fifth-checkpoint refresh executes
all 5,638 files: 423 pass, with 19,240 successful records including failed-file
prefixes. No checkpoint-three full-file pass is lost. The initial shorter
constant-column timeout prefix does not reproduce against the last pushed source
in paired diagnosis; a later join deadline reaches all previous assertions with
an extended timeout, but its source-paired investigation remains open. Timeouts
and the 512 MiB writer limit remain gaps. All three new timing trials pass
33/34 faster-reference workloads: decimal total aggregation still fails and is
being repaired before pushing. The preceding 34/34 checkpoint-three timing
matrix is historical, not evidence that this later source passes. All failures
remain retained; new-family workloads and the rest of the parity matrix remain
open.

The [earlier performance comparisons](settings/README.md) pass all sixteen
measured workloads against both **v1.5.5** (`d8cdaa33fd`) and the pinned
development build. ORDER BY ALL joins grouped SUM, ROLLUP, CUBE and the original
twelve passing cases. Each uses 21 paired samples and the unchanged 1.0 maximum. The
[earlier combined campaign](references-source-builds/summary.json), including
its failed performance measurements, is retained. Its file/ALTER compatibility
failures remain unresolved. Passing sixteen cases does not establish full
performance parity. [Builds, commands and compatibility gaps](reference-builds.md).

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

The [numeric foundation campaign](upstream-parity-numeric.json) records **305 passed
files**, 1,887 failures, 3,435 unsupported files, eight timeouts and three
incomplete files. All 5,638 identities have exactly one outcome. Its 16,968
passed SQL instances include prefixes of files that later fail; they are not
16,968 passing files. The run retains the three-second per-file limit, two
workers and unchanged assertions. Native/client mappings and other required
scope obligations remain unverified or unported.

Against the older report, 110 files moved into passing status and two formerly
passing files now fail: `test/issues/rigger/test_536.test` exposes a VARCHAR
value changed by numeric VALUES coercion, and `test/sql/cte/cte_schema.test`
fails with an ambiguous table reference. Both are fixed in the
[binding-only serial refresh](upstream-parity-binding-regressions-final.json):
313 passed files, 1,877 failures, 3,437 unsupported files, eight timeouts and
three incomplete files. All 5,638 identities are accounted for, with 17,018
passed records including prefixes of failed files. Eight files newly pass and
none of the prior 305 passes is lost. This is not evidence for later batch
source changes; their fresh validation is tracked separately. Many formerly
unsupported files now reach an assertion failure instead.

The [final batch-code refresh](upstream-parity-batches-final.json) repeats all
5,638 files after the performance changes: 313 passed, 1,877 failed, 3,437
unsupported, eight timeouts and three incomplete. Its 17,018 passed records
include failed-file prefixes. No file changes status relative to the valid
binding-only checkpoint, and the worker identity remains unchanged throughout
the serial campaign. The [final 34-workload timing matrix](numeric-batches.md)
passes each workload against the faster C++ reference in both Rust campaigns.
These are scoped regression checks, not full test, native-file or performance
parity; earlier failed campaigns remain retained.

The [preceding full campaign](upstream-parity-final.json), recorded before the
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

The native local SQL runner now owns default and named connections for each
file over the selected `Database`. It preserves session/transaction isolation,
checks the original result/error assertions, and rejects unavailable engine
capabilities even inside an expected-error record. It remains a smaller harness
than the Python upstream interpreter; labels, loops and other unsupported native
directives still fail explicitly.

[`session_reference.py`](../scripts/session_reference.py) uses the same Python
record oracle for persistent Rust and C++ sessions. Its default command builds
the current Rust worker, records source/binary identities and adapter selections,
and validates both pinned C++ references. An explicitly supplied worker is marked
as having unverified source identity. Partial files and mismatches remain failed
even when later records pass. [Settings and ordering results](settings/README.md)
retain differing C++ behaviors and earlier failed runs.

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

[`fastest_reference.py`](../scripts/fastest_reference.py) combines the two
retained campaigns after checking source, binary, workload, host, configuration
and case identities. It selects the smaller C++ median per workload and checks
both retained Rust medians against it, without choosing the better Rust sample.
Missing cases, incorrect results and mismatched identities are rejected.

Reports identify source, workload, compiler, library, executable, configuration
and host. They retain failed measurements. The C++ worker is an independent
oracle and is not linked into the Rust engine.

The [initial comparison](native-regressions-initial.json) failed scans, filtering,
aggregation and correlated EXISTS. Their measured Rust/C++ ratios were 2.679,
7.109, 28.505 and 76.419. Those failures are acceptance failures, even though
other cases passed. Subsequent changes must be checked against the same C++
baseline; unrelated performance improvement is not the objective.

The [earlier development comparison](native-regressions-alter.json) passes all 12 measured
workloads over 21 paired samples against the unchanged C++ release baseline
and 1.0 limit. The prior nine query definitions and setup remain unchanged.
The workers and driver now also accept explicit DDL reset/effect-check phases.
Query cases still time execution and complete result validation. Each DDL
sample starts from reset state, times execution/result consumption (including
any required prepared-statement rebind), and verifies the mutation afterward.
Reset and effect verification are untimed and are retained in the report.

| Workload | C++ median (ms) | Rust median (ms) | Rust/C++ ratio | Maximum |
| --- | ---: | ---: | ---: | ---: |
| scan | 0.919125 | 0.180042 | 0.196 | 1.0 |
| filter | 0.630333 | 0.511417 | 0.811 | 1.0 |
| aggregate | 0.125083 | 0.051750 | 0.414 | 1.0 |
| point | 0.122333 | 0.012292 | 0.100 | 1.0 |
| limit | 4.520875 | 0.014083 | 0.003 | 1.0 |
| correlated_exists | 2.110459 | 1.899334 | 0.900 | 1.0 |
| recursive_linear | 2.584458 | 1.242792 | 0.481 | 1.0 |
| recursive_cycle | 0.894125 | 0.308125 | 0.345 | 1.0 |
| recursive_correlated | 0.289833 | 0.240250 | 0.829 | 1.0 |
| alter_add_column | 0.143834 | 0.005417 | 0.038 | 1.0 |
| alter_drop_column | 0.082500 | 0.003958 | 0.048 | 1.0 |
| alter_rename_table | 0.077208 | 0.003958 | 0.051 | 1.0 |

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
C++ respectively. The [six-case column-key comparison](native-regressions-column-keys.json)
first cleared those measured development regressions. The later 12-case
development comparison preserved that result and covered recursive queries and
three DDL operations. These results do not override the v1.5.5 failures recorded
above and do not establish full performance parity.

```sh
python3 scripts/compare_native.py --report target/native-new.json
```

This currently requires the sibling checkout's release build at
`build/engine-walkthrough`. The cases cover serial in-memory queries and
three DDL operations. Startup, general binding/planning, cold storage, durability/recovery,
concurrency, other APIs, clients/tooling, CPU, memory and I/O costs still need
comparable measurements. `complete_performance_parity` remains false while
these scopes are unmeasured. Rust-to-Rust adapter benchmarks are diagnostics,
not substitutes for the C++ acceptance baseline.

The [SQLite review](sqlite-testing-review.md) describes the additional tests and
the remaining allocator, crash simulation, concurrency and coverage work.

## Local validation

The [earlier ALTER validation record](alter-validation.json) records 175 passing
Cargo tests, 16 Python harness tests, 23 release ALTER/logging/checkpointing
tests, formatting and Clippy. It checks source and executable hashes against the
performance and file-oracle reports. Its 12-case development performance gate passes;
full upstream test and full performance acceptance remain unmet, as described
above. Local conformance success cannot override those wider gaps.

The [earlier recursive validation](recursive-validation.json),
[earlier regression validation](regression-validation.json),
[earlier parity validation](parity-validation.json) and
[mutation campaign](mutation-report-isolated.json) retain their original source
provenance. The latter detected all three injected semantic faults after an
unchanged passing baseline; it was not rerun for this record.

The [historical independent native-file check](reference-alter.json)
also passed 39 top-level compatibility checks against DuckDB v1.3.0,
including continued reads and writes in both engines. This is a file/behavior
oracle for the supported subset; the performance baseline remains the separately
pinned C++ v2.0 development checkout.

The [SQL/catalog worklist](sql-catalog-parity.md) retains the complete active
goal. Targeted CTE and ALTER campaigns remain incomplete. The
[ALTER campaign](upstream-alter-initial.json) passed 17 of 117 files, with 23
failures and 77 unsupported outcomes. The [local SQL comparison](alter-sql-reference.json)
also records a pinned C++ ADD COLUMN constraint discrepancy. The historical
full campaign above has not been rerun for these changes.
