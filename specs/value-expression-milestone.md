# Value-and-expression milestone

Accepted user assignment, 2026-09-10. This is one sustained implementation
milestone, not a sequence of isolated type declarations. The outcome is realistic
typed data behaving consistently throughout the database. Small commits and
integration checkpoints are internal steps, not requests for renewed authority
to continue the assignment.

## Scope and acceptance

- Finish numeric semantics, coercion, overloads and functions; investigate and
  repair recorded correctness, native-file compatibility and performance gaps.
  Preserve the corrected `f5f0fae` baseline: 34 passing measured workloads and
  no lost passes in the 5,638-file upstream refresh. Its passing subsets do not
  establish full numeric or database parity.
- Add temporal types, timestamp units/timezones, intervals and remaining DATE
  behavior; nested LIST, ARRAY, STRUCT, MAP, UNION and VARIANT; and remaining
  core scalar families, including binary, UUID and enum values. Inventory the
  pinned development source rather than treating this list as exhaustive.
- Carry values and child metadata through literals, constructors/accessors,
  casts, overload resolution, functions, scalar/vector execution, comparisons,
  equality keys, joins, aggregates, sorting and windows. Follow reference-correct
  errors where an operation does not apply to a type.
- Integrate prepared parameters, indexes, table mutations, constraints applicable
  to those values, persistence, rollback and reopen. A private-format round trip
  does not establish native DuckDB compatibility. Retained type/cast bindings
  must preserve selected child semantics across registry replacement.
- Validate combined schemas and expressions: decimals inside structs, temporal
  values inside lists, nested casts, child/container NULL distinctions, mixed
  indexed mutations, failed statements, rollback, checkpoint/WAL recovery and
  reopen. Preserve values, metadata and observable errors, not just row counts.

The [rewrite principles](rewrite-principles.md), [parity rules](testing/parity.md)
and [exploratory Kani policy](testing/kani.md) continue to govern this milestone.
Development wins correctness disagreements. Every comparable performance case
uses the faster pinned release/development reference; failed measurements and
unimplemented behavior remain explicit. Expand evidence beyond the existing
selected workloads. Neither declarations, adapter existence, isolated tests nor
bounded proofs satisfy the end-to-end outcome.

Representations, algorithms and internal interfaces remain provisional. Use
early working paths to expose problems and revise the shared design. Do not
force opaque encodings, a particular vector layout or a proof-friendly design
at the expense of required semantics or replaceability.

## Parallel ownership and integration

| Owner | Implementation responsibility |
| --- | --- |
| Integration lead | Shared type/value definitions, registry/binding/coercion infrastructure, generic vector behavior, prepared-parameter integration, native persistence machinery and mixed-family behavior |
| Scalar owner | Numeric completion and other core scalar families, including relevant functions, execution, codecs, reference observations and regression tests |
| Temporal owner | Temporal parsing/casts/arithmetic/functions, explicit timezone configuration, comparisons/keys, execution, codecs and regression tests |
| Nested owner | Child metadata, constructors/accessors, recursive casts/comparisons/keys, nested execution, codecs and regression tests |

All workers start from the same corrected integration commit in separate
worktrees. Family owners deliver usable increments and continue within their
assignments. Shared-file prerequisites are communicated and isolated; the lead
integrates their definitions and dispatch changes continuously. Workers regularly
incorporate the integrated branch through coordinated commits. Ownership may be
rebalanced as actual remaining work becomes clear.

Build artifacts, fixtures and reports are worktree-local. Only coordinated
quiet periods may be used for acceptance benchmarks; separate worktrees do not
isolate CPU, memory or I/O load. Do not rebuild shared C++ references or overwrite
shared workers during another campaign. Ordinary correctness work may proceed
concurrently. Run and report maintained Kani harnesses at substantial integrated
checkpoints; investigate findings and record limitations without treating proof
success as an exploratory completion gate.

## Subsequent large assignments

1. SQL and catalog completeness: remaining queries, views, macros, sequences,
   constraints, dependencies and schema changes.
2. Storage and transactions: native compatibility, indexes, recovery, durability
   and concurrent visibility.
3. Execution at scale: memory accounting, spill, parallelism, scheduling and
   planning.
4. External data and ecosystem: formats, filesystems, embedding APIs, clients
   and extensions.

These are primary work assignments, not barriers against required dependencies.
Catalog identity, timezone settings or storage changes needed for typed values
belong in this milestone rather than being postponed because another assignment
will later expand that subsystem.
