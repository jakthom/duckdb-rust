# Numeric regression work

This is the regression follow-up to `760fd9a` and the
[binding corrections](binding-regressions.md). Full DuckDB parity is not
achieved. Development remains the correctness authority; each comparable
workload must match the faster pinned C++ reference. Both retained Rust medians
must pass that same reference median, not only the better Rust campaign.

## Implemented corrections

The selected exact numeric type adapter exposes `NumericCoefficient` equality
keys separately from signed integer ordering. Unsigned 128-bit payloads map
injectively by their bits; decimal keys use their coefficient within the bound
logical type. NULL remains separate. Grouping, joins, membership, sets and
window partitioning use this capability. The independent decimal-digit adapter
retains canonical byte keys and scalar comparison. No consumer downcasts an
adapter or infers signed arithmetic from numeric equality.

Type adapters can select comparison rows directly. Exact comparisons use
validated ascending numeric vectors for bounded searches and, when proved,
whole-batch acceptance/rejection. Other adapters default to their scalar
comparison. Logical validation, NULL rules, cancellation, and selection shape
checks remain at the boundary. Vector constructors establish physical ordering;
slices preserve it and selections require nondecreasing indices. This physical
fact does not grant an arbitrary SQL comparator an ordering shortcut.

Cast adapters have a checked batch default and an explicit totality capability.
Built-in unsigned widening casts and constant unsigned division/remainder use
column kernels. Fused widening/comparison requires the selected cast's exact
integer-preservation promise and the selected target's native ordering, and
declines additional target logical validation. Small power-of-two remainder
domains use physical dictionaries when that reduces storage; filtering and
grouping reuse total lookups for repeated dictionary entries. NULLs remain
distinct and groups are created at their first logical occurrence. Zero-divisor
errors follow development. Fallible, wide and unproved cases retain scalar
behavior. Borrowed family-name lookup and fixed-size operator candidate scoring
remove temporary binding allocations without changing precedence or ambiguity.

SUM uses physical coefficients only inside the selected built-in aggregate.
The aggregate interface can borrow one argument column; its default forwards to
the selected batch implementation, preserving custom adapters.
Declared domains prove safe accumulator prefixes; bounded blocks use machine-
width partial sums with a checked wide fallback. Grouped states additionally
enforce lifetime update counts before relying on their bounds. Signatures that
cannot promise error-free grouped updates decline that interface. Window SUM
uses actual partition bounds. Tests compare these paths with independent scalar
states, including NULLs, selected views, empty groups/frames and overflow prefixes.

Hash joins retain validated column ownership, inline singleton matches and
bounded dense equality domains, with sparse fallback for full-width keys.
Probe selections use machine-width row ordinals with a private unmatched
sentinel that cannot alias an index below the checked build cardinality.
Regression testing exposed an intermediate overflow when growing a dense index
at `i128::MAX`; the calculation is fixed and tested with four join kinds and
four batch sizes, including unsigned keys crossing the signed midpoint.

Whole-partition unsigned/decimal window results can use an owned
dictionary after all original outputs pass physical validation. Restricted pure
one-column expression trees reuse complete root results at their first logical
occurrence, preserving first-error order and TRY_CAST/NULL behavior. Signed
window results retain flat delivery because gathering them for ordinary result
materialization was slower in the regression matrix. Floats and scalar
callbacks are excluded. Single-root projections can use the checked
batch interface; multiple fallible roots retain inter-column row order.

These changes do not add a query-result cache or bypass selected adapter
semantics. Byte budgets, spill, concurrency and complete numeric/persistence
behavior remain open obligations.

## Retained performance trials

All trials use the unchanged eight-case numeric workload, three warmups,
alternating paired engine order and complete row/checksum validation. Trials
A–E use nine measured pairs; F onward use 21. Each trial retains separate
release/development samples and a combined faster-reference decision. Failed
trials are historical evidence, not overwritten by later measurements.

| Trial | Failed faster-reference workloads |
| --- | ---: |
| [Foundation](numeric-performance-fastest.json) | 7 |
| [A](numeric-performance-batched-a-fastest.json) | 6 |
| [B](numeric-performance-batched-b-fastest.json) | 6 |
| [C](numeric-performance-batched-c-fastest.json) | 6 |
| [D](numeric-performance-batched-d-fastest.json) | 5 |
| [E](numeric-performance-batched-e-fastest.json) | 5 |
| [F](numeric-performance-batched-f-fastest.json) | 4 |
| [G](numeric-performance-batched-g-fastest.json) | 3 |
| [H](numeric-performance-batched-h-fastest.json) | 2 |
| [I](numeric-performance-batched-i-fastest.json) | 2 |
| [J](numeric-performance-batched-j-fastest.json) | 3 |
| [K](numeric-performance-batched-k-fastest.json) | 2 |
| [L](numeric-performance-batched-l-fastest.json) | 1 |
| [M](numeric-performance-batched-m-fastest.json) | 1 |
| [N](numeric-performance-batched-n-fastest.json) | 0 |
| [O](numeric-performance-batched-o-fastest.json) | 0 |
| [P](numeric-performance-batched-p-fastest.json) | 0 |
| [Q](numeric-performance-batched-q-fastest.json) | 0 |
| [R](numeric-performance-batched-r-fastest.json) | 0 |

H and I fail decimal total and grouped total; J also fails decimal join. K
clears total and join, but still fails unsigned filtering and grouped total.
The initial dictionary producer exposed repeated lookup work in its consumers.
L clears filtering and still fails grouped totals. A short sampling profile of
the L worker identified remainder generation, group lookup and grouped updates
as the main costs; its temporary files were deleted. Subsequent source changes
remove repeated encoding dispatch in remainder generation. Profiling timings
are not acceptance measurements.
Release-only passes do not clear a case when the Rust median from the
development campaign exceeds the faster C++ median. Sampling variability is
not a percentage allowance. N passes all eight numeric cases; both Rust
medians pass the release baseline, which is faster for all eight in that run.

The first older-workload refresh nevertheless finds six failures:
[correlated recursion](numeric-batches-native-fastest.json),
[grouped SUM and CUBE](numeric-batches-grouping-fastest.json), and
[USING inner/full joins and partitioned SUM](numeric-batches-relational-fastest.json).
[ORDER BY ALL](numeric-batches-ordering-fastest.json) passes. These results
prevent a push despite the numeric subset passing. Subsequent changes restore
small-build join preselection, retain flat signed window delivery, inline
compact key helpers and decline whole-batch proof overhead for one-row inputs.
The O–R campaigns below repeat all 34 workloads after subsequent source changes.

The O refresh reduces the older matrix to three failures: grouped SUM (1.079),
CUBE (1.117), and partitioned window SUM (1.021). The P refresh clears all three:
grouped SUM's worst ratio is 0.871, CUBE's 0.840, and partitioned SUM's 0.997.
A sampling profile identified repeated key dispatch and out-of-line nullable
tuple copies in grouping. The direct signed SUM path is restored, signed key
decoding is selected outside the row loop, and small lookup helpers are inlined.
Temporary sampling files are deleted; sampling durations are not acceptance
evidence. Signed extrema are added to the independent scalar aggregate/window
comparisons.

P nevertheless fails [correlated recursion](numeric-batches-p-native-fastest.json)
(1.012) and [USING inner](numeric-batches-p-relational-fastest.json) (1.319).
Its [grouping](numeric-batches-p-grouping-fastest.json),
[ordering](numeric-batches-p-ordering-fastest.json), and numeric groups pass.
The follow-up remained unpushed. The next changes decline dictionary analysis
for single-row input, move signed membership dispatch outside the row loop, and
use constructor-proved bounded dense offsets without repeated checked-conversion
chains. Full-width outliers still cannot alias in-range keys.

Q clears recursion and the inner join but fails
[partitioned SUM](numeric-batches-q-relational-fastest.json) at 1.043; the other
33 cases pass. Its short sampling profile shows repeated inline-value copying
and partition-vector growth. The next source change specializes copying for
physically validated non-NULL signed flat vectors and reserves a bounded initial
capacity only for occupied dense window buckets. Skew retains ordinary growth;
no partition ordering or SQL key semantics change. The copy contract tests cover
all signed widths, extrema, destination prefixes, NULLs, slices, selections and
empty views. Temporary sampling files are deleted.

## Final performance refresh

R passes all **34 workloads** against the faster pinned reference. Both Rust
campaign medians pass the same smaller C++ median for every case. The ten
source reports retain 21 paired measurements per workload/reference, three
warmups, alternating engine order, and complete result/checksum checks.
All campaigns ran serially without concurrent builds or diagnostic workloads.

| Workload group | Passing cases | Largest Rust/faster-C++ ratio |
| --- | ---: | ---: |
| [Numeric](numeric-performance-batched-r-fastest.json) | 8/8 | 0.971 |
| [Original engine workloads](numeric-batches-r-native-fastest.json) | 12/12 | 0.972 |
| [Grouping](numeric-batches-r-grouping-fastest.json) | 3/3 | 0.882 |
| [Ordering](numeric-batches-r-ordering-fastest.json) | 1/1 | 0.736 |
| [Relational](numeric-batches-r-relational-fastest.json) | 10/10 | 0.890 |

Table ratios are rounded upward to three decimals; the gate uses exact medians.
The previously failing grouped SUM, CUBE, inner join, and partitioned SUM now
have worst ratios of approximately 0.882, 0.836, 0.700 and 0.849. Development
is faster for unsigned scan, CUBE, and correlated recursion in this campaign;
baseline selection remains per workload, not global.

All ten source reports share performance source fingerprint
`7234df8f2fd5ce28b07b8efaf01ea030c1b294f5bf688a54be10778289f9dbe4`
and measurement-worker SHA-256
`4add0a00b2ad8a2f09ded65b6765d3594f39ce8e1ea6f3bb9c094c9a8385121c`.
This establishes only the measured serial in-memory latency scope on this host.
It does not prove complete workload, memory, I/O, concurrency or native-file
parity. Every preceding failed campaign remains retained.

## Refreshed correctness evidence

The [final session report](binding-batches-reference-final.json) repeats all
92 binding/relational assertions against each C++ reference and passes, including
the unchanged upstream VALUES and schema/CTE regressions. It verifies that both
Rust source and worker identities remain unchanged throughout the run.

The [final numeric reference report](numeric-reference-batches-final.json)
matches development for all 38 selected typed SQL/error cases. Release matches
20/38; the same 18 documented reference-version divergences remain and Rust
continues to follow development. All three release persistence scenarios pass.
The three development persistence scenarios still fail (storage version 999 and
internal catalog errors reading Rust checkpoints/WAL), just as at the foundation
checkpoint. These are unresolved compatibility gaps, not passing cases or new
batch regressions. This command correctly exits nonzero for those gaps.

The [full upstream refresh](upstream-parity-batches-final.json) and its
[journal](upstream-parity-batches-final.jsonl) account for all 5,638 identities:
**313 passed**, 1,877 failed, 3,437 unsupported, eight timeouts and three
incomplete files. The 17,018 passed records include prefixes of failed files.
Every file retains the same status and passed-record count as the valid
binding-only checkpoint; no previously passing file is lost. The three-second deadline, two workers and
original assertions are unchanged. The worker hash was checked again after
completion and matches the recorded identity. Full parity remains false.

## Earlier N verification checkpoint

At the N source checkpoint (`f806c9abea2056779effa5cf0be1b4e765dd22b108fa0f422a5aab0defee9f05`
under the performance fingerprint), the workspace test suite passes with two
external-CLI analytics tests ignored. All 34 Python harness tests pass.
Instrumentation coverage reports 213 files, 1,855 functions, 195 interface
methods and no missing attributes. Tracing compatibility checks pass and remove
their temporary telemetry. Ordinary check/test/clippy passes also accompany the
subsequent older-workload fixes.

`python3 scripts/verify_kani.py` with Kani 0.67.0 verifies all five maintained
harnesses at that checkpoint. The new proofs cover full-width unsigned key
identity/NULL separation and one-dimensional dense group offsets against an
independent checked-arithmetic oracle. The latter assumes the constructor's
bounded width and non-wrapping maximum, leaves the key unrestricted, and uses
unwind 2 for one iteration plus loop exit. It does not prove two-dimensional
grouping or complete SQL behavior. The earlier packed-size and window-bounds
proofs also pass. Verification takes approximately 0.64, 0.48, 2.13, 2.27 and
104.07 seconds respectively. Unsupported foreign/caller-location constructs
are not reachable failures in these harnesses; atomics are modeled sequentially,
not as a concurrency proof. The final source receives a separate checkpoint below.

## Final R verification checkpoint

The production source fingerprint still matches the R timing reports after
the serial SQL/native/full-upstream campaigns and verification checks.

- Ordinary `cargo check`, focused `cargo test`, and
  `cargo clippy --workspace --all-targets -- -D warnings` pass.
- `cargo test --workspace --no-fail-fast`: **260 passed, zero failed, two
  ignored** external-CLI analytics tests. The numeric target has 24 passing
  component tests. An initial new copy-test fixture incorrectly called a helper
  that requires nonempty source values with an empty source; that test setup
  was corrected. Empty vector views remain explicitly covered.
- `cargo dev coverage`: 213 files, 1,858 functions, 195 interface methods, no
  missing instrumentation. `cargo dev trace check --workspace --all-targets`
  passes in 30.22 seconds and deletes temporary telemetry.
- `python3 -m unittest discover -s scripts -p 'test_*.py'`: 34 passed.
- `cargo build --release --no-default-features` and
  `cargo fmt --all --check` pass. The root README remains identical to
  `origin/main`; the user's Kani changes are preserved.
- `python3 scripts/verify_kani.py`, pinned 0.67.0: **five verified, zero
  failures**. Unsigned keys, dense offsets, ROWS bounds, uniform bounds and
  packed sizes take approximately 0.64, 0.48, 2.15, 2.50 and 82.63 seconds.
  The bounds and limitations described above still apply. Foreign/caller-location
  warnings do not become reachable failures; sequential atomics do not prove
  concurrency. No full binding, SQL or storage correctness claim follows from
  these five harnesses.

The earlier binding-only reports remain historical; the fresh reports above
validate the final batch source. Full parity remains open despite clearing the
observed regressions in this follow-up.
