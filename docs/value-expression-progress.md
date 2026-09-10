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

Literal-sensitive and comparison/CASE/function coercion; fuller UNION/VARIANT
NULL behavior; constant-NULL result metadata; named prepared parameters;
explicit indexes; transactional named-type lookup/dependencies;
native nested persistence and mixed-family recovery/reopen; remaining numeric
and core scalar/temporal/nested semantics; combined upstream and performance
regression campaigns. These are implementation work within the assignment, not
reasons to stop after the first isolated passing path.

## Second integrated checkpoint, 2026-09-10

At `7c2841e`, temporal arithmetic, native temporal checkpoint/WAL/default codecs,
ordered ENUM metadata, UNION member constructors/accessors and scalar casts,
LIST aggregates/windows, anonymous prepared parameters, and CTAS NULL-leaf
normalization are integrated. The selected cast NULL-handling contract permits
a typed NULL to become an active NULL UNION member, retaining child adapters in
scalar and batch execution. Invalid non-NULL-to-NULL results remain rejected.
Anonymous parameters receive lexical positions before binding visits FROM,
aliases or reused expressions; numbered parameters advance the position and
quoted/comment text does not consume positions. Mixed-schema tests cover typed
inserts, updates and rollback. CTAS resolves untyped NULL children recursively
before assignment, publication and private-format reopen.

The immutable [numeric campaign](value-expression-numeric-reference-checkpoint2.json)
records unchanged sources during the run: development 38/38 SQL and 3/3 native
cases, release 20/38 SQL and 3/3 native cases. The same 18 release disagreements
remain governed by development. Newly consumed development ownership metadata
and bounded segment payload sizes repair reading development-produced files.
Constant segments may retain a prior nonzero byte size despite having no block;
their decoder uses validated statistics. No broader file compatibility is
inferred from these three producer paths.

The combined workspace suite passed with the same two ignored external-CLI
tests. After parameter/storage-boundary changes, all 19 contract tests passed;
ordinary check/clippy and instrumentation coverage/trace checks passed. Coverage
reported 238 files, 2,070 functions and 201 interface methods with no missing
instrumentation. Temporary telemetry was deleted.

`python3 scripts/verify_kani.py` ran on this checkpoint with Kani 0.67.0: 6/6
maintained harnesses passed, zero failures. Times were TIMETZ packing 20.983 s,
unsigned keys 0.765 s, dense offsets 0.494 s, ROWS clipping 2.142 s, uniform bounds
2.333 s and packed byte counts 91.410 s. Caller-location (1) and foreign-function
(4) warnings were unreachable in these proofs; atomics remained sequential.
The new proof bounds valid TIMETZ packing and equality identity. It does not
prove general timezone/calendar semantics, parser behavior, recursive values,
transactional durability or concurrency.

Native nested checkpoint work and grammar fixes are subsequent increments.
Performance and the full upstream regression refresh have not been rerun on
this checkpoint; the preceding passing baseline remains historical evidence.
