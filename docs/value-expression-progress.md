# Value-and-expression integration

The [accepted milestone](../specs/value-expression-milestone.md) remains active.
The integration branch combines scalar, temporal and nested worker increments;
none of those families, or the larger milestone, is declared complete here.

## First integrated checkpoint, 2026-09-10

- Shared type factories retain selected child adapters; composite casts retain
  child casts. Recursive common-type inference preserves original operand order
  within one family, including left-first STRUCT member ordering. Distinct-family
  proposals must agree. Composite overload ranking checks the selected child
  registrations rather than shape alone. Replacement, missing-child, invalid
  capability, scalar/batch and cancellation contracts remain enforced.
- BLOB and UUID now have SQL/cast/function, comparison/key, relational,
  prepared-value, mutation and native checkpoint/WAL paths. The worker's
  [report](scalar-values.md) retains the failed BLOB-default encoding trial and
  its repair: scalar/default serialization uses escaped text while column and
  WAL vectors use raw bytes. Constant-NULL concatenation result metadata remains
  a shared binding obligation.
- Temporal SQL literals and retained comparisons/keys are integrated. The next
  worker increment adds arithmetic and native codecs. INTERVAL is deliberately
  rejected as a table index key, matching development, while grouping and joins
  continue to use its canonical equality. Index admissibility is a separate
  selected type capability, not inferred from comparison support.
- LIST/ARRAY/STRUCT/MAP constructors, accessors, recursive casts and nested NULLs
  work through the tested relational paths and private snapshot reopen. See the
  [nested report](nested-values.md). Native nested storage, fuller UNION/VARIANT
  behavior and their cross-family interactions remain in progress. Compact
  metadata/value footprints remain covered by the existing tests.
- Native development compatibility now recognizes the main-header 999 sentinel
  using the selected database-header storage version, qualified catalog names
  and default-expression source spans. Providing full catalog qualification
  fixes development opening Rust checkpoints and replaying Rust CREATE TABLE
  WAL records. Ownership metadata and segment byte extents are being integrated
  for development-produced files; no full native compatibility claim is made.

The first combined `cargo test --workspace` passed, with the two existing
external-CLI analytics tests ignored. Ordinary focused checks/tests/clippy,
instrumentation coverage and tracing compatibility were also run during
integration; retained telemetry is deleted. The numeric
[checkpoint campaign](value-expression-numeric-reference-checkpoint1.json)
retains 38/38 development SQL passes and the 18 known release disagreements.
Release native cases pass 3/3; development passes 2/3, with its own newly produced
file still stopped by the then-unhandled segment byte-size field. This report
is immutable evidence for that intermediate source identity, not a final result.

`python3 scripts/verify_kani.py` ran on the first combined implementation with
Kani 0.67.0: all five maintained harnesses passed. Proof times were 0.845 s for
unsigned keys, 0.571 s for dense offsets, 3.001 s for ROWS clipping, 3.584 s for
uniform frame bounds and 88.936 s for packed byte counts. Caller-location and
foreign-function warnings were not reached by those proofs; atomics were modeled
sequentially. This does not prove temporal/nested semantics, transactions,
concurrency or whole-engine correctness. Further substantial integrated changes
receive further checkpoints under the exploratory policy.

No new acceptance performance campaign has run. The 34 passing workloads at
`f5f0fae` are the preceding baseline, not measurements of this combined code.
Both references must be measured in a coordinated quiet period before claiming
that performance is preserved. No milestone changes have been pushed yet.

## Continuing integration obligations

Literal-sensitive and comparison/CASE/function coercion; typed NULL versus an
active NULL UNION member; constant-NULL result metadata; unnumbered prepared
parameters; explicit indexes; transactional named-type lookup/dependencies;
native nested persistence and mixed-family recovery/reopen; remaining numeric
and core scalar/temporal/nested semantics; combined upstream and performance
regression campaigns. These are implementation work within the assignment, not
reasons to stop after the first isolated passing path.
