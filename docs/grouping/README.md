# Grouping implementation and verification

GROUPING SETS, ROLLUP, CUBE, GROUPING and GROUPING_ID use explicit logical
metadata and replaceable aggregation algorithms. Grouped integer SUM, ROLLUP
and CUBE now pass the zero-regression gate against both pinned C++ references,
along with the twelve existing native workloads. Full SQL, catalog, test and
performance parity remain unfinished.

The validation manifest records current Rust source SHA-256
`f52f51fa1ab0db25fd6e041738f36018191088d05158f31d3de9025f7a27678f`,
binary identities, commands, logs and evidence hashes. These results include
uncommitted code on the existing rewrite branch.

## Implementation and contracts

[`Aggregation`](../../src/planner/aggregation.rs) carries bound grouping
expressions, canonical ordinals for each grouping set, and aggregate or grouping
mask outputs. Sets retain independent state, including repeated empty sets.
Stored NULL keys and subtotal NULLs remain distinguishable through masks.
An empty grouping set produces a row on empty input. Metadata validation bounds
expansion and rejects invalid ordinals or masks before execution.

[`AggregationAlgorithm`](../../src/execution/operator/aggregate.rs) is selected
through `NativePhysicalPlanner::with_aggregation`. Hash and ordered aggregation
share SQL, expression effects, cancellation, row limits and snapshot contracts.
The ordinary row driver evaluates each grouping expression and each function's
arguments/filter once per input row, independent of set count. FILTER changes
updates while preserving group existence; DISTINCT state is independent per
function, group and set. Failure publishes no partial aggregate result.

[`GroupedAggregateState`](../../src/function/grouped.rs) is an optional function
interface with contiguous group ordinals, grow-only state, typed argument
columns and owned results. The default function implementation declines before
input is consumed. Opt-in updates promise no external effects or data-dependent
errors for valid input within the enforced update-count bound. Input order is
retained within each group; interleaving between groups/functions may change.
An alternative implementation backed by ordinary scalar states exercises the
same interface without changes to execution callers.

The hash algorithm selects a column path for total expressions, up to two
integer-capable keys per set, and opt-in functions without DISTINCT/FILTER.
Capabilities come from retained type adapters after logical validation. Each
set owns an adaptive integer index: a bounded dense domain plus sparse overflow
keys, with NULL positions represented independently. Other keys and signatures
retain the row driver. There is no cached query result or cross-cursor state.

Group destinations retain input order and optionally a bounded count histogram.
COUNT and flat integer SUM reuse counts without constructing a row permutation.
Constant destinations use the ordinary single-group SUM kernel. Every signed
SUM input through 64 bits fits each i128 prefix under the checked per-group
`usize::MAX` update bound on supported hosts. Broader inputs retain the existing
checked behavior. The unused grouped gather kernel and eager row-permutation
storage were removed. The ordinary narrow SUM kernel remains shared.

## Performance

References are source-built **v1.5.5** (`d8cdaa33fd`) and pinned development
**v2.0.0-dev84019** (`99063af2bd`). The harness checks source/executable identities
before SQL, then checks the linked library's source ID. All campaigns use the
same Rust source/binary, optimized builds, one execution thread, three warmups
and 21 alternating paired samples. Each sample checks expected row count and
checksum. The maximum Rust/C++ median ratio remains 1.0 per case.

| Reference | 50,000-row workload | Rust median | C++ median | C++ / Rust |
| --- | --- | ---: | ---: | ---: |
| v1.5.5 | Grouped SUM, COUNT and mask | 0.522 ms | 0.649 ms | 1.24× |
| v1.5.5 | ROLLUP with SUM, COUNT and mask | 0.415 ms | 0.719 ms | 1.73× |
| v1.5.5 | CUBE with SUM, COUNT and mask | 1.034 ms | 1.543 ms | 1.49× |
| Development | Grouped SUM, COUNT and mask | 0.522 ms | 0.923 ms | 1.77× |
| Development | ROLLUP with SUM, COUNT and mask | 0.408 ms | 0.791 ms | 1.94× |
| Development | CUBE with SUM, COUNT and mask | 1.049 ms | 1.259 ms | 1.20× |

Raw grouping results: release and
development. The existing twelve cases also
pass: core release and
core development.

Query timings include execution and complete result consumption through each
engine's embedded API. DDL timings include execution and any required rebind;
each sample resets the schema and verifies the resulting mutation outside
timing. Setup, initial preparation and process startup are untimed. These small
serial in-memory workloads do not establish cold-I/O, durable-write, concurrency,
memory, isolated-kernel, client or whole-engine performance parity.

The original row driver took 13.86–37.02 ms for the release grouping cases and
failed all three gates by 19–24×: release baseline
and development baseline. Intermediate
failed measurements remain in this directory. The
[preceding inspection](../quality-performance-current/README.md) still failed
grouped SUM against release and CUBE against development. No failed measurement
was removed, tolerance added or workload assertion weakened.

The [sample profile](grouped-profile.txt) is historical: it predates the fixed
integer index, column arithmetic dispatch and count-histogram changes, and
corresponds to the clustered initial build.
It identified grouping lookup, destination preparation and per-value arithmetic
dispatch as costs to investigate. C++'s clustered integer SUM updates informed
the work; the Rust representation ultimately benefits from sequential Value
reads and shared group counts.

## Correctness and compatibility

Current checks pass:

- **188 local tests**, including nine grouping tests, in the
  [complete debug run](debug-tests.log).
- **47 optimized contract tests** covering execution, grouping and types in
  the [release run](release-tests.log).
- **21 Python harness tests**, [log](python-tests.log).
- Formatting and Clippy with warnings denied: [format](format.log),
  [Clippy](clippy.log).

The 35-record local grouping corpus runs across two aggregation algorithms,
two optimizers, two expression evaluators, two executors and three batch sizes:
96 configurations. Tests check explicit values, masks, duplicate/empty/nested
sets, FILTER/DISTINCT, aliases, errors, prepared snapshots and atomic mutations.
Independent models cover cube results with dense/sparse keys, late extrema and
NULLs. Grouped states are compared with ordinary scalar states across integer
widths, vector encodings and batch sizes; COUNT additionally covers nullable
strings and cancellation of empty states. Malformed adapters cannot publish
incorrect group counts, result lengths or logical values.

The current reference campaign still fails overall:
**34 of 35 corpus records match each C++ reference**. Rust returns true for an
EXISTS query over two empty grouping sets with `LIMIT 1 OFFSET 1`; both C++
references return false. The local assertion remains unchanged. Earlier
direct probes reproduced the discrepancy with C++
optimization disabled, so it is not characterized as only an optimizer issue.
Separately, zero-argument GROUPING matches development but is rejected by
v1.5.5. Neither discrepancy is counted as a pass.

The current unchanged upstream campaign selects all
eight files under `test/sql/aggregate/grouping_sets/`: **two pass and six are
unsupported**, with no skipped assertions counted as passes. Five files stop
at `SET default_null_order='nulls_first'`; the sixth reaches four successful
records before a NATURAL/USING join. Slow and duplicate-empty-set files pass.
Missing configuration or join semantics must be implemented, not stripped from
the source tests. The remaining complete SQL/catalog objective is tracked in
[SQL and catalog parity](../sql-catalog-parity.md).

## Next configuration blocker

The independent ordering probes and
lifecycle probes establish the behavior needed
for real ordering settings. Both references support global defaults and SESSION
overrides; setting changes survive transaction rollback. RESET or SET DEFAULT
removes the selected override, so a reset SESSION value falls back to the global
value. Explicit order/null modifiers still override defaults. Direction-dependent
NULL ordering includes the SQLite/MySQL and Postgres aliases.

Both references reject SET LOCAL; it must not be conflated with SESSION. The
first lifecycle probe included unsupported LOCAL syntax and therefore cannot
establish the complete lifecycle; the second report uses supported SESSION
syntax. Multi-connection behavior, configuration-provider replacement and the
Rust implementation still need work. The general current_setting function and
broader configuration catalog are also unimplemented.
