# Selected typed constants

This provisional shared prerequisite lets scalar binders request a closed,
effect-free argument converted to explicit target metadata with
`ScalarBindArguments::constant_as`. It is an internal integration step toward
precision-dependent numeric functions; no numeric function uses it in this
commit, and it is not a substantial-stage completion claim.

The SQL frontend retains the selected cast in an ordinary bound expression and
evaluates it with the selected statement evaluator. The ordinary scalar path
and constant path share cast-mode selection: only Implicit requests gain SQL
literal privileges; Explicit and Assignment retain their exact mode. Typed API
parameters, casts and CASE expressions do not become SQL literals. The result
is validated against the target logical type. Other frontends explicitly reject
the capability rather than using an ambient or built-in cast fallback.

Two contract tests cover both evaluators and optimizers, SQL literals versus
typed parameters, selected Explicit/Assignment replacement casts, typed NULL,
closed arithmetic and CASE, missing arguments, relational dependencies,
volatile/external effects, malformed physical/range/undeclared-NULL cast output,
malformed evaluator output, Resource/Internal/Interrupted propagation, outer
TRY_CAST, and atomic failure of a table mutation. A metadata-only frontend test
also verifies that the default never invokes its untyped constant evaluator.

Focused tests pass. The wider pass includes contracts 29, casts 12, numeric 32,
settings 8, nested 28 and temporal 25, plus workspace/all-target check and clippy.
Coverage reports 300 files, 2,726 functions and 210 interface methods with no
missing attributes. The final focused tests include Interrupted propagation;
instrumentation compatibility then passes in 43.07 seconds with no error
returns, panics or open spans. Temporary telemetry is deleted. Integration-base
refresh and the lead's maintained Kani checkpoint remain separate checks; no
performance or proof result is claimed here.
