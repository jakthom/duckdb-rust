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

Broader literal-sensitive comparison/CASE/function coercion; fuller UNION/VARIANT
NULL behavior; named prepared parameters;
explicit indexes; transactional named-type lookup/dependencies;
native nested WAL/defaults and broader mixed-family recovery/reopen; remaining numeric
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

## Third integrated checkpoint, 2026-09-10

Engine `2536548` combines development temporal precision/functions, BLOB/UUID,
ordered anonymous ENUM, nested checkpoint streams and MAP access with shared
literal-sensitive binding. Mixed schemas carry DECIMAL/temporal children through
MAP access, prepared parameters, joins, DISTINCT, window partitions, indexed row
mutations, rollback and native reopen. The grammar fork retains DuckDB dialect
behavior for recursive MAP/TUPLE declarations and empty ENUM diagnostics; TUPLE
declaration parsing alone is not runtime support. Vendored source now participates
in reference/performance source identities.

ROARING (13), DICT_FSST (15) and EMPTY_VALIDITY (14) repair the independently
produced development child streams. All six original nested fixtures now use
exact positive acceptance paths, with no codec skips/negative placeholders;
additional ROARING and all three DICT_FSST modes cover validity, empty strings,
partial tails, mutations, rollback and reopen. A selected decoder capability
distinguishes preserving an inline NULL mask from asserting validity. Ordinary
or malformed validity adapters cannot return the preserve marker. Native
parameterless built-in UNBOUND type expressions in defaults resolve through a
bounded decoder; unresolved names, parameterized expressions and aliases remain
explicit gaps rather than becoming SQL NULL.

Shared comparison coercion distinguishes actual string literals from VARCHAR
columns and retains selected casts across comparisons, IN and subqueries.
Selected scalar binding exposes that distinction to MAP and temporal functions.
Constant-NULL concatenation now has development's `"NULL"` result metadata,
without changing ordinary arithmetic types or discarding declared effects.
Prepared rebinding and CTAS normalization preserve the distinction. Independent
cast input-NULL and output-nullability capabilities support the next VARIANT
increment; both are retained and scalar/batch boundaries still reject malformed
results. No VARIANT runtime completion is implied by that shared prerequisite.

Selected reference evidence (each report retains its own exact source identity):

| Campaign | Development SQL | Release SQL | Native paths per pin |
| --- | ---: | ---: | ---: |
| [Numeric](value-expression-numeric-reference-checkpoint3.json) | 38/38 | 20/38 | 3/3 |
| [BLOB/UUID](value-expression-binary-reference-checkpoint3.json) | 25/25 | 23/25 | 3/3 |
| [ENUM](enum-reference-integrated.json) | 28/29 | 28/29 | 6/6 |
| [Temporal functions](temporal-functions-reference-integrated.json) | 437/437 | 412/437 | 3/3 |

The numeric/BLOB refreshes use engine `28063ef`, preceding the catalog-order
repair below. Development remains authoritative for the retained release
differences. ENUM boundary row-zero broadcasting remains open. The
[binding corpus](value-expression-binding-reference-checkpoint3.json) preserves
all 92 records against each reference. These are selected workloads, not full
type/function/catalog/native parity.

### Regressions investigated before pushing

The first [full upstream run](value-expression-upstream-checkpoint3-a.json)
increased passing files but lost two previously passing negative-test files and
three passing records in another file. Shared scalar-call refactoring had moved
catalog lookup after argument binding: unsupported ANY/star/tuple arguments
masked a missing-function error. `2536548` restores catalog-first lookup and
passes the same selected function into binding. Regression tests retain this
ordering and still check known functions' arguments.

The [repaired run](value-expression-upstream-checkpoint3-b.json) accounts for all
5,638 original file identities: 391 passed, 2,072 failed, 3,162 unsupported,
10 timed out and three incomplete. Its 18,606 passing records include prefixes
of failing files. Compared with `upstream-parity-batches-final.json` (313 files,
17,018 records), **no passing file or passing record prefix is lost**. Both failed
and repaired reports/journals remain retained. Unchanged assertions, three-second
file limits and two workers were used. Broader SQL and verification gaps remain.

The quiet performance campaign used 21 paired samples after three warmups,
with all workers' builds/tests/Kani paused. Both Rust medians must be at most
the smaller C++ median for every workload. All 34 pass; no failed measurement
was discarded or compensated by a faster workload:

| Manifest / faster-reference report | Cases | Maximum Rust/faster-C++ ratio |
| --- | ---: | ---: |
| [Numeric](value-expression-performance-checkpoint3-a-numeric-fastest.json) | 8 | 0.968104 |
| [Native API](value-expression-performance-checkpoint3-a-native-fastest.json) | 12 | 0.955703 |
| [Grouping](value-expression-performance-checkpoint3-a-grouping-fastest.json) | 3 | 0.936352 |
| [Ordering](value-expression-performance-checkpoint3-a-ordering-fastest.json) | 1 | 0.748576 |
| [Relational](value-expression-performance-checkpoint3-a-relational-fastest.json) | 10 | 0.950477 |

The linked summaries retain the release/development input reports and hashes.
All groups share source fingerprint
`fc0048adc62ac6428e88d94a53356fa336498214c90d0af07f464a7a1461cdb5`
and Rust worker SHA-256
`015bbe4b9910c11378adf79ec64693f50911dbae52cc2c3c2b19b6138dfc0df2`.
Build commands now explicitly disable default features. This preserves the
preceding 34-workload latency baseline; new-family performance coverage, memory,
I/O, durability and concurrency costs remain unmeasured.

### Validation and exploratory limits

The complete workspace suite passed at `28063ef`, with the same two ignored
external-CLI analytics tests. After the catalog-order repair, check, contracts
(23), nested (11), temporal (10), operators (10) and all-target clippy passed.
Instrumentation coverage reports 254 files, 2,205 functions, 205 interface
methods and no missing attributes; the final trace check passed in 34.06 s and
deleted telemetry. All 35 Python harness tests passed. Production CLI, test and
measurement workers built successfully without default features.

Full `python3 scripts/verify_kani.py` ran both before and after the regression
repair, using Kani 0.67.0: 6/6 harnesses passed in each run, zero failures. Final
times were TIMETZ packing 17.476 s, unsigned keys 1.078 s, dense offsets 0.701 s,
ROWS clipping 2.976 s, uniform bounds 2.920 s and packed byte counts 63.116 s.
Caller-location (1) and foreign-function (4) constructs remained unreachable;
atomics remained sequential. The parser dependency emitted an unused-variable
warning in verifier compilation. These bounded proofs do not prove recursive
values, new cast/decoder protocols, SQL binding, native compatibility,
transactions or concurrent execution. Ordinary tests/reference campaigns carry
those checks; unproved behavior remains explicit.

The milestone continues. Temporal scanner improvements and declared VARIANT
payload metadata are delivered worker increments awaiting subsequent integration;
VARIANT/TUPLE runtime behavior, BIT/BIGNUM/GEOMETRY, named types/index DDL,
remaining coercion/functions, nested WAL/defaults and broader combined workloads
remain required work, not deferred completion claims.

## Fourth integrated checkpoint, 2026-09-10

The combined source at `b43faaa` adds usable value/expression paths while the
same milestone remains active:

- BIT uses packed logical bits with explicit lengths, typed casts, all-width
  bitwise operators, scalar functions, aggregate/window behavior and native
  checkpoint/WAL paths. BIT children now participate in VARIANT. Independent
  native fixtures exposed and repaired empty-BLOB conversion and legacy FSST
  NULL handling. The [BIT report](bit-values.md) retains both failed and repaired
  reference trials, including still-open syntax and TRY_CAST context differences.
- VARIANT retains declared dynamic child metadata and selected adapters, exact
  integer/decimal number keys, object/array access and mixed relational behavior.
  It supports private persistence, not native VARIANT compatibility. TUPLE adds
  positional metadata, empty/singleton syntax, casts/accessors, mixed queries,
  prepared mutations and private reopen. Independent development ID110 streams
  are readable; native TUPLE publication remains explicitly unsupported until
  version-aware writing is integrated. See [nested values](nested-values.md).
- Streaming INTERVAL and clock scanners preserve checked progress and pinned
  parsing/rounding behavior. Text acceptance and physical validity are distinct:
  SQL constructors and casts can produce clocks slightly beyond 24h. The
  provisional physical domain includes these source-backed witnesses without
  broadening text grammar or claiming arbitrary native/API raw-value support.
  The [clock campaign](temporal-clock-domain-integrated.json) records development
  667/667 selected SQL cases and 2/3 expanded native producers; release is 602/667
  and 1/3. Nested native WAL remains an executable failing obligation at
  `cargo run --example temporal_nested_wal_obligation`, not an ignored pass.
- Native DECIMAL sentinels now become temporary NULL placeholders before the
  enclosing validity mask is applied, including every physical coefficient
  width. A valid row with no decoded value remains corruption. The independent
  TUPLE fixture exposed this compatibility bug.
- Shared binding preserves SQL literal identity across early CASE pruning and
  distinguishes typed API parameters from literals. Scalar and operator overloads
  can use fitting integer literals at all supported signed widths. Explicit
  casts, unary plus, CASE and parameters do not acquire that privilege merely
  because their values are constant. Prepared index lookups remain tested.
- Cast attempts retain invalid-input versus fatal failure origin independently
  of the public error category. Source/output validation and infrastructure
  failures cannot become TRY_CAST NULLs. LIST/ARRAY/STRUCT/UNION propagate partial
  child NULLs; invalid converted MAP keys reject the whole map; VARIANT extraction
  rejects the whole value on a child conversion failure. Ordinary CAST keeps
  the original diagnostic category. Tests cover selected replacements, malformed
  validators, both evaluators/optimizers, prepared updates and rollback.

The complete workspace suite passed at this checkpoint, retaining the two
existing external-CLI analytics omissions. Check and all-target clippy passed;
all 35 Python harness tests passed. Instrumentation coverage reports 273 files,
2,408 functions, 207 interface methods and no missing attributes. The trace check
passed in 66.20 s with zero errors/panics/open spans and deleted temporary telemetry.

Full `python3 scripts/verify_kani.py` ran with Kani 0.67.0 on this frozen combined
source: 6/6 maintained harnesses passed, zero failures. Times were TIMETZ packing
63.232 s, unsigned keys 0.828 s, dense offsets 0.511 s, ROWS clipping 2.583 s,
uniform bounds 2.847 s and packed byte counts 97.648 s. The TIMETZ proof now uses
the wider supported clock domain. Caller-location (1) and foreign-function (1)
constructs remained unreachable; atomic fence/subtraction constructs remained
sequential. The parser verifier build retains its unused-variable warning.
These bounded proofs do not establish parser, cast-protocol, recursive-value,
native-file, transaction or concurrent execution parity.

No new acceptance performance campaign or complete upstream refresh has run for
this checkpoint. The 34 passing measurements and no-lost-prefix upstream result
in checkpoint three belong to that earlier source, not this code. Further
regression investigation and a coordinated quiet campaign are required before
claiming preservation on the follow-up. The last pushed checkpoint is `ae0cd51`;
this fourth checkpoint and subsequent internal work are not yet pushed.
