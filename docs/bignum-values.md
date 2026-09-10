# BIGNUM value-and-expression work in progress

Correctness follows pinned development `99063af2bd`; release `d8cdaa33fd` remains
the second reference. This is an internal prerequisite in the sustained scalar
assignment, not a declaration of BIGNUM or numeric parity.

`DataType::Bignum` and `Value::Bignum(Arc<BignumValue>)` use a sign and normalized
little-endian u32 magnitude limbs. Native complemented bytes/header are a codec
boundary, not an opaque logical representation. Value/type footprint checks
remain at most 32/16 bytes. The representation and algorithms are provisional.

The logical module implements decimal parsing/formatting, exact arbitrary-width
addition/negation/comparison, integral and floating conversion, and checked native
encoding. Registered type/cast adapters carry values through SQL casts, VARINT
alias declarations, comparison/equality keys, joins, MIN/MAX, prepared parameters,
primary keys, defaults, failed mutations, rollback and native WAL/checkpoint
reopen. String-column/codec dispatch includes BIGNUM logical type ID 39 and checks
NULL placeholders through the outer validity contract. Non-NULL native constant
string compression remains explicitly unsupported, as for the other string-
physical families. This prerequisite's persistence tests use Rust-produced files;
the follow-up independent campaign below adds C++-produced interchange evidence.

Independent development SQL/source inspection exposed important distinctions:

- VARCHAR `'-0'` canonicalizes to `0`; DOUBLE `-0.5` truncates to a distinct `-0`.
  Plain BIGNUM comparison/index keys distinguish negative zero from positive zero.
  Both values can coexist in a primary key. Negation of negative zero yields
  positive zero; adding two negative zeros retains negative zero.
- VARCHAR fractional conversion rounds away from zero, unlike floating BIGNUM
  conversion, which truncates. The reference parser's bounded accumulator also
  rounds `'0.400000000000000000001'` to `1`, but its all-zero-tail counterpart to
  `0`. These are retained witnesses, not inferred mathematically ideal behavior.
- Integral output first accumulates into a wrapping 128-bit temporary. Thus
  `2^128` BIGNUM becomes UTINYINT `0`; negative `-1` becomes UTINYINT `255`.
  The custom C++ HUGEINT/UHUGEINT `std::numeric_limits<T>::max()` used in this
  particular template is zero: positive BIGNUM `1` overflows both destinations,
  while negative `-1` becomes HUGEINT `-1` or UHUGEINT maximum. These quirks are
  confined to the selected cast, not exact arithmetic or comparison.
- BIGNUM-to-integer failures are Out of Range under CAST and Internal under
  TRY_CAST in development because that execution path throws from an infallible
  cast. Invalid VARCHAR/floating inputs and unsupported target conversions retain
  their own recovery contracts. Full diagnostic-string parity is not claimed.
- VARIANT retains BIGNUM payload/type and permits unbounded values, but normalizes
  negative zero for numeric comparison. This differs from plain BIGNUM ordering;
  nested-family integration owns the dynamic comparison/key behavior.

Primary implementations inspected are `duckdb/src/common/types/bignum.cpp`,
`duckdb/src/common/bignum.cpp`, `duckdb/src/function/cast/bignum_casts.cpp`,
`duckdb/src/include/duckdb/common/types/bignum.hpp` and the pinned BIGNUM SQL
directory. The main debug-only `Verify` assertion rejects negative zero even
though this pinned development SQL constructs and stores it; that assertion
cannot substitute for the observable correctness authority.

Two component tests cover independent machine-width arithmetic oracles, 10,000-
digit carries/subtraction/formatting, native headers, malformed inputs, negative
zero, cancellation, scalar/flat/dictionary/constant casts, both SQL evaluators,
indexed parameters, joins, defaults, failed updates, rollback, WAL, overflow
storage, checkpoint and read-only reopen. Normal workspace/all-target check and
clippy pass; regression suites pass numeric (27), BIT (7), casts (11), ENUM (3),
compression (16) and nested (17). Coverage reports 278 files, 2,452 functions,
207 interface methods and no missing instrumentation. Exhaustive tracing
compilation passes and removes its temporary telemetry.

The next integrable increment registers exact symbolic addition, subtraction,
negation and SUM, including DISTINCT, empty inputs and window frames. The
cumulative window shortcut previously instantiated a fixed-width aggregate
state directly; the BIGNUM adapter now declines that shortcut and uses its
selected exact limb state. A sole negative-zero input sums to positive zero,
matching the independent development witness. BIGNUM multiplication/division,
unary plus and existing numeric scalar functions retain selected DOUBLE
overloads; mixed FLOAT addition remains BIGNUM while DOUBLE/DECIMAL addition
is DOUBLE. Binary functions expose the native header/complement bytes, as in
development, not a mathematical base conversion of the magnitude.

Three component tests now cover these SQL/operator/group/window paths,
scalar/batched evaluation, both optimizers, typed prepared arithmetic and
rollback of exact updates. BIGNUM (3), numeric (27), operators (10), grouping
(9), BIT (7) and binary scalar (4) suites pass, as do ordinary workspace check
and clippy. The focused cumulative-window exhaustive trace agrees with
development on negative zero followed by 2^128, with 59,909 operations and no
errors, panics or open spans; temporary telemetry was deleted. An earlier form
without the ORDER key in projection encountered a handled binder resolution
probe while still producing the correct output; the narrowed trace avoids that
unrelated probe. This is correctness investigation, not timing evidence.

## Independent paired checkpoint

`CARGO_BUILD_JOBS=2 python3 scripts/bignum_reference.py --report
docs/bignum-reference-initial.json` built uninstrumented production binaries in
1 minute 47 seconds. The retained report verifies both pinned identities,
binary/source hashes and unchanged source, and records complete raw typed SQL
outcomes. Development passes 45/45 SQL cases; release passes 39/45. The six
release disagreements are fractional VARCHAR/ENUM conversion (three cases) and
TRY_CAST overflow categories (three cases). Development remains authoritative.
The script exits 1 because its all-pins aggregate is strict; the independent
`development_passed` result is true. No assertion or diagnostic body is rewritten
to conceal a disagreement; category-label casing alone is normalized for the
declared error-category comparison.

Native interchange passes 3/3 producer paths on both pins: C++ checkpoint,
Rust checkpoint and Rust WAL. Each path checks independent typed rows and
indexed lookups before and after Rust mutation/rollback and C++ mutation/reopen.
Inputs include distinct negative-zero/positive-zero primary keys, fractional
defaults, a beyond-128-bit key, NULL and a 10,000-digit overflow-stored value.
Producer-local values are preserved even when the two pins' default-expression
conversion differs. This is an exact row/interchange check, not a claim that
every native codec, format version or BIGNUM workload is covered.

Remaining scalar functions, broader independent compatibility campaigns,
mixed-family VARIANT/UNION support,
broader upstream mappings, diagnostics and controlled faster-reference performance
are continuing work. The lead runs and investigates the maintained Kani suite
on integrated checkpoints before declaring the substantial stage complete;
this prerequisite is not that declaration and adds no performance acceptance
claim. BIGNUM does not replace the remaining BIT aggregate or core GEOMETRY work.
