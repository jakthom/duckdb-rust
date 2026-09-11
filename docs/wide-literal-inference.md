# Full-width literal inference

This continuing scalar slice addresses the three wide-UHUGEINT CASE/list
metadata failures retained in [the numeric binding report](numeric-binding-reference.json).
Development `99063af2bd` governs correctness; release `d8cdaa33fd` remains a
separate observation. Checkpoint 9 is the integrated starting point.

## Selected interface prerequisite

The provisional `IntegerLiteral` hint owns either an exact signed i128 or an
exact unsigned u128 payload. The registry checks representation and bounds
against each source type before invoking an adapter. Full-aware type adapters
receive each hint with its operand, including when distinct families reverse
their proposal order. Same-family requests retain original order and dispatch
once. Conflicting targets, invalid metadata and adapter failures remain errors.

Existing signed Option<i128> methods remain compatibility wrappers. The new
defaulted hook delegates signed-only requests to the old selected signed hook.
If either hint is unsigned, the default calls that adapter's ordinary proposal
instead of passing a misleading partial pair or narrowing/wrapping the value.
No-hint requests retain ordinary dispatch. This prerequisite changes neither
Value/DataType footprints nor SQL function argument coercion and does not
evaluate expressions to manufacture provenance.

Tests exercise full u128/i128 extrema, reversed distinct families, same-family
order, conflicting/invalid proposals, fatal resource errors, invalid hint
representations/ranges and legacy signed adapters. An initial test delegated
key writing through a nonexistent BoundType method; it was corrected to use
the existing explicit PrimitiveTypes adapter delegation. Types 20 and contracts
32 tests pass; all-target clippy passes and coverage has no missing annotations.
SQL family integration, expanded reference evidence and maintained Kani are
subsequent checkpoints, not claims made by this prerequisite.

## Source-backed common-type behavior

`GetExpressionReturnType` in `bind_comparison_expression.cpp` marks non-NULL
integral constants, including UHUGEINT, as INTEGER_LITERAL. Equal integer
pseudo-types combine their underlying types; one literal can adopt a fitting
concrete integral type. Collection templates retain exact repeated literals
and skip later NULLs, unlike CASE's pairwise normalization.

`CombineNumericTypes` in `types.cpp` checks the signed operand before its final
HUGEINT/UHUGEINT branch. Thus ordinary UHUGEINT combined with TINYINT, SMALLINT,
INTEGER, BIGINT and HUGEINT selects SMALLINT, INTEGER, BIGINT, HUGEINT and
DOUBLE respectively. These selected types do not necessarily contain the full
input domain: a later retained cast may fail. This source behavior must not be
replaced by a silently lossless-widening assumption. Arithmetic overload ranking
is distinct: UHUGEINT plus typed INTEGER or BIGINT selects DOUBLE. Pinned direct
CLI probes confirm these differences. Implementation and regression evidence
for the selected family follows separately.
