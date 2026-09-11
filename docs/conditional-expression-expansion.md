# Conditional expression expansion

This continuing scalar slice starts after the repaired wide inference and
COALESCE [evidence](wide-literal-inference.md). Development `99063af2bd` remains
correctness authority. The shared interface and template representation are
provisional and do not implement a general macro catalog.

Development's default NULLIF macro is `CASE WHEN a=b THEN NULL ELSE a END`
(`src/catalog/default/default_functions.cpp:128`). It retains the first
argument's result type and can evaluate that expression again. Direct C++ probes
using a sequence starting at 1 return 2/current-value 2 for `nullif(nextval,99)`,
then NULL/current-value 3 for `nullif(nextval,3)`. A cast-to-common-type eager
scalar, followed by casting its result back, is not equivalent.

The selected expansion prerequisite uses owned child-before-parent nodes for
argument occurrences, NULL, equality and CASE. Complete bounded validation
precedes lowering and CASE pruning. SQL reuses its retained selected comparison
and CASE combination casts; an argument node is cloned for each occurrence,
not evaluated or cached during expansion. Ordinary selected scalar replacements
retain their default no-expansion path. Unsupported frontends must reject an
adapter that requires expansion rather than silently invoking different behavior.
This validation boundary concerns the returned template, not construction by the
selected callback. That callback retains ordinary selected binding requests;
eligible constants can be requested under their existing closed/effect-free,
selected-service and output-validation checks. Review did not find a bypass of
those checks and did not restrict the shared view to enforce a stronger guarantee.

The independent `selected_choose` contract adapter exercises four-argument CASE
selection, full-width result inference, prepared parameters, lazy conversion
failures, selected comparison casts and fatal errors, plus the sequence-like
occurrence pattern above under both evaluators and optimizers. Malformed/cyclic
graphs, invalid argument positions, depth/occurrence/node limits, cancellation,
effectful-own expansion metadata and ordinary replacements have explicit tests.

Review found that template-only occurrence counts treated Argument as one node,
although lowering clones its entire already-bound scalar subtree. Nested selected
calls could therefore amplify earlier expansions. The follow-up binder preflight
measures those actual argument subtree weights before any occurrence is cloned,
then combines weighted counts/depths across the complete template. It bounds the
expanded bound tree to 4,096 nodes/depth 128, conservatively reserving one cast
per child edge. A 16-deep duplicating custom call now fails Resource before large
allocation or any volatile child execution; a separate test combines an 81-deep
argument with a valid 32-node template. Both tests exercise limits not established
by the original template-only validation. Payload byte sizes/general query memory
and the referenced immutable relational plans are outside this scalar-tree budget.

NULLIF registration and a shared comparison execution dependency follow this
internal prerequisite. Development comparison expressions are now bound scalar
functions (`src/function/scalar/comparison/comparison.cpp:147`), so default
constant-NULL handling stops before later children. Even with optimizers disabled,
`NULL::INTEGER=CAST('bad' AS INTEGER)` and the equivalent NULLIF return NULL;
a sequence in the second operand is not advanced. A VALUES column containing
only NULLs still evaluates the bad cast and raises Conversion, while a projected
constant NULL over range rows returns NULL. This is physical provenance, not
observed equality of values, and must not become a NULLIF-specific shortcut.

The temporal owner's integrated execution-provenance interface supplies the
needed Constant versus Unknown distinction. The comparison repair is owned by
this scalar slice but stays separate from the expansion prerequisite. Maintained
Kani will run at a substantial integrated checkpoint; no unexecuted proof or
completed NULLIF/whole-function parity is claimed here.

## NULLIF selected registration

The scalar catalog now registers NULLIF as a required selected expansion using
the five-node CASE/equality template above. Its ordinary bind, return-type and
evaluation methods explicitly report Unsupported. The old eager common-type
coercion and ambient comparison lookup have been removed; ordinary frontend
comparison and CASE services retain both casts and the original ELSE expression.

Connected tests cover unsigned/decimal/VARCHAR and nested first-result metadata,
equal/non-equal/NULL values, full-width comparison conversion failure, typed
prepared parameters, volatile occurrence counts, grouping, joins, windows, indexed
lookup, atomic failed UPDATE, rollback and native WAL/checkpoint reopen. Both
evaluator and optimizer compositions exercise the scalar cases. Removing the old
NULLIF branch exposed an unused type parameter and a single-arm concat match;
routine compiler/clippy cleanup left concat behavior unchanged.

The physical constant-NULL comparison rule remains explicitly pending at this
internal registration step. The existing successful NULLIF tests do not claim
that skipped-error/effect boundary is already implemented. Its shared follow-up
will use actual execution provenance and cover scalar, batch and predicate paths,
including Unknown/flat NULL inputs and required fatal validation.

## Shared comparison execution follow-up

The six ordinary comparison nodes now consume actual Constant/Unknown provenance
at execution. A left constant NULL returns a typed constant NULL before the right
child. A right constant NULL retains the already executed left child's effects,
errors and selected validation. This applies to scalar, batch and predicate paths;
dictionary root caching retains provenance and validates produced values before
normalizing the result. A value-only evaluator replacement still supplies Unknown.

The connected comparison contract matrix spans both evaluators and optimizers,
batch sizes 1/2/5, all six comparison operators, projected constants versus flat
VALUES NULLs, prepared NULL/non-NULL parameters, scalar subqueries, skipped and
required volatile calls, selected total casts that raise Resource, dictionary
inputs, malformed retained metadata and selected result/earlier-value validation
failures. The initial custom NULL-payload rejection test was invalid: BoundType
universally accepts NULL, so the corrected test exercises actual metadata and
nonnull logical-value validation boundaries instead. No validator behavior was
weakened to satisfy that test.

A direct development CLI sequence probe independently returns these exact CSV
lines for nonmatching, matching and constant-NULL first arguments, in order:
`2,2`, `NULL,3`, `NULL,3`. The second column is `currval`; therefore nonmatching
NULLIF evaluates its first argument twice, matching evaluates it once, and a
constant NULL first argument does not advance the second argument's sequence.
The sequence catalog itself remains outside this scalar implementation; Rust
contract adapters provide the equivalent counted volatile occurrence witness.

Routine workspace checks and the paired report are recorded in the follow-up
evidence commit. Kani remains assigned to the substantial integrated checkpoint;
neither the comparison/expansion invariants nor general NULLIF/macro completeness
are claimed formally proved by this internal delivery.

## First retained paired checkpoint

On unchanged engine source `38952bd`, `numeric-nullif-reference.json` records
894/899 development SQL cases and all 3 native producer/checkpoint/WAL paths
against each pin. The existing 830 development case identities all still pass.
Release passes 538/899; development remains correctness authority. The first
report is immutable, including five new failures, not evidence of full parity.

Two failures concern GROUP/window output types: the VALUES input
`(1::UHUGEINT),(2),(NULL)` already becomes BIGINT in Rust but UHUGEINT in
development before NULLIF is applied. Direct independent CLI probes confirm
that replacing literal `2` with `2::INTEGER` produces BIGINT in development.
The other three failures concern `nullif()`, `nullif(1)` and `nullif(1,2,3)`:
both references report Parser while Rust reports Binder. The initial campaign's
declared Binder expectation was itself incorrect; raw results remain retained.
Follow-ups repair VALUES literal combination and reserved NULLIF syntax rather
than special-case NULLIF result coercion or weaken result/error comparison.

Routine checks passed: full workspace before the final earlier-value validation
tightening; then contracts 51, numeric 48, execution 51, nested 42, temporal 31,
operators 10 and types 22 on final source. All-target clippy, coverage
(348 files / 3,322 functions / 227 interface methods, missing 0), trace
compatibility and 38 Python harness tests pass. A focused final-source SQL trace
returns `NULL,1,UHUGEINT` with 59,338 completed operations, zero error returns,
zero panics and zero open spans. Temporary telemetry was deleted. No acceptance
timings were run; integrated Kani remains pending for this continuing slice.

## VALUES inference dependency repair

The VALUES binder now combines columns through the selected full-literal path,
starting from SQL NULL exactly as development's ExpressionListRef binder does.
Every pair normalizes the current type; it does not reuse collection skip rules.
Direct reference witnesses cover forward/reverse unsigned/literal order, explicit
INTEGER, first max-UHUGEINT literal and a later fitting literal. Typed parameters
remain nonliteral, selected replacement proposals stay authoritative, and INSERT
destination Assignment binding does not perform unrelated common-type inference.

Connected tests retain inferred UHUGEINT through NULLIF grouping/windows, CTAS,
constraint-owned indexes, prepared insertion of max UHUGEINT, atomic failed
mutation, rollback and native WAL/checkpoint reopen. Standalone CREATE INDEX is
still unsupported; the test uses real UNIQUE/PRIMARY KEY-owned indexes rather
than implying that additional SQL catalog feature is implemented.

## Reserved NULLIF parser boundary

The reserved unquoted/unqualified spelling now parses exactly two expressions
and emits an ordinary function AST. Extra/missing arguments, trailing commas,
named/ordered/distinct arguments and FILTER/OVER syntax are rejected by parsing.
Quoted and qualified names remain on the ordinary function grammar, matching the
source grammar distinction; this is not general qualified catalog resolution.
An initial token-only prototype incorrectly rejected `main.nullif()` with
`expected statement delimiter, found (`. Differential parsing against stock
sqlparser identified this as a new-change regression, not an existing gap:
compound-name parsing re-enters the prefix hook after a dot. The repaired hook
checks the previous non-whitespace token without changing parser state and
declines reserved syntax after `.`. Qualified NULLIF with zero/three arguments,
intervening comments, qualified CEIL/FLOOR/DATE and field identifiers are retained
as parser regression checks. Generic qualified/quoted catalog lookup is separate.
A selected ordinary `nullif` replacement returning 42 still returns 42 for
`nullif(1,1)`, proving that syntax did not force the builtin CASE expansion.

The original three expected Binder categories in the first report are corrected
to independently observed Parser categories only in the follow-up driver/report.
The first raw report and all case identities remain intact. Broader macro/catalog
support and remaining generic argument diagnostics (including star arguments)
are not covered by this bounded syntax correction.

## Retained VALUES and parser checkpoint

On unchanged engine source `c3358c0`, `numeric-nullif-values-reference.json`
records 935/936 development SQL cases, 579/936 release cases and all three
native producer/checkpoint/WAL paths against each pin. All five failures from
the first NULLIF report are repaired; both raw reports remain immutable.
The expanded corpus adds ordered VALUES types and reserved grammar boundaries.
`upstream-nullif-values.json` and its record stream retain the complete upstream
`test/sql/function/generic/test_null_if.test` pass (11/11 records); this selected
file run is not a full upstream-suite pass.

The sole new mismatch is `SELECT a FROM(VALUES('1'),(2))t(a)`: both references
reject it with `Not implemented Error`, while Rust returns Binder. Development's
`LogicalType::MaxLogicalType` throws `NotImplementedException` for this recognized
incompatible combination. This is an existing error-category gap, not a newly
lost passing case. Rust's `Unsupported` means a missing rewrite capability and
must remain distinguishable from such supported rejections. A separate error
variant and narrowly scoped VALUES consumer are the next internal steps; error
comparators have not been normalized or weakened to hide this mismatch.

Full workspace tests and trace compatibility passed on that unchanged source.
All-target clippy and coverage (349 files / 3,331 functions / 227 interface
methods, missing 0) passed before the frozen campaign. A focused trace of NULLIF
over mixed typed/literal VALUES returns `NULL,2,NULL`, all typed UHUGEINT, with
66,573 completed operations, zero error returns, zero panics and zero open spans.
Temporary telemetry was deleted and `cargo dev clean` completed. No worker
acceptance benchmarks ran. The parent owns the substantial integrated Kani
checkpoint; these internal checks do not prove expansion or execution parity.
