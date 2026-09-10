# Quality and performance inspection, 2026-09-09

This inspection tests the current uncommitted Rust implementation without changing
engine code. Its native source SHA-256 is `0a1501343aede7b8af12f9922411d5eddd061a14a4b28a3200e6c8629fe18e93`;
its measurement binary SHA-256 is `7acc4cd658c05e2dbe7840990a235e62d16dcc7878beab5d079fa9b068d9a92f`.
All six campaigns use that same source and binary. The full source, test and
harness fingerprint is in [validation.json](validation.json), with an unchanged
source check after testing and measurement.

**15 specific workloads pass their relevant local correctness/interface
checks and have lower median latency than both C++ references.** These are scoped
results. No entire subsystem has demonstrated every requirement in the
[rewrite principles](../../specs/rewrite-principles.md) and
[test/performance acceptance specification](../../specs/testing/parity.md).
The table is the complete qualifying list from the 16 currently defined native
workloads; it is not an inventory of all operations that might be faster.

The references are **v1.5.5** (`d8cdaa33fda8df955cc76ef58a280f68f4cd43fa`)
and pinned development **v2.0.0-dev84019**
(`99063af2bd7092aff02e14184a20e24699d34d71`). The development build is not
asserted to be the latest remote main. Each campaign validates the clean source
checkout, executable version/revision and loaded library identity.

## Qualifying measured workloads

Speedup is C++ median latency divided by Rust median latency. The default Rust
composition is timed. Other adapters share the applicable conformance tests;
their timings are not inferred from the default implementation.

| Measured workload | Quality/functionality checked | Speedup over v1.5.5 | Speedup over development |
| --- | --- | ---: | ---: |
| Full scan, 50,000 BIGINT rows | [Owned batches, row identities, snapshot visibility and consistent delivery across executors.](../../test/component/execution/batches.rs) | 5.55× | 6.03× |
| Filter i % 2 = 0, 50,000 input rows | [Scalar/batch equivalence, NULL propagation, lazy errors and expression effects.](../../test/component/execution/expressions.rs) | 1.04× | 1.24× |
| Ungrouped integer SUM, 50,000 rows | [Scalar/batch equivalence across integer widths, NULLs, encodings and overflow boundaries; replaceable aggregate states.](../../test/component/execution/batches.rs) | 1.29× | 3.76× |
| Indexed equality lookup, 50,000-row table | [Hash/B-tree contracts, row/key identity, atomic index publication, restart and rollback.](../../test/component/indexes.rs) | 2.07× | 8.96× |
| Scan with LIMIT 256 | [Bounded upstream demand, consumer stop, cancellation and retained result ownership.](../../test/component/execution.rs) | 15.84× | 225.48× |
| Correlated equality EXISTS, 50,000 outer rows | [Hash/nested-loop equivalence across NULLs, key domains and batches; independent cursors and cancellation.](../../test/component/execution/joins.rs) | 5.48× | 6.88× |
| Recursive UNION ALL, 1,000 iterations | [Streaming/materializing recursion, lexical scope, iteration and resource contracts.](../../test/component/recursive.rs) | 6.48× | 1.95× |
| Recursive UNION, 257-value cycle | [Duplicate elimination and fixed-point semantics across both recursion algorithms.](../../test/component/recursive.rs) | 6.57× | 2.82× |
| Correlated recursive CTE, 16 outer rows | [Outer scope, independent recursion state and prepared snapshot behavior.](../../test/component/recursive.rs) | 1.38× | 1.13× |
| Add column with literal default, 50,000-row table | [Atomic catalog/data publication, retained snapshots, rollback and supported WAL recovery.](../../test/component/alter.rs) | 19.53× | 26.47× |
| Drop column, 50,000-row table | [Column/row identity preservation and atomic alteration across both index adapters.](../../test/component/alter.rs) | 9.60× | 17.86× |
| Rename table | [Catalog visibility, prepared rebinding, rollback and writer conflicts.](../../test/component/alter.rs) | 9.10× | 18.23× |
| GROUP BY i % 64 with SUM, COUNT and mask, 50,000 rows | [Hash/ordered aggregation across 48 compositions; typed group state and aggregate adapter replacement.](../../test/component/grouping.rs) | 1.23× | 1.81× |
| ROLLUP(i % 64) with SUM, COUNT and mask, 50,000 rows | [Subtotal/empty-group behavior, NULLs, grouping masks and independent state per grouping set.](../../test/component/grouping.rs) | 1.75× | 2.00× |
| CUBE(i % 64, i % 8) with SUM, COUNT and mask, 50,000 rows | [Independent cube oracle across dense, sparse and NULL key domains; masks and typed aggregate results.](../../test/component/grouping/domains.rs) | 1.52× | 1.22× |

## Exclusions and remaining acceptance gaps

- **ORDER BY ALL DESC on two integer columns, 50,000 rows**: release: 1.87× faster; development: 4.29× faster. The two sort implementations still need a shared conformance suite. It is not included in the qualifying table.

Grouping and recursion each run 48 combinations of two algorithms, two
optimizers, two expression evaluators, two result executors and three batch
sizes in their central SQL matrix, plus additional targeted contracts. The
older inspection's claim of 96 grouping combinations was incorrect. ALTER's
SQL matrix has 24 combinations of index, optimizer, executor and batch size.
Catalog and transaction implementation replacement remain unproven because
those major boundaries still have only one substantive implementation.

Full upstream case mappings/execution, broad SQL/types/functions/catalog
compatibility, current native-file compatibility and extension/client/UI parity
remain incomplete. The [SQL/catalog worklist](../sql-catalog-parity.md) and
[testing worklist](../testing-parity.md) describe those gaps. The earlier
[persistent reference campaign](../settings/reference-final.json) records
settings diagnostics/transaction differences and an empty grouping-set EXISTS
discrepancy. It predates this source and was not rerun for this inspection.
Earlier failed performance runs remain in their original directories.

## Current validation

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --offline --all-targets -- -D warnings`: passed.
- `cargo test --offline --all-targets`: **199 passed**, no failures or ignored tests.
- Release component tests for execution, grouping, recursion, ALTER, indexes,
  settings, SQL and types: **77 passed**, no failures or ignored tests.
- Python harness self-tests: **24 passed**; see [python-tests.log](python-tests.log).

The [validation manifest](validation.json) preserves each exact command, exit
code, duration, log and source fingerprint. These are local test counts, not
upstream DuckDB parity counts. Local compatibility tests include historical
fixtures; their success does not establish v1.5.5 file compatibility.

## Measurement scope

Release builds; one execution thread; in-memory data; three warmups and
21 samples per engine per workload; alternating engine order. Tests and builds
finish before timed samples. Every sample validates expected row count and
integer checksum. Component tests separately check values, NULLs, errors,
ordering and state behavior. Every median Rust/C++ ratio above 1.0 fails
independently. These speed ratios are observed sample medians, not statistical
significance claims; small margins do not establish a dependable advantage.

Timing includes execution and complete result consumption/checksum through each
engine's embedded API, so it includes result access costs. Setup, initial
preparation and process startup are excluded. DDL resets state before each sample,
times execution, result consumption and any required prepared-statement rebind,
and checks the mutation after timing. It measures in-memory DDL, not durable commit
latency. Results do not establish isolated operator kernel speedups.

Cold I/O, durable writes/recovery, concurrency, memory/CPU/I/O cost parity,
other client/tooling paths and extension interoperability have no speed claim
from this campaign. The benchmark suite remains small and synthetic.

## Absolute median latencies

All values are microseconds. Each C++ reference has its own paired Rust samples.

| Workload | Rust vs release | C++ v1.5.5 | Rust vs development | C++ development |
| --- | ---: | ---: | ---: | ---: |
| scan | 151.459 | 840.000 | 151.625 | 913.584 |
| filter | 521.750 | 541.709 | 552.708 | 687.917 |
| aggregate | 36.125 | 46.625 | 38.417 | 144.625 |
| point | 13.000 | 26.958 | 14.167 | 126.958 |
| limit | 11.041 | 174.916 | 20.333 | 4584.667 |
| correlated_exists | 334.708 | 1832.541 | 319.208 | 2194.750 |
| recursive_linear | 1340.917 | 8689.250 | 1376.084 | 2688.583 |
| recursive_cycle | 329.166 | 2162.834 | 318.708 | 899.875 |
| recursive_correlated | 255.958 | 353.125 | 254.042 | 288.334 |
| alter_add_column | 5.375 | 104.958 | 6.125 | 162.125 |
| alter_drop_column | 4.292 | 41.209 | 4.583 | 81.875 |
| alter_rename_table | 4.250 | 38.666 | 4.458 | 81.291 |
| grouped_sum | 373.000 | 457.542 | 387.125 | 700.125 |
| rollup_sum | 418.833 | 732.000 | 419.709 | 839.541 |
| cube_sum | 1069.833 | 1622.750 | 1067.042 | 1296.500 |
| order_all | 3455.208 | 6459.417 | 3438.750 | 14761.750 |

Raw samples, workload SQL, hashes, configurations and adapter selections:
[core-release.json](core-release.json),
[grouping-release.json](grouping-release.json),
[ordering-release.json](ordering-release.json),
[core-development.json](core-development.json),
[grouping-development.json](grouping-development.json),
[ordering-development.json](ordering-development.json).
The [machine-readable assessment](assessment.json) maps each workload to its tests and measurements.
