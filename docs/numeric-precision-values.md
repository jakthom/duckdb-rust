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
