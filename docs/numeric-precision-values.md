# Precision-aware rounding increment

The continuing numeric family now implements unary and binary `round`/`trunc`
and binary `round_even`/`roundbankers`, retaining ordinary selected overloads,
casts, exact integer/decimal coefficients, result metadata and floating widths.
Development `99063af2bd` is authoritative; release `d8cdaa33fd` is separately
retained. This is not full numeric/function or engine parity.

The source is `extension/core_functions/scalar/math/numeric.cpp`, including
`BindDecimalRoundPrecision`, integer and floating precision operators and the
two decimal precision directions. Required DECIMAL precision uses the selected
typed-constant frontend request, never a private default cast. Carry can widen
scale-zero DECIMAL metadata and crosses the selected input cast only at native
physical-width transitions. Integer rounding retains checked overflow, including
HUGEINT extrema; truncation retains all unsigned widths. The smaller truncation
overloads deliberately return zero at precision -19, including UBIGINT, matching
their reference NumericHelper cache cutoff. Floating precision preserves the
different negative-overflow policies of ROUND (zero) and TRUNC (input), signed
zeros and FLOAT-to-DOUBLE modifier arithmetic.

Three component tests check every DECIMAL width/scale with an independent
text-digit rounding oracle, integer extrema, constant/sliced/dictionary/flat
views, NULLs, overload metadata, selected precision-cast replacement, prepared
parameters, floating extremes, joins, aggregation, windows, nested/concat text,
primary keys, atomic failed mutations, rollback and private/native checkpoint,
WAL and reopen. Initial test compile errors came from incorrect test API names;
those were corrected against existing persistence tests. A signed-overflow bug
in the independent expected-value oracle was also corrected before the tests
passed. Full-width unsigned values are tested through quoted typed input; the
bare-literal parser mismatch is retained independently below.

The [initial paired campaign](numeric-precision-reference-initial.json) built
unchanged production source in 1m35s. Development matches 590/591 SQL cases;
release matches 262/591. All three native producer paths pass both pins. The
single development mismatch is the existing bare full-width integer literal:
`trunc(340282366920938463463374607431768211455::UHUGEINT,-38)` fails in Rust's
signed literal parser, whereas development returns exact UHUGEINT
300000000000000000000000000000000000000. It remains lead-owned frontend work.
Release disagreements include its older ROUND_EVEN macro/DOUBLE result and
older overflow behavior; development governs, and raw release failures remain.

An additional independent lazy-branch probe exposed a new prototype regression:
`SELECT CASE WHEN false THEN round(CAST('bad' AS DECIMAL(4,2)),1) ELSE 1 END`.
The initial production Rust CLI returns
`Conversion Error: Could not convert string "bad" to DECIMAL(4,2)`;
the development CLI succeeds with 1.0. The cause is using required-error
`constant_if_closed` evaluation merely to discover a NULL template. Development
instead uses `TryEvaluateScalar` in `src/function/function_binder.cpp:618-648`,
which leaves unsuccessful ordinary folds for runtime. This regression must be
repaired before delivery, not normalized away. The approved repair exposes the
existing selected operator NULL probe separately to scalar binding while
preserving required-constant error propagation.

Numeric 35, contracts 29, casts 12, temporal 25 and nested 31 tests pass on the
initial source, as do workspace/all-target check and clippy. Coverage reports
305 files, 2,803 functions and 213 interface methods with no omissions. The
CASE regression above was found afterward, so these tests are not a complete
correctness verdict. Trace refresh, upstream rounding files, repaired paired
evidence and the lead's maintained integrated Kani checkpoint are pending.
No performance acceptance measurement is claimed.

The CASE regression is repaired through the separate selected speculative NULL
probe; required DECIMAL precision still uses `constant_as` and still raises
errors even inside an unreachable branch, matching development's binding stage.
An independent follow-up established its contextual error distinction: a direct
SQL precision literal `'bad'` fails with Invalid Input Error in the decimal
binder's DefaultCastAs, while `CAST('bad' AS INTEGER)` fails with Conversion
Error during child evaluation. The scalar family reclassifies only the former
provenance; typed VARCHAR and parameters receive no literal privilege. Selected
source/output validation remains Internal, and Resource/Interrupted are not
reclassified. Tests cover the repaired branch, NULL short-circuiting, required
precision errors, malformed selected cast results, typed expressions/parameters
and replacement failure provenance. Focused precision tests, contracts 29,
operators 10 and all-target clippy pass. The repaired paired report follows on
the refreshed integration base; the initial observations above remain intact.

After merging combined base `b3175c1`, the [repaired paired
campaign](numeric-precision-reference-repaired.json) builds production source
in 1m30s and matches development **606/607 SQL** and release **270/607**. All
three native producer paths pass both pins, now including precision expressions
in native reads and cross-engine primary-key mutations. The only development
mismatch remains the explicitly retained bare UHUGEINT literal. All 378 other
new precision cases match development, including the regression repairs.
Release retains its separate older macro/overflow semantics; the strict
all-pin wrapper correctly exits 1. Combined numeric 35, contracts 29, operators
10, casts 12, temporal 25 and nested 31 pass; workspace/all-target check and
clippy pass. Coverage is 309 files, 2,846 functions and 214 interfaces, missing 0.

The initial unchanged upstream runs are retained in [rounding
results](upstream-numeric-precision-round.json) and [truncation
results](upstream-numeric-precision-trunc.json). ROUND reaches 2/17 records,
TRUNC 2/14 and TRUNC precision 4/14, then the Python oracle rejects exact numeric
equality such as expected `42.000000` versus actual `42`. Development's
`test/sqlite/result_helper.cpp:479-545` instead compares mismatched numeric cells
through their returned logical type. ROUND integers reaches 4/18 then the
unimplemented `test_all_types` table function. ROUND_EVEN reaches 18/20, then
requires the source's exact `Overflow in ROUND_EVEN of DECIMAL(38)` substring;
the family now reports the source integer/DECIMAL domain rather than generic
`numeric value`. Focused precision tests and all-target clippy pass after that
wording repair. The oracle discrepancy is separately coordinated with the lead;
no expected values or source SQL assertions have been changed.

Final interface instrumentation compatibility passes on `e101f1e` in 55.38
seconds with no error returns, panics or open spans. A focused trace then builds
in 3m00s and executes selected decimal precision, banker's rounding, nullable
column truncation and the repaired lazy CASE together. It returns two identical
rows `1.3, 1.2, 0, 1.0`: 63,331 completed operations, six intentional conversion
error returns from the unsuccessful speculative cast probe, zero panics and zero
open spans. The query succeeds and temporary telemetry is deleted. Traced
durations are not performance measurements. The lead still owns the maintained
integrated Kani checkpoint before substantial-stage completion.

The separately reviewed [exact numeric-oracle fallback](exact-numeric-oracle.md)
is now verified on combined `4b2a027`: all 38 Python tests pass; unchanged
ROUND_EVEN passes 20 expanded records, TRUNC 47 and TRUNC precision 51. ROUND
advances to five records before the deliberately unsupported approximate
floating oracle rule, with the actual values independently confirmed equal to
development SQL. ROUND integers remains blocked by the existing `test_all_types`
catalog gap after four records. Original and repaired reports remain separate;
no SQL expectations were weakened or rewritten.
