# Numeric binding follow-up

This bounded follow-up addresses the three development failures retained in
[the ABS numeric campaign](numeric-absolute-reference.json): two CASE literal
result types and the bare maximum UHUGEINT literal. Development `99063af2bd`+is the correctness authority; original failed evidence is retained.

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
