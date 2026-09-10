# Current quality and performance inspection

This inspection covers Rust source SHA-256 `456911885546eac5703450d731e3f3f57f9595d82f415df28daa72bbfacf43f2`
and measurement binary SHA-256 `886c6963b829add7a0ad64cef5426a79edcdd4a6163e47baac7cad02085133e8`.
The source fingerprint includes the current uncommitted implementation. All four
performance campaigns used this same source and binary.

Fifteen measured workloads pass their relevant component contracts and have lower
median latency than both pinned C++ references. These are scoped correctness,
interface and performance results. Full subsystem acceptance under the
[rewrite principles](../../specs/rewrite-principles.md) and
[test/performance requirements](../../specs/testing/parity.md) is unfinished.

References: **v1.5.5** (`d8cdaa33fda8df955cc76ef58a280f68f4cd43fa`) and
pinned development **v2.0.0-dev84019** (`99063af2bd7092aff02e14184a20e24699d34d71`).
The development checkout is not asserted to be the latest remote main. Source,
executable and loaded library identities are checked separately for each campaign.

## Qualifying measured workloads

Speedup is C++ median latency divided by Rust median latency. Timings apply to the
default composition recorded in the raw reports. Alternative adapters run the
component contracts and do not inherit these timings.

| Workload | Quality/functionality checked | Speedup over v1.5.5 | Speedup over development |
| --- | --- | ---: | ---: |
| Full scan, 50,000 BIGINT rows | [Owned batches, row identities, snapshot visibility and consistent delivery across executors.](../../test/component/execution/batches.rs) | 5.78× | 5.16× |
| Filter i % 2 = 0, 50,000 input rows | [Scalar/batch equivalence, NULL propagation, lazy errors and expression effects.](../../test/component/execution/expressions.rs) | 1.04× | 1.32× |
| Ungrouped integer SUM, 50,000 rows | [Scalar/batch equivalence across integer widths, NULLs, encodings and overflow boundaries; replaceable aggregate states.](../../test/component/execution/batches.rs) | 1.33× | 3.69× |
| Indexed equality lookup, 50,000-row table | [Hash/B-tree contracts, row/key identity, atomic index publication, restart and rollback.](../../test/component/indexes.rs) | 1.81× | 9.49× |
| Scan with LIMIT 256 | [Bounded upstream demand, consumer stop, cancellation and retained result ownership.](../../test/component/execution.rs) | 17.30× | 368.96× |
| Correlated equality EXISTS, 50,000 outer rows | [Hash/nested-loop equivalence across NULLs, key domains and batches; independent cursors and cancellation.](../../test/component/execution/joins.rs) | 5.41× | 7.21× |
| Recursive UNION ALL, 1,000 iterations | [Streaming/materializing recursion, lexical scope, iteration and resource contracts.](../../test/component/recursive.rs) | 6.53× | 2.06× |
| Recursive UNION, 257-value cycle | [Duplicate elimination and fixed-point semantics across both recursion algorithms.](../../test/component/recursive.rs) | 6.60× | 3.01× |
| Correlated recursive CTE, 16 outer rows | [Outer scope, independent recursion state and prepared snapshot behavior.](../../test/component/recursive.rs) | 1.36× | 1.19× |
| Add column with literal default, 50,000-row table | [Atomic catalog/data publication, retained snapshots, rollback and supported WAL recovery.](../../test/component/alter.rs) | 17.98× | 27.02× |
| Drop column, 50,000-row table | [Column/row identity preservation and atomic alteration across both index adapters.](../../test/component/alter.rs) | 9.12× | 19.04× |
| Rename table | [Catalog visibility, prepared rebinding, rollback and writer conflicts.](../../test/component/alter.rs) | 9.77× | 18.71× |
| GROUP BY i % 64 with SUM, COUNT and mask, 50,000 rows | [Hash/ordered aggregation across 96 compositions; typed group state and aggregate adapter replacement.](../../test/component/grouping.rs) | 1.19× | 1.77× |
| ROLLUP(i % 64) with SUM, COUNT and mask, 50,000 rows | [Subtotal/empty-group behavior, NULLs, grouping masks and independent state per grouping set.](../../test/component/grouping.rs) | 1.71× | 2.05× |
| CUBE(i % 64, i % 8) with SUM, COUNT and mask, 50,000 rows | [Independent cube oracle across dense, sparse and NULL key domains; masks and typed aggregate results.](../../test/component/grouping/domains.rs) | 1.55× | 1.24× |

## Quality gate and known failures

Formatting and Clippy with warnings denied pass. The combined local test results
are **193 passed, 1 failed, 0 ignored**. All component test binaries pass.
`cargo test --offline --all-targets` fails in `sql_logic_corpus` because the new
`test/sql/settings_sessions.test` uses the named-connection directive `query T a`,
which the Rust SQL harness does not support. Cargo stopped at that binary; the
remaining subquery and type binaries were run separately and passed. The failure
remains recorded; no assertion or directive was skipped or weakened.

See the [validation manifest](validation.json), [full test log](debug-tests.log),
[remaining component tests](remaining-contracts.log), [Clippy](clippy.log) and
[format check](format.log). The test count is local coverage, not a count of mapped
or passing upstream DuckDB cases.

The existing [persistent settings comparison](../settings/reference-debug.json)
also records differences between the two C++ references: v1.5.5 differs in the
empty-key error text, and the development build aborts an explicit transaction
after an invalid setting value where Rust and v1.5.5 retain it. This inspection
did not rerun that comparison and does not claim settings parity.

Broader [grouping](../grouping/reference-batched.json),
[SQL/catalog](../sql-catalog-parity.md), native-file and upstream-suite obligations
remain open. A passing workload here does not clear those failures.

## Measurement scope

Optimized builds, one execution thread, in-memory data, three warmups and
21 samples per engine per workload, with alternating engine order. Every sample
checks expected row count and integer checksum. Component tests separately check
values, types, NULLs, errors and state semantics. Every median Rust/C++ ratio above
1.0 fails independently. These four campaigns have 15 of 15 passing workloads per
reference. Testing and builds completed before each timed campaign; no component
tests ran concurrently with timing.

Query timing includes execution and complete result consumption/checksum through
each engine's embedded API. Thus these are API-path measurements, not isolated
operator kernels. Setup, initial preparation and process startup are excluded.
DDL resets state before each sample, times execution, result consumption and any
required prepared-statement rebind, then verifies the resulting change outside
timing. DDL latency here does not establish durable-write latency.

Cold I/O, durable commits, concurrent workloads, memory/CPU/I/O cost parity,
clients, UI/UX and extension interoperability remain unmeasured by this campaign.
Small synthetic workloads on this host cannot establish whole-engine performance
parity. Prior measurements and failures remain in their original directories.

## Absolute median latencies

All values below are microseconds. Each reference has its own paired Rust samples.

| Workload | Rust vs release | C++ v1.5.5 | Rust vs development | C++ development |
| --- | ---: | ---: | ---: | ---: |
| scan | 154.500 | 893.625 | 186.500 | 961.834 |
| filter | 559.584 | 581.916 | 537.625 | 712.333 |
| aggregate | 37.625 | 49.958 | 38.667 | 142.541 |
| point | 14.167 | 25.583 | 15.083 | 143.209 |
| limit | 9.583 | 165.792 | 12.875 | 4750.375 |
| correlated_exists | 354.666 | 1917.833 | 343.959 | 2481.250 |
| recursive_linear | 1353.625 | 8835.917 | 1348.708 | 2781.833 |
| recursive_cycle | 346.375 | 2285.625 | 321.666 | 969.375 |
| recursive_correlated | 250.792 | 340.209 | 251.875 | 299.666 |
| alter_add_column | 5.917 | 106.417 | 5.875 | 158.750 |
| alter_drop_column | 4.416 | 40.292 | 4.458 | 84.875 |
| alter_rename_table | 4.167 | 40.709 | 4.292 | 80.292 |
| grouped_sum | 391.583 | 466.291 | 371.209 | 656.250 |
| rollup_sum | 439.458 | 751.000 | 450.042 | 924.458 |
| cube_sum | 1125.083 | 1744.750 | 1104.458 | 1371.083 |

Raw samples, SQL, expected results, adapter selections and build identities:
[core release](core-release.json), [core development](core-development.json),
[grouping release](grouping-release.json), [grouping development](grouping-development.json).
The [machine-readable assessment](assessment.json) links each row to its tests and measurements.
