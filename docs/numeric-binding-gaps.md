# Numeric binding follow-up

This bounded follow-up addresses the three development failures retained in
the ABS numeric campaign: two CASE literal
result types and the bare maximum UHUGEINT literal. Development `99063af2bd`
is the correctness authority; original failed evidence is retained.

## CASE prerequisite

The development CASE binder binds children in source order, then infers the
result starting with ELSE followed by each THEN. The rewrite previously combined
plain declared types in the opposite order, losing integer/string pseudo-type
information. It now uses selected literal-aware proposals in that source order.

The ordered helper is shared with collection constructors, but the policies
remain distinct. CASE normalizes every pair, including equal literals and later
NULLs. Collection templates skip those particular pairs. Source inspection of
`bind_case_expression.cpp`, `combine_types_rules.cpp` and `types.cpp` plus direct
CLI probes establishes the distinction: a repeated literal or intervening NULL
before a later TINYINT branch makes CASE INTEGER, while the corresponding list
template can remain TINYINT[]. No result child is evaluated for inference, and
pruning retains CASE provenance for enclosing overload resolution.

On combined `29a5b22`, contracts 32, types 18, nested 41 and numeric 38 tests pass,
including selected proposal replacement, both evaluators/optimizers, typed
parameters, lazy failures, atomic mutation and the pre-existing collection
contract suite. Workspace/all-target check and clippy pass. Paired report and
integrated maintained Kani evidence follow after the number-parsing prerequisite;
this is not a separate substantial-stage completion or performance claim.

## Bare integer prerequisite

Numeric token construction now tries signed i128, unsigned u128, then the
existing exact BIGNUM family parser. `Value::data_type` retains the established
signed INTEGER/BIGINT/HUGEINT widths, and integers beyond u128 or below i128
remain BIGNUM. Decimal and floating token rules are not expanded. The helper
receives the actual statement QueryContext, including its existing decimal
parse path; BIGNUM allocation and limb loops check that context rather than
constructing an uninterruptible background query. Source authority is
`PEGTransformerFactory::ConvertNumberToValue` in `transform_common.cpp`.

Focused tests cover each signed-width transition, i128/u128 boundaries, a
1,000-digit integer, exact casts/arithmetic, same-domain CASE/nested values,
prepared parameters, indexed joins, atomic duplicate-key mutation, rollback,
private/native checkpoints, native WAL and reopen. A helper-level test verifies
the actual supplied cancellation context for integer, decimal, floating,
UHUGEINT and BIGNUM tokens. An initial test incorrectly expected a Conversion
error when assigning BIGNUM(2^128) to UHUGEINT; independent development instead
returns zero under the previously recorded narrowing semantics. The atomic
mutation test now exercises a genuine duplicate-key failure without changing
those existing conversion semantics.

Unsigned-literal provenance remains a separate observed limitation:
`GetExpressionReturnType` classifies all non-NULL integral constants, including
UHUGEINT, as INTEGER_LITERAL. Existing signed Option<i128> hints cannot express
that whole domain. Further direct probes show development combining a maximum
UHUGEINT with INTEGER into BIGINT in CASE/list contexts, including explicitly
cast inputs; the rewrite's existing wide signed/unsigned proposal differs.
The new campaign retains those failures explicitly. This prerequisite does not
widen the public hint contract or pretend its signed hint covers UHUGEINT.

After the numeric-token repair, numeric 40, contracts 32, types 18, nested 41,
operators 10 and casts 12 tests pass. The direct statement-cancellation helper
test passes separately. Workspace/all-target check and clippy pass; coverage
reports 324 files, 3,033 functions and 216 interface methods, with no missing
annotations. The interface-count change is from the integrated base, not a new
public literal API. Trace compatibility passes in 1m00s with zero error returns,
panics or open spans; temporary telemetry is deleted. Integrated maintained
Kani and controlled regression/performance evidence remain lead-owned.

## Refreshed evidence

The expanded paired report builds unchanged
production source in 1m43s. All 672 prior numeric cases now match development,
including the three requested repairs. Thirty additional direct literal and
ordered CASE cases also match, giving development 702/705. The three explicitly
retained wider UHUGEINT CASE/list combinations above account for every remaining
development failure. Release matches 361/705 and retains its distinct older
rounding and oversized-integer behavior; it can parse beyond-machine integers
as DOUBLE where development preserves BIGNUM. Native C++ producer, Rust
checkpoint and Rust WAL paths pass 3/3 against each pin. Comparing prior passing
SQL identities on both pins finds no lost passing cases. The strict all-pin
wrapper exits 1; no failures or reference divergences were hidden.

The focused upstream CASE run passes
`case_varchar.test` 3/3 records. `case_condition.test` remains at 1/2 because
numeric predicates are not implicitly BOOLEAN; `case_short_circuit.test`
remains at 2/3 on unsupported SUBSTR syntax. The integer-literal upstream
file remains at its first assertion:
the Python oracle compares expected `true` with returned `1`. These exact
statuses, prefixes and reasons also appear in the earlier controlled
`value-expression-upstream-checkpoint5-d.json`; they are not new regressions.
The numeric exact fallback intentionally does not reinterpret BOOLEAN cells.
No expected SQL values, assertion types or diagnostics were changed.
