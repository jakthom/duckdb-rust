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
independent C++-produced BIGNUM compatibility evidence is still to be added.

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

SQL exact arithmetic/SUM/window registration, remaining scalar functions,
independent paired compatibility campaigns, mixed-family VARIANT/UNION support,
broader upstream mappings, diagnostics and controlled faster-reference performance
are continuing work. The lead runs and investigates the maintained Kani suite
on integrated checkpoints before declaring the substantial stage complete;
this prerequisite is not that declaration and adds no performance acceptance
claim. BIGNUM does not replace the remaining BIT aggregate or core GEOMETRY work.
