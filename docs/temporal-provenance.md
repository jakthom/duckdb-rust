# Temporal execution provenance checkpoint

This bounded follow-up closes the four remaining calendar-difference constant-
vector gaps from [the previous checkpoint](temporal-differences.md). It does not
complete the value-and-expression milestone, stored DEFAULT IR, ICU functions,
native compatibility, or performance parity.

The clean integration base is `9e19237`. Shared interface prerequisite `75c4ad7`
adds defaulted selected callbacks; runtime `0ed1f18` carries actual execution
metadata; family `3d9e2ce` consumes it in `date_diff`, `datediff`, `date_sub`, and
`datesub`. Representations and execution algorithms remain provisional.

## What changed

The selected evaluator now distinguishes a physical Constant argument from an
unknown/flat/dictionary argument after evaluating it. Literals, parameters,
constant projections, and already materialized statement-local scalar-subquery
results retain Constant. A single VALUES row, equal repeated VALUES rows, and
correlated scalar-subquery results do not acquire that encoding. Selection keeps
an existing constant; ORDER BY materialization can flatten it.

Selected functions receive metadata through a defaulted callback that preserves
ordinary replacements. A replacement evaluator must opt into encoding metadata;
its ordinary value-only callback defaults to Unknown. Children are not evaluated
again to discover metadata. Every requested row remains evaluated in order, with
logical validation before constant normalization discards later payloads. The
batch-invariance assertion is an explicit evaluator contract, not SQL equality.

Calendar difference dispatch no longer substitutes binder closedness for this
runtime information. Consequently a projected or scalar-subquery `'bad'`
specifier raises the development Conversion error even with infinite endpoints;
the VALUES equivalent still returns NULL.

An adjacent source-backed NULL boundary also required repair. The explicit
`NullOnConstant` argument policy stops at an executed Constant NULL, after
selected child/output metadata validation. It skips later children and the
callback, but preserves earlier effects and failures. Ordinary Eager callbacks
and unknown/flat NULL retain their previous behavior. Development returns NULL
for these projected-NULL cases; release still evaluates and rejects them.
Development is the correctness authority.

Sources: [development scalar execution](../../duckdb/src/execution/expression_executor/execute_function.cpp),
[constant slicing](../../duckdb/src/common/types/vector_buffer.cpp),
[date_diff](../../duckdb/extension/core_functions/scalar/date/date_diff.cpp), and
[date_sub](../../duckdb/extension/core_functions/scalar/date/date_sub.cpp).

## Retained paired trials

The narrow driver compares exact result types/values or complete error messages.
It does not normalize away error categories or diagnostic suffixes. Both pinned
source revisions, executable/library hashes, worker hashes, source fingerprints,
SQL, and failures are retained. Reference sources/binaries were not modified.

| Trial | Development | Release |
| --- | --- | --- |
| [Initial physical-provenance corpus](temporal-provenance-initial.json) | 60 / 88 | 60 / 88 |
| [Runtime dispatch repaired](temporal-provenance-runtime.json) | 88 / 88 | 88 / 88 |
| [Expanded constant-NULL initial](temporal-provenance-null-initial.json) | 92 / 112 | 104 / 112 |
| [Development NULL policy repaired](temporal-provenance-null-repaired.json) | 112 / 112 | 92 / 112 |

The twenty remaining release disagreements are intentional development-policy
choices, not accepted development failures. The earlier release initial trial
also retained its query-location suffix differences on failing DATE casts.
The exact driver therefore still returns nonzero for the repaired paired run:
its overall flag requires agreement with both references, not just the
development correctness authority. No failed trial was overwritten or relabeled.

The [full temporal-family refresh](temporal-provenance-family-reference.json)
passes 1,288 / 1,289 development SQL cases and 1,206 / 1,289 release cases. Every
previously passing case remains passing: development gains exactly the four
recorded gaps (1,284 to 1,288), as does release (1,202 to 1,206). This broader
legacy driver compares rejection presence, not exact error categories; the
narrow provenance driver above retains the stronger exact-message comparison.

Rust checkpoint and Rust WAL producers still pass against both pinned readers
(2 / 3 producer workflows per reference). The development C++ producer still
stops at parsed function DEFAULT class 9/kind 140; release still lacks the
existing TIME-to-TIME_NS fixture conversion. The single development SQL failure
remains the raw unrenderable timestamp DEFAULT witness, not a provenance case.

## Connected verification

Six shared provenance contract tests cover both evaluator implementations,
identity/pipeline optimizers, batch sizes 1/2/5, scalar and prepared execution,
constant versus dictionary selections including zero rows, ordinary-only and
metadata-aware replacements, declared effects, lazy children, and cancellation.
A deliberately dishonest Constant result with an invalid later physical or
ASCII logical value still fails validation; later Resource/Internal failures
also remain failures. Constant-NULL short-circuiting validates both selected
input and output type metadata rather than hiding unavailable adapters.

Two temporal tests cover the four aliases and both DATE/TIMESTAMP endpoints,
projected/subquery/VALUES/ORDER BY distinctions, prepared rebinds, failed writes,
rollback, successful updates, joins, grouping, windows, indexed lookup, native
WAL recovery, checkpoint, and read-only reopen. All 31 temporal tests and the six
focused contract tests pass. No test was newly ignored. Full checkpoint results
are recorded below.

The full frozen-source workspace suite passes, with only the two preexisting
external-CLI analytics tests ignored. The complete contract suite has 38 tests.
Workspace/all-target check and clippy pass. Coverage reports 331 files, 3,106
functions, and 220 interface methods with no missing instrumentation. The
all-target traced check passes in 46.50 seconds and deletes temporary telemetry.
All 38 Python verification-harness tests pass (120.66 seconds, including their
production worker build); those are harness correctness timings, not acceptance
benchmarks.

The full maintained `python3 scripts/verify_kani.py` checkpoint with pinned Kani
0.67.0 passes all six harnesses, with zero failures. Individual proof times are
55.002, 0.924, 0.504, 2.244, 2.716, and 101.694 seconds. Existing TIMETZ packing,
unsigned keys, dense grouping offsets, window bounds, and packed byte-count
assumptions are unchanged. No new provenance/calendar proof is claimed. Kani
models the warned atomics sequentially; warned foreign/caller-location
constructs must remain unreachable in the successful bounded proofs. The new
execution contracts remain tested and reference-checked, not formally proven.

## Still outside this slice

The raw unrenderable timestamp DEFAULT discrepancy and development parsed
function DEFAULT decoding remain separate integration-owned obligations. No
temporal function whitelist was added to native storage. General vector encoding
coverage, broader SQL/function completeness, IANA/ICU behavior, and isolated
performance acceptance remain open; no worker timing is performance evidence.
