# Relational functionality port

This increment implements three engine capabilities in Rust. Full DuckDB test,
file, client, extension and performance parity remains open. No C++ library is
used by these operators; C++ is the external compatibility and timing reference.

This report retains the earlier relational checkpoint. The subsequent
[binding regression follow-up](binding-regressions.md) adds schema/CTE namespace
resolution, fixes a previously passing upstream case and records fresh assertion
evidence. The timings below are historical; the
[numeric batch follow-up](numeric-batches.md) records the latest all-34-workload
refresh, including these ten relational cases, against both pinned references.

| Capability | Implemented behavior | Principal code |
| --- | --- | --- |
| NATURAL and USING joins | Inner, outer, semi and anti joins; merged keys and original qualified keys; wildcard order; chained joins; derived aliases and correlated scopes; type coercion and NULL padding | [Binding scope](../src/planner/binder/scope.rs), [table binding](../src/planner/binder/table.rs), [hash join](../src/execution/operator/join/hash.rs) |
| Set operations | UNION, INTERSECT and EXCEPT with DISTINCT/ALL multiplicities, NULL equality, coercion and nested operations | [Set algorithms](../src/execution/operator/set.rs) |
| Window functions and QUALIFY | Ranking, distribution, NTILE, LEAD/LAG, FIRST/LAST/NTH_VALUE and registered aggregates; partitions, ordering, named windows, constant ROWS/GROUPS frames, default/current/unbounded RANGE frames, DISTINCT, FILTER, value-function NULL treatment and QUALIFY | [Window plan](../src/planner/window.rs), [binding](../src/planner/binder/window.rs), [execution](../src/execution/operator/window.rs), [functions](../src/function/window.rs) |

## Interfaces and implementation

Name resolution distinguishes physical columns, visible wildcard columns and
merged USING bindings. FULL joins produce a real coalesced key column. Qualified
source keys remain addressable; derived aliases expose their visible projection.
Internal column identities are ordinals, not fabricated SQL names.

`SetAlgorithm` has hash and ordered-map implementations. `WindowAlgorithm` has
hash-partitioned and sorted-partition implementations; both accept replaceable
sort algorithms and registered `WindowFunction` implementations. Physical
composition selects these adapters with `with_sets`, `with_windows` and the
existing `with_joins`. The same conformance cases exercise both implementations,
different evaluators/optimizers/executors and batch sizes 1, 3 and 2048.

Window preparation evaluates arguments and keys once, preserving effects in row
order. Partition row views borrow evaluated payloads. Uniform frame/peer bounds
are stored once. Function cardinality, physical/logical types and sort row
identities are checked. QUALIFY runs before result projection, so an expression
on a discarded row does not run. Volatile window calls have distinct results.

Hash joins retain the build side and stream the probe side, including duplicate
matches across demand boundaries. Typed integer key capabilities permit compact
indexes; other types retain registered key semantics. Set intersections stop
probing an exhausted build while still consuming checked input, preserving later
errors and effects. Unordered frame-independent ranking can stream. Aggregate
windows use optional exact prefix/partition evaluation through the ordinary
aggregate interface, with a generic frame-state implementation as the fallback.

Every cursor owns its progress. Returned chunks survive cursor/database drops.
Cancellation, terminal errors and row budgets are covered by component tests.
Byte accounting, spill and parallel window execution remain unimplemented.

## Upstream cases and boundaries

These files were copied unchanged from the pinned development checkout:

| Rust corpus | Original C++ source case |
| --- | --- |
| `test/sql/using-chain.test` | `test/sql/join/inner/test_using_chain.test` |
| `test/sql/nested-except.test` | `test/sql/setops/test_nested_except.test` |
| `test/sql/union-except-empty.test` | `test/sql/setops/test_union_except_empty.test` |
| `test/sql/window-binding.test` | `test/sql/window/test_window_binding.test` |

Additional SQL and component tests cover multiplicities against independent
counts, window frames against independent sums and a separate aggregate-state
adapter, malformed adapter results, duplicate demand, cancellation, resource
limits, expression effects, prepared queries and result ownership. Retaining the
complete original upstream archive is not equivalent to executing that suite.

UNION BY NAME, finite RANGE value offsets, dynamic frame offsets, EXCLUDE, ordered window
arguments, complete argument coercion parity, additional function/type families,
lateral joins and the remainder of
DuckDB's SQL surface are still separate porting work. These changes do not
establish full file-format, diagnostic formatting or extension ABI compatibility.

The references have explicit differences. Development accepts correlated columns
inside window calls (`test/sql/windows-development.test`); v1.5.5 rejects them.
Development also accepts an alias on an entire parenthesized USING join
(`test/sql/using-development.test`) where
v1.5.5 rejects the alias. Rust follows development for those cases. Raw floating
text formatting remains a UX gap (for example `0` versus `0.0`); distribution
functions also have typed floating-result assertions.

Window positions are checked against the signed BIGINT domain before row/peer
arithmetic. The references differ in coercion of wider integer and floating
position arguments, so full function-overload/coercion compatibility remains open.

## Validation evidence

Compatibility and performance campaigns use v1.5.5 at `d8cdaa33fd` and development
at `99063af2bd`, separately. Each campaign records executable/library hashes,
source identity, adapter choices, unchanged assertions or result checks, and its
complete outcomes. Production timings exclude tracing and compare each workload
independently at a maximum median ratio of 1.0. The new workloads use 50,000 input
rows, one thread, three warmups and nine paired, alternating samples. Setup and
initial preparation are outside the timed execution/materialization interval;
this does not establish planning, memory, I/O, concurrency or complete performance
parity.

The workspace suite, additional external-CLI analytics tests and component SQL
checks pass. `cargo dev coverage` reports no missing instrumentation; traced
workspace compilation is checked separately. Managed trace checks delete their
temporary telemetry. The README remains identical to main.

The [compact validation record](relational-validation.json) retains reference and
source identities, assertion counts, every timing sample and the earlier failed
performance campaigns. It contains test/benchmark evidence, not operation traces.
The final shared SQL campaign passes **138 records against each reference**,
including 37 records from the four unchanged upstream files. Two additional
development-specific records pass Rust and development and fail the release
reference as described above; they are not counted in the shared 138.

All **26 measured workloads per reference** pass the median-ratio gate: ten new
relational cases and the sixteen existing native/grouping/ordering cases. The
final campaigns use the same Rust source identity. The new case medians are:

| Workload | Rust ms, release campaign | C++ v1.5.5 ms | Rust/C++ | Rust ms, development campaign | C++ development ms | Rust/C++ |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| USING inner join | 0.132 | 0.142 | 0.931 | 0.165 | 0.427 | 0.386 |
| USING full join and sum | 0.184 | 0.393 | 0.469 | 0.171 | 0.525 | 0.326 |
| NATURAL semi join | 0.109 | 0.127 | 0.863 | 0.115 | 0.313 | 0.368 |
| INTERSECT DISTINCT | 0.040 | 0.273 | 0.148 | 0.041 | 0.404 | 0.102 |
| INTERSECT ALL | 2.111 | 6.551 | 0.322 | 2.149 | 15.301 | 0.140 |
| EXCEPT ALL | 2.194 | 6.883 | 0.319 | 2.186 | 15.459 | 0.141 |
| Unordered ROW_NUMBER | 0.371 | 0.968 | 0.383 | 0.348 | 0.878 | 0.396 |
| Running SUM | 2.312 | 4.898 | 0.472 | 2.326 | 9.078 | 0.256 |
| Partitioned SUM | 1.611 | 1.733 | 0.929 | 1.605 | 2.004 | 0.801 |
| Partitioned ranking with QUALIFY | 5.432 | 6.711 | 0.810 | 5.273 | 14.931 | 0.353 |

These are scoped latency measurements. In particular, the small margin on
partitioned SUM is not evidence of universal performance parity.
