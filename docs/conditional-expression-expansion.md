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

The independent `selected_choose` contract adapter exercises four-argument CASE
selection, full-width result inference, prepared parameters, lazy conversion
failures, selected comparison casts and fatal errors, plus the sequence-like
occurrence pattern above under both evaluators and optimizers. Malformed/cyclic
graphs, invalid argument positions, depth/occurrence/node limits, cancellation,
effectful-own expansion metadata and ordinary replacements have explicit tests.

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
