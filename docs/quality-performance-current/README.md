# Quality and performance inspection before group-count simplification

This is a historical inspection. The subsequent
[group-count implementation and measurements](../grouping/README.md) remove
the two regressions recorded below. The original evidence remains unchanged.

Recorded 2026-09-09 for Rust source SHA-256
`adb5a8d8637fc07255fbb53318d369c05e666bd3587ccffde443ae92358dda69`
and measurement binary SHA-256
`73cba687d1251bda34839797577884d83356635fbca7b721422c1fe8cffa23b0`.
The source fingerprint includes the uncommitted implementation and measurement
worker. This inspection changed no engine code.

Thirteen measured workloads pass their relevant local correctness and interface
checks and have lower median latency than both pinned C++ references. The table
describes the tested contracts for each workload. Full subsystem acceptance under
the [rewrite principles](../../specs/rewrite-principles.md) and complete DuckDB
test, feature and performance parity remain unfinished.

The references are **v1.5.5**, revision
`d8cdaa33fda8df955cc76ef58a280f68f4cd43fa`, and **v2.0.0-dev84019**, revision
`99063af2bd7092aff02e14184a20e24699d34d71`. The latter is the pinned development
checkout, with no claim that it is the latest remote revision. Each campaign
checks the source checkout and executable version before SQL and records the
loaded library identity.

## Workloads faster than both C++ references

Speedup is C++ median latency divided by Rust median latency. The measurements
use the default Rust composition; alternative adapters are exercised by the
contract tests and do not inherit these timing results.

| Measured workload | Quality/functionality checked | Speedup over v1.5.5 | Speedup over development |
| --- | --- | ---: | ---: |
| Full scan, 50,000 BIGINT rows | [Owned batches, row identities, snapshot visibility and delivery contracts](../../test/component/execution/batches.rs) | 5.72× | 6.14× |
| Filter `i % 2 = 0`, 50,000 input rows | [Scalar/batch equivalence, NULL propagation, lazy errors and effects](../../test/component/execution/expressions.rs) | 1.06× | 1.27× |
| Ungrouped integer `SUM`, 50,000 rows | [Scalar/batch equivalence across widths, NULLs, encodings and overflow boundaries](../../test/component/execution/batches.rs) | 1.31× | 3.63× |
| Indexed equality lookup, 50,000-row table | [Hash/B-tree contracts, atomic index publication, restart and rollback](../../test/component/indexes.rs) | 2.09× | 9.40× |
| Scan with `LIMIT 256` | [Demand bounds, consumer stop, cancellation and retained result ownership](../../test/component/execution.rs) | 19.06× | 391.00× |
| Correlated equality `EXISTS`, 50,000 outer rows | [Hash/nested-loop equivalence, NULLs, key domains, batches and cancellation](../../test/component/execution/joins.rs) | 5.66× | 6.50× |
| Recursive `UNION ALL`, 1,000 iterations | [Streaming/materializing recursion, scope, iteration and resource contracts](../../test/component/recursive.rs) | 6.47× | 2.03× |
| Recursive `UNION`, 257-value cycle | [Duplicate elimination and fixed-point semantics across recursion adapters](../../test/component/recursive.rs) | 6.96× | 2.84× |
| Correlated recursive CTE, 16 outer rows | [Outer scope, independent recursion state and prepared snapshot behavior](../../test/component/recursive.rs) | 1.37× | 1.20× |
| Add a column with literal default, 50,000-row table | [Atomic catalog/data publication, retained snapshots, rollback and WAL recovery](../../test/component/alter.rs) | 20.11× | 26.49× |
| Drop a column, 50,000-row table | [Column/row identity preservation and atomic alteration across both index adapters](../../test/component/alter.rs) | 10.28× | 19.34× |
| Rename a table | [Catalog visibility, prepared rebinding, rollback and writer conflicts](../../test/component/alter.rs) | 9.85× | 20.22× |
| `ROLLUP(i % 64)` with SUM, COUNT and grouping mask, 50,000 rows | [Hash/ordered aggregation across 96 compositions, NULLs, empty groups, masks and aggregate adapter replacement](../../test/component/grouping.rs) | 1.46× | 1.64× |

## Workloads that pass only one performance baseline

Their local correctness and contract tests pass. Both references are required by
the acceptance criteria, so neither workload qualifies for the table above.

| Workload | Against v1.5.5 | Against development |
| --- | ---: | ---: |
| `GROUP BY i % 64` with SUM, COUNT and grouping mask | **0.92% slower; fails** | 1.49× faster |
| `CUBE(i % 64, i % 8)` with SUM, COUNT and grouping mask | 1.24× faster | **3.36% slower; fails** |

The preceding [same-source release measurement](../grouping/sequential-release.json)
also failed grouped SUM by 1.12%. It is retained along with all earlier failed
measurements. No acceptance allowance was added and no failed result was removed.

## Validation and measurement scope

The current source passes `cargo fmt --all -- --check`,
`cargo clippy --offline --all-targets -- -D warnings`, and
`cargo test --offline --all-targets`: **187 passed, 0 failed, 0 ignored**.
The [validation manifest](validation.json) identifies the source and logs:
[formatting](check-1.log), [Clippy](check-2.log), and [Rust tests](check-3.log).
These are local tests, including contract matrices; the count is not an upstream
DuckDB suite pass count.

All four campaigns use the same Rust source and binary, optimized builds, serial
in-memory execution, three warmups and 21 samples per engine per workload. Engine
order alternates. Every sample checks the expected row count and integer checksum;
the contract tests additionally check values, errors and state behavior. Every
median ratio above 1.0 fails independently. The tests finished before benchmarking.

Query timings include execution and complete result consumption through each
engine's native embedded API, including per-value checksum access. They therefore
measure that API path as well as the engine. Setup, initial preparation and process
startup are excluded. DDL resets state before every sample and checks the resulting
mutation after timing; execution and any required prepared rebind are timed.

Cold I/O, durable commit latency, concurrency, memory/CPU/I/O cost parity, isolated
operator kernels, client/UI behavior and extension compatibility are outside these
measurements. Small synthetic workloads on this host do not establish performance
parity for whole subsystems or other workloads. Broader grouping compatibility has
[known reference differences](../grouping/reference.json) and
[unported upstream obligations](../grouping/upstream.json); those previous campaigns
are separate evidence and were not rerun for this inspection.

The complete raw samples, workload SQL, expected results, build identities and
adapter selections are preserved in [core release](core-release.json),
[core development](core-development.json), [grouping release](grouping-release.json)
and [grouping development](grouping-development.json).
