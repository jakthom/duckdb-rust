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

## CASE and collection integration

The SQL frontend now passes the exact unsigned hint to the selected adapter
without changing signed-only scalar overload metadata. Built-in signed and
exact-numeric families accept a single fitting full-width literal; two literal
hints combine the underlying types. The common-type fallback follows the
development signed-width ordering above. Explicit casts, prepared parameters,
CASE wrappers and computed expressions remain nonliteral inputs.

Focused tests cover both evaluators and optimizers, exact hint replacement
dispatch, CASE short-circuiting, required conversion failures, fatal selected
casts, MAP and nested child metadata, prepared parameters, joins, grouping,
windows, indexed lookup, atomic failed updates, rollback and native/private
checkpoint and WAL reopen. Types 22, numeric 43, contracts 32, nested 41, bit 8,
operators 10 and casts 12 pass. All-target check and clippy pass. Coverage lists
330 files, 3,084 functions and 217 interface methods with no missing annotations.
An initial test expected an outer TRY_CAST to catch a failed child CASE cast;
independent development execution also raises Conversion, so the test now
preserves that boundary instead of weakening required child evaluation.

This is an integrable internal step, not a completed verification checkpoint.
Paired reference evidence, tracing and maintained Kani follow on the combined
slice. The source-wide common correction exposed a directly affected existing
COALESCE policy gap: `coalesce(1::UHUGEINT,1::INTEGER)` now raises Rust Binder
`no Implicit cast from UHUGEINT to BIGINT`; development returns BIGINT value 1.
The former DOUBLE result was already incorrect, but its accepted query must
not be lost. A selected combination specialization is the next repair; global
implicit casts will not be widened. NULLIF also has an independently observed
existing result-type gap: development retains its first UHUGEINT input type.

The immutable [wide inference campaign](numeric-wide-inference-reference.json)
records 803/803 development SQL cases and all three native persistence paths
passing on unchanged source. All 705 prior case identities remain present and
pass, including the original three wide failures; 98 added cases exercise the
signed-width ladder, full-domain values, nested/MAP inference and error boundaries.
Release passes 462/803 SQL cases and all three persistence paths. The runner's
nonzero exit retains those release disagreements; it is not a development
correctness failure. This campaign predates the COALESCE follow-up and does not
claim that its separately retained regression is covered or fixed.

## Selected scalar combination prerequisite

COALESCE's C++ `ResolveCoalesceType` starts with argument zero and normalizes
each subsequent pair in source order. Direct development probes confirm that
`coalesce(1,NULL,1::UHUGEINT)` is BIGINT while
`coalesce(1,1::UHUGEINT,NULL)` is UHUGEINT. The frontend's new selected request
therefore shares the pairwise inference helper but supplies argument order,
not CASE's ELSE-first traversal or collection skip rules.

The owned proposal retains its common type and every selected combination cast
mode. Its requesting specialization disables only the later scalar literal-mode
rewrite, so available selected Implicit casts remain selected. The defaulted
hooks preserve other scalar adapters. Tests check source order, unsigned hints,
typed parameters, unevaluated failing/effectful children, malformed proposals,
missing frontend capability, invalid positions and cancellation. Separate cast
counter tests distinguish ordinary literal privilege from exact selected mode.
Initial tests incorrectly assumed signed-to-UHUGEINT was implicit and attempted
to replace an unregistered narrowing cast; they now reflect the existing selected
registry and explicitly register that test-only capability. A test Debug derive
also required excluding the non-Debug interruption handle. No production casts
or interruption contracts were altered to satisfy those test setup errors.

The preceding core inference trace compatibility check passed with zero error
returns, panics or open spans; its temporary telemetry was deleted. Combined
maintained Kani remains pending the integration checkpoint, not claimed by these
prerequisite commits.
