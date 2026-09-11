# Aggregation and correlated EXISTS regressions

References: **v1.5.5** (`d8cdaa33fda8df955cc76ef58a280f68f4cd43fa`) and
**v2.0.0-dev84019** (`99063af2bd7092aff02e14184a20e24699d34d71`). Both are
pinned C++ source builds; the [reference runbook](../reference-builds.md) records
their configuration. The Rust engine does not link either reference library.

## What the C++ review established

Both [release](release-plan.txt) and [development](development-plan.txt) choose
`sum_no_overflow` for the measured `SELECT sum(i) FROM t`. The release's
[`SumPropagateStats`](../../../duckdb-v1.5.5/extension/core_functions/aggregate/distributive/sum.cpp)
proves that the value range times maximum cardinality fits an int64 accumulator.
It then selects a narrower aggregate implementation with a HUGEINT SQL result.
Development retains that optimization and adds clustered updates and local
int64 partial sums in its
[`sum.cpp`](../../../duckdb/extension/core_functions/aggregate/distributive/sum.cpp).
The history includes `e8ce60ced58c5552e4bdfc0e723efde308700d04`, which switched
aggregation from simple updates to clustered updates. The narrowed sum used by
this benchmark was already present in v1.5.5.

For correlated EXISTS, v1.5.5 plans a delim semi join, a grouped correlation
input, and hash joins. Development uses a CTE-based correlation plan and a MARK
join followed by a filter. Its
[`DelimJoinCTERewriter`](../../../duckdb/src/planner/subquery/delim_join_cte_rewriter.cpp)
materializes the correlation and distinct inputs as CTEs; commit `88e074226a`
moved that rewrite into its own module. Neither plan reruns the entire subquery
independently for every outer row. The release's
[`ScanKeyMatches`](../../../duckdb-v1.5.5/src/execution/join_hashtable.cpp)
probes a batch, marks matches, and selects left rows without duplicating them.
Its separate
[`PerfectHashJoinExecutor`](../../../duckdb-v1.5.5/src/execution/operator/join/perfect_hash_join_executor.cpp)
uses compact integer domains for direct addressing. That executor is restricted
to inner joins in this revision; it is an architectural lesson for Rust's
membership set, not a claim that this EXISTS plan uses it.

The earlier reports do not isolate a release-to-release C++ speedup. Identical
Rust source and binary hashes had substantially different absolute timings
between those runs. A fresh pre-change measurement
reproduced the actual regressions against v1.5.5: SUM was 10.7% slower and EXISTS
9.6% slower. The failed measurements remain failures in the retained history.

## Rust changes

- [`AggregateState`](../../src/function/aggregate.rs) uses independent checked
  int64 partial accumulators for flat non-NULL narrow integers. Partial overflow
  falls back to int128 for that block. The existing prefix-bound proof preserves
  SQL overflow behavior near either HUGEINT limit; other encodings and types
  keep the ordered checked path. No planner statistics are assumed.
- [`ScanBatch`](../../src/storage/scan.rs) can retain a checked view of shared
  row identities, just as it retains columns. The
  [CPU profile](aggregate-profile.txt) located copying in the aggregation scan
  path. Removing that copy preserves physical IDs, snapshot isolation, demand,
  and ownership after cursor destruction.
- [`MembershipBuilder`](../../src/execution/operator/join/membership.rs) seals
  a right-side equality set once per semi/anti cursor. An explicit
  [`TypeAdapter` capability](../../src/common/type_registry.rs) permits integer
  identity keys; dense domains become bounded bitmaps, while sparse domains use
  typed hashing. Canonical byte keys remain the default, including adapter
  normalization, validation, and failures. Rust already decorrelated this query;
  this change removes serialization and generic byte hashing from its probe loop.

The integer capability is retained by the bound type and cannot be advertised
for a non-integer physical type. The bitmap is capped at 128 KiB and eight bytes
per distinct key. Negative values, holes, duplicates, NULLs, and overflow in
offset calculations preserve semi/anti semantics. These are private details of
the replaceable hash-join adapter; nested-loop joins use the same public contract.

## Validation and measurement history

`cargo test --offline --all-targets` passed **178 tests**, with no failures or
ignored tests. All **51 execution, type, and subquery tests** also passed in an
optimized release build. `cargo clippy --offline --all-targets -- -D warnings`
passed. The validation record retains commands, test logs,
counts, source identities, and artifact hashes.
New conformance checks compare scalar and batch SUM across signed widths,
overflowing partials, NULLs, constants, slices, selections, and batch boundaries.
Hash and nested-loop membership results agree across dense, sparse, empty,
nullable, and extreme HUGEINT domains. Replacement adapters verify both key
representations and preserve canonical-key failures. Scan tests retain identity
views after the original identity allocation is changed and dropped.

The original twelve workloads, setup, worker timing contract, three warmups,
21 paired samples, and maximum Rust/C++ ratio **1.0** are unchanged. Every sample
materializes results and checks row counts and checksums. Builds and tests were
finished before the final sequential measurements.

Retained development steps are before,
membership with a single narrow accumulator, and
independent partial sums. The first two fail; the
third passed with only a 0.9% aggregation margin. Profiling identified the scan
copy as remaining work before the final comparisons. These reports have not
been overwritten or relabeled as final results.

The final reports are v1.5.5 and
development. They record source and binary
hashes, reference identities, every sample, and each workload's result.
**All twelve workloads pass against each reference.** The same Rust source and
binary hashes appear in both final reports.

| Reference | Workload | C++ median, µs | Rust median, µs | Rust / C++ |
| --- | --- | ---: | ---: | ---: |
| v1.5.5 | SUM | 48.750 | 35.917 | 0.737 |
| v1.5.5 | Correlated EXISTS | 1,732.625 | 314.416 | 0.181 |
| Development | SUM | 167.417 | 40.750 | 0.243 |
| Development | Correlated EXISTS | 2,292.208 | 348.958 | 0.152 |

Full performance and test parity remain unproven: these twelve serial in-memory
cases do not cover general grouped aggregation, all subquery shapes, cold I/O,
concurrency, or the full DuckDB workload suite. Existing file and ALTER
compatibility failures in the combined campaign
remain unresolved by this change.
