# Value-and-expression integration

The [accepted milestone](../specs/value-expression-milestone.md) remains active.
The integration branch combines scalar, temporal and nested worker increments;
none of those families, or the larger milestone, is declared complete here.

Latest pushed, fully regression-checked engine: `27d190b`, in report commit
`9e19237`. This includes recursive bit-exact nested
checkpoint validation, ordered CASE/collection coercion, full-width integer
literals, ABS, calendar difference functions, native row-ID gaps/free tails and
historical/current deletion-mask compatibility. Full workspace check/tests and
all-target clippy, tracing and exploratory Kani pass for this ninth checkpoint.
The earlier nested-NaN failure is repaired. Production file campaigns and the
strict 34-workload faster-reference gate pass. The full upstream refresh has
487 passing files and 20,811 passing records, with no lost full-file pass or
decreased passed-record prefix against the last pushed checkpoint, `135eece`
(engine `88e9094`). This ninth checkpoint is validated for the follow-up push;
the broader milestone remains active. Earlier sections retain historical
results and limits.

## Continuing versioned WAL integration

The lead's [exact format-owned layout validation](variant-checkpoint-equivalence.md)
and [actual storage-version handoff](wal-storage-version.md) now combine with
the worker's native VARIANT WAL codec. Both recovery preparation and live-session
rebasing validate canonical values before publication. Actual storage 68 carries
VARIANT, and 69 additionally carries TUPLE/empty STRUCT, including nested children.
The initial six-version bidirectional WAL campaign passes all seven stages each;
15 independently produced C++ commit/rollback boundaries also survive writable
Rust recovery. The tenth integrated checkpoint below validates this increment.
The ninth checkpoint remains the latest measured performance
and full-upstream acceptance; no new follow-up push has occurred.

Scalar full-width inference, its directly affected COALESCE repair, and temporal
physical-constant provenance are now integrated through `320000a`. The first
combined focused check passes contracts 42, numeric 46, temporal 31, types 22,
nested 42, execution 51, checkpointing 18 and logging 7, with workspace check and
all-target clippy. Coverage is 343 files/3,253 functions/226 interface methods,
none missing. The source-specific worker campaigns record development 830/830
numeric cases and 1,288/1,289 temporal cases, with no lost prior passes; those
do not replace a combined full-upstream or performance refresh.

The lead's [stored-expression prerequisite](stored-expressions.md), `1bdf1a0`,
and selected scalar expansion `e93a1d6` pass the eleventh combined workspace,
tracing and exploratory Kani run below. Native DEFAULT decoding/catalog migration,
non-NULL nested defaults and the larger milestone remain open. The twelfth
checkpoint integrates NULLIF, its comparison-runtime and expansion-resource
repairs, calendar grids, and selected stored-expression startup/recovery context.
The following checkpoints integrate full timestamp payload validity, native
typed-Value metadata prerequisites and selected native decode/encode context.
No new follow-up push has occurred.

## Fifteenth integrated validation checkpoint

Frozen engine `81e49d2` adds the selected calendar overload and nested constructor
consumers, advertised argument labels, nullable IEEE setting, syntax-only nested
capture, and the shared native Value codec session. Full workspace check/tests,
all-target clippy with warnings denied and traced all-target check pass. The same
two external-CLI analytics tests remain ignored. Coverage reports 378 files,
3,666 functions and 239 interface methods, none missing. Tracing completes in
55.236 seconds with zero errors, panics or open spans; temporary telemetry is
deleted. One captured test-output chunk was truncated, so this report does not
reconstruct every individual suite count; the chained commands completed with
exit status zero.

Full `python3 scripts/verify_kani.py`, Kani 0.67.0, passes all six maintained
harnesses: TIMETZ packing 21.469 s, unsigned keys 0.844 s, dense offsets 0.494 s,
ROWS clipping 2.325 s, uniform bounds 2.771 s and packed byte counts 64.890 s.
There are zero failures. Caller-location (1) and foreign-function (4) warnings
remain unreachable in these proofs; atomic fences (4) and subtracts (5) remain
sequential, and the vendor-parser warning remains. These narrow proofs do not
establish retained DEFAULT semantics, codec budgets, overloads or concurrency.

The catalog/native expression path and prepared-plan settings retention remain
open. Worker math/formatter/membership increments are not part of this frozen
source. No new performance, full-upstream or production file campaign, or push,
is claimed. README still matches origin/main and the user's stash is untouched.

## Increments after checkpoint fourteen

The queued calendar overload consumer (`0101221`), selected ordinary/stored nested
constructors (`551b693`) and nullable IEEE-setting prerequisite (`5fce9f2`) are
integrated. Combined focused checks pass at `551b693`: contracts 76, nested 42,
numeric 51, temporal 41, settings 11, casts 13 and types 24, with workspace check
and all-target clippy. Coverage is 377 files/3,651 functions/239 methods, none
missing. The IEEE math consumers and prepared-binding setting retention are not
implemented by setting registration alone. The upcoming integrated checkpoint
will validate this new source; checkpoint fourteen's proof result is not reused
as its acceptance.

The retained numeric repair campaign
passes 944/944 development cases, 588/944 release cases and native 3/3 per pin,
preserving all prior development passes. Its oracle compares retained value/type
results and declared error categories, not complete error text. The
[calendar diagnostic campaign](temporal-diagnostics.md) passes 800/827 exact
development cases and 700/827 release cases with no lost passes; all 19 candidate
bodies now match, but missing source-location suffixes remain failed full-message
cases. Broader temporal development remains 1,288/1,289. These family reports do
not replace a combined full-upstream or faster-reference performance refresh.

## Fourteenth integrated validation checkpoint

Frozen engine `abf7eed` integrates selected native decoding (`b4a9c9b`) and
fresh/bound/recovery-successor encoding (`3e1452c`), the distinct recognized-input
NotImplemented category and VALUES repair, temporal cast costs, generic selected
overload requests and named/nested stored-tree metadata. Calendar rejection
categories are repaired; the calendar overload and nested constructor consumers
are still queued worker increments, not part of this frozen source.

Full workspace check/tests and all-target clippy with warnings denied pass. Only
the same two external-CLI analytics tests are ignored. Library 78, compatibility
19, contracts 71, numeric 51, temporal 38, nested 42, DATE 11, casts 13, types 24,
execution 51, checkpointing 18, logging 7 and recovery 14 pass. The exhaustive
recovery-tail sweep took 75.17 seconds. Coverage reports 373 files, 3,596 functions
and 239 interface methods, none missing. Traced all-target check passes in
83.533 seconds with zero errors, panics or open spans; temporary telemetry was
deleted. The five native-context contracts cover real recursive decoding,
selected callbacks, encoding limits, cancellation and unchanged files before
publication. They do not establish native parsed DEFAULT support.

Full `python3 scripts/verify_kani.py`, Kani 0.67.0, passes all six maintained
harnesses: TIMETZ packing 29.897 s, unsigned keys 0.853 s, dense offsets 0.501 s,
ROWS clipping 2.393 s, uniform bounds 3.984 s and packed byte counts 84.951 s.
There are zero failures. Warnings now list caller-location (1) and foreign
functions (4), unreachable in these verified harnesses; atomic fences (4) and
subtracts (5) remain sequential, and the vendor-parser warning remains. These
proofs do not establish context/publication protocols, stored-expression
semantics, overload correctness or general concurrency.

All 40 Python verification tests pass, including the generated SQLite transaction
and predicate-partition comparisons and a production test-worker build. An initial
two-module invocation from the repository root had import-path errors; rerunning
from scripts passed 21 tests, then normal discovery passed all 40. No oracle or
engine change was made to repair that invocation.

The [default-demand investigation](stored-expressions.md) now distinguishes
deleted physical rows before and after CHECKPOINT: development demands a failing
ADD default before checkpoint, but succeeds after dropping the deleted storage.
Neither visible cardinality nor a historical row-ID high-water mark is sufficient
to model this lifecycle. Retained defaults and prepared-plan binding settings
remain lead-owned correctness gaps. No new performance/native-file/full-upstream
acceptance or follow-up push occurred. A fresh fetch found 44 local commits and
no incoming commits; README still matches origin/main and the user's stash is
untouched. Checkpoint nine remains the pushed performance/upstream baseline.

## Thirteenth integrated validation checkpoint

Frozen engine `26c709b` adds ordered VALUES literal inference, reserved NULLIF
syntax and qualified-name regression repair, the complete physical timestamp
MIN domain, and the independent native typed-Value codec prerequisite. The latter
still has test-only registration pending parsed-expression integration; its
fixtures are not evidence of connected native DEFAULT support.

Full workspace check/tests and all-target clippy with warnings denied pass. Only
the same two external-CLI analytics tests are ignored. Library 78, compatibility
19, contracts 60, numeric 50, temporal 38, nested 42, DATE 11, casts 12, types 23,
execution 51, checkpointing 18, logging 7 and recovery 14 pass. The exhaustive
recovery-tail sweep took 63.52 seconds. Coverage reports 368 files, 3,506
functions and 231 interface methods with none missing. Traced all-target check
passes in 51.523 seconds with zero errors, panics or open spans; temporary
telemetry was deleted. These durations are not performance measurements.

Full `python3 scripts/verify_kani.py`, Kani 0.67.0, passes all six maintained
harnesses with zero failures: TIMETZ packing 48.375 s, unsigned keys 0.800 s,
dense offsets 0.489 s, ROWS clipping 2.222 s, uniform bounds 2.723 s and packed
byte counts 80.098 s. The same unreachable caller-location/foreign-function,
sequential atomic-fence/subtract, and vendor-parser warnings remain. These proofs
do not establish the new native Value protocol, DEFAULT behavior, general
concurrency, calendar semantics or expansion resource bounds.

Native parsed DEFAULTs, retained catalog expressions and connected default demand
remain open. No new production performance, native-file or full-upstream campaign
ran on this source. Checkpoint nine remains the latest pushed faster-reference
and full-upstream acceptance, and checkpoint ten the latest production native-file
campaign. No follow-up push is implied by this internal checkpoint.

## Twelfth integrated validation checkpoint

Frozen engine `6f5e6ac` combines the lead's selected closed-expression evaluation
and startup/recovery composition (`c71ee58`, `111f552`), NULLIF (`8fc1743`), actual
argument-weight expansion bounds (`5e4b3fb`), constant-NULL comparison execution
(`a26bc4d`) and calendar truncation/bucketing (`6f5e6ac`). The expansion bug found
at checkpoint eleven is repaired before lowering/cloning; its dedicated nested
duplicator and combined argument/template-depth regressions pass. Comparison
execution preserves already demanded operands, selected validation, physical
provenance and fatal errors across scalar/batch/predicate paths.

Full workspace check/tests and all-target clippy with warnings denied pass. Only
the same two external-CLI analytics tests are ignored. Library 68, compatibility
19, contracts 60, numeric 48, temporal 35, nested 42, DATE 11, casts 12, types 22,
execution 51, checkpointing 18, logging 7 and recovery 14 pass. The exhaustive
recovery-tail sweep took 65.22 seconds. Coverage reports 358 files, 3,442
functions and 231 interface methods with none missing. Traced all-target check
passes in 73.942 seconds with zero errors, panics or open spans; temporary
telemetry was deleted. These durations are not performance measurements.

Full `python3 scripts/verify_kani.py`, Kani 0.67.0, passes all six maintained
harnesses with zero failures: TIMETZ packing 43.590 s, unsigned keys 0.808 s,
dense offsets 0.492 s, ROWS clipping 2.449 s, uniform bounds 2.871 s and packed
byte counts 75.105 s. Caller-location (1) and foreign-function (2) warnings remain
unreachable; atomic fences (4) and subtracts (5) remain sequential. The vendor
parser's unused-variable warning remains. These proofs do not cover stored
expression evaluation/composition, expansion budgets, calendar semantics,
native protocols or general concurrent durability.

The source-specific NULLIF campaign, retained
in `f7bc459`, passes 894/899 development cases and 538/899 release cases under
the retained value/type and declared-error-category oracle (not full error text);
native paths pass 3/3 per pin. All preceding 830 development passes remain.
Five new cases exposed VALUES literal-inference and reserved NULLIF arity gaps;
their worker repairs are not part of this frozen source. The timestamp MIN
physical-domain/native-validity repair and independent typed Value codec are
also queued for the next integrated increment, not counted as acceptance here.

The lead's [default-demand investigation](stored-expressions.md) confirms that
CREATE/SET DEFAULT and never-populated ADD COLUMN must retain failing expressions
without evaluating them, while development can demand backfill for deleted
physical rows even with zero visible rows. Existing eager-default failures and
the previously isolated raw timestamp planner-name mismatch remain open. Context
transport contracts do not substitute for the native parsed-expression codec,
catalog migration or connected default execution.

No new production performance or full-upstream refresh ran on this source.
Checkpoint nine remains the latest pushed faster-reference/upstream acceptance;
checkpoint ten remains the latest production native-file campaign. A fresh
remote fetch found 27 local commits and no incoming commits. README still matches
`origin/main`; the user's pre-sync stash remains unchanged.

## Eleventh integrated validation checkpoint

Frozen engine `e93a1d6` passes full workspace check/tests and all-target clippy
with warnings denied. Only the same two external-CLI analytics tests are ignored.
Library 67, compatibility 19, contracts 49, numeric 46, temporal 31, nested 42,
DATE 11, casts 12, types 22, execution 51, checkpointing 18, logging 7 and recovery
14 pass; the full recovery-tail sweep took 62.11 seconds. Coverage reports 348
files, 3,302 functions and 228 interface methods, none missing. Traced all-target
check passes in 57.059 seconds with zero errors, panics or open spans; temporary
telemetry was deleted. The earlier stored-binder-only pass had contracts 45,
coverage 346/3,274/227 and a clean 69.554-second trace check.

Full `python3 scripts/verify_kani.py`, Kani 0.67.0, passes all six maintained
harnesses with zero failures: TIMETZ packing 47.055 s, unsigned keys 0.837 s,
dense offsets 0.495 s, ROWS clipping 2.445 s, uniform bounds 2.791 s and packed
byte counts 62.017 s. Caller-location (1) and foreign-function (2) warnings
remain unreachable; atomic fences (4) and subtracts (5) are modeled sequentially.
These proofs do not establish scalar expansion bounds, stored-expression
semantics, physical vector provenance, calendar behavior or native protocols.

An integration review identified an open expansion resource bug despite these
passing checks: a template's Argument leaf counts as one node, but lowering
clones the entire supplied bound argument. Nested duplicating calls can exceed
the intended total bound. The scalar owner is adding actual argument subtree
weights and combined depth/occurrence validation before cloning, with dedicated
regressions. The checkpoint does not waive that repair or declare the expansion
slice complete. Its NULLIF family and comparison-runtime follow-ups are also
still worker-local.

The lead additionally isolated the [raw timestamp DEFAULT mismatch](stored-expressions.md)
to development's optimizer/planner-name behavior using independent disabled-pass
probes and the source call chain. The physical timestamp remains valid. Native
function DEFAULT decoding and selected catalog/runtime integration are still
implementation work, not passing file-compatibility claims. The nested review
found no new canonical identity/version blocker, but recorded the pre-existing
non-cooperative successor-encoding context boundary for shared persistence work.

No new production performance or full-upstream refresh was run on this source.
Checkpoint nine remains the latest pushed faster-reference/upstream acceptance;
checkpoint ten remains the latest production native-file campaign. README still
matches `origin/main`, and the user's pre-sync stash is unchanged.

## Tenth integrated implementation checkpoint

Frozen engine `f9c0911` passes full workspace check/tests and all-target clippy
with warnings denied. The same two external-CLI analytics tests remain ignored.
Library 67, compatibility 19, numeric 40, nested 42, contracts 33, DATE 11,
temporal 29, casts 12, types 20, checkpointing 18 and recovery 14 pass. The full
recovery-tail sweep took 69.34 seconds. Instrumentation coverage reports 337
files, 3,143 functions and 224 interface methods, none missing. Traced all-target
check passes in 92.414 seconds with zero errors, panics or open spans; temporary
telemetry was deleted. Instrumented durations are not performance acceptance.

Full `python3 scripts/verify_kani.py`, Kani 0.67.0, passes all six maintained
harnesses with zero failures: TIMETZ packing 49.152 s, unsigned keys 0.860 s,
dense offsets 0.511 s, ROWS clipping 2.359 s, uniform bounds 2.917 s and packed
byte counts 90.950 s. Caller-location (1) and foreign-function (2) warnings
remain unreachable; atomic fences (4) and subtracts (5) are modeled sequentially.
These bounded proofs do not establish canonical VARIANT equivalence, selected
format validation, native codec/recovery protocols, general expressions or
concurrent durability. Connected tests and independent file readers are separate
evidence for the new storage paths.

The production checkpoint campaign
passes all six versions and all five stages per version. The production
WAL campaign passes all six versions and
all seven stages, including initial/final checkpoints, rollback, Rust and C++
mutations, and Rust mutation after a C++ checkpoint. Every development twin is
also read by Rust. Release agrees for storage 64–68; its storage-69 rejection
remains an explicit version limit. The production
deletion-mask campaign passes all
four retained independent fixtures. All three reports record unchanged source
`defec4314138d31a8165b827dd4dc5a2c2aa2644e47afc9049bc965e9728973b`
and production shell binary
`63febbf4bc5330036ba8c5c3d094a7d5fb3bfc03b3d6b82fb8cfa72e857a7939`.

This closes the current integrated versioned-WAL validation stage, not the
value-and-expression milestone. New performance and full-upstream regression
campaigns have not run on this source; checkpoint nine remains the latest
follow-up pushed after those gates. Worker numeric/temporal reports remain
source-specific until their complete repaired increments are integrated.

## Ninth integrated implementation checkpoint

At `27d190b`, the full workspace suite passes with the same two external-CLI
analytics tests ignored. Library 61, compatibility 19, numeric 40, nested 41,
contracts 32, DATE 11, temporal 29, casts 12, types 18, checkpointing 13 and
recovery 14 pass. The full recovery-tail sweep took 62.31 seconds. Coverage
reports 328 files, 3,053 functions and 216 interface methods, no missing
instrumentation. Trace compatibility passes in 44.096 seconds with no errors,
panics or open spans; temporary telemetry was deleted.

Full `python3 scripts/verify_kani.py` with Kani 0.67.0 passes all six maintained
harnesses, zero failures: TIMETZ packing 44.857 s, unsigned keys 0.781 s, dense
offsets 0.473 s, ROWS clipping 2.080 s, uniform bounds 2.499 s and packed byte
counts 60.287 s. Caller-location (1) and foreign-function (4) warnings remained
unreachable; atomic fences (4) and subtracts (5) were modeled sequentially.
These proofs do not establish native row-group/deletion/free-list correctness,
recursive checkpoint identity, general SQL binding, calendar semantics, VARIANT
canonicalization or concurrent durability. Those remain distinct evidence scopes.

The lead repaired [recursive nested IEEE layout comparison](nested-checkpoint-exactness.md),
[native row identity and free tails](fresh-native-values.md), and
[deletion-mask compatibility](native-deletion-identity.md). A read-only worker
review found two additional issues before push: nonzero-group deletion origins
and an overly permissive trailing append watermark. A relative-only deletion
edit then regressed the retained v1.3 fixture; the final source follows C++'s
actual vector-index addressing and preserves the obsolete wire field. The
historical regression and all new independent fixtures now pass without removing
assertions or mask bounds. Initial failures remain recorded.

The expanded production file campaign
passes all six storage versions and all five stages per version: creation,
rollback, Rust commit, C++ commit, and a Rust mutation after C++ checkpointing.
Rust also reads every independently produced development twin. Release agrees
for storage 64–68; its storage-69 rejection remains an expected version limit.
The repaired production deletion campaign
passes all four retained release/development files. Both production reports
record unchanged source `4ce64917ee194eff0d2628cbfffce8caa10663ab499ce8de146064f64feea328`
and shell binary `dca261a4e93f537387e061fce10bb1b6d82f2f0ea6f0523cbc6d2aed5af99869`.
The earlier passing debug reports are retained separately.

All 34 measured workloads pass the strict faster-reference gate with three
warmups and 21 paired, alternating-order samples against each pin. Both Rust
campaign medians are gated against the smaller C++ median; no tolerance or
median selection was added. Maximum ratios by suite are numeric 0.958160 (8),
native 0.959253 (12), grouping 0.932525 (3), ordering 0.715233 (1), and relational
0.969849 (10). Reports are `value-expression-performance-checkpoint9-<suite>-{release,development,fastest}.json`.
Every campaign uses the same measurement source
`3a9d208a7579ec91da3a55e63c5959970209e73126751dcaa4879d7c09638e1a`
and Rust measurement binary
`fc063459719ab08ef70d987f33e88f8c8afe874813ddd83db8b6ba091f670cfe`.
Workers and root proof/build jobs were finished before measurement. This is
serial in-memory embedded-API latency evidence, not coverage of new types,
CPU/memory, cold I/O, durability, concurrency or the entire performance target.

The controlled full upstream refresh
accounts for the same 5,638 unique file identities, archive, manifest and upstream
revision as checkpoint5-d, with the same three-second deadline and two workers.
Outcomes are 487 passed, 2,027 failed, 3,110 unsupported, 11 timeout and three
incomplete; full-suite parity is false. Passed records increase from 19,241 to
20,811. Comparing every file identity finds 64 newly passing files, no lost full
pass and no decreased passed-record prefix. These combined engine/harness
results include the documented exact numeric-oracle repair; they are not 64
independent engine-only feature claims. No new worker-exit error appeared.
The additional timeout, `test/sql/setops/test_joins_under_setops.test_slow`,
advances from three passed records and a numeric-rendering assertion failure to
four passed records before its deadline. Its later statement remains unverified,
not waived. Source identity is
`4ce64917ee194eff0d2628cbfffce8caa10663ab499ce8de146064f64feea328`;
the production worker is
`5e14dfc83931cc23642260d49a9e34cbb01102537fad34f48272d0a184242673`.
The report and complete journal retain all failures and unsupported obligations.

The scalar [binding follow-up](numeric-binding-gaps.md) repairs all three prior
numeric SQL gaps: all 672 prior development cases pass, expanded coverage is
702/705, and the three broader unsigned CASE/list combinations remain explicit.
Calendar [difference work](temporal-differences.md) retains its independent
constant-provenance and stored-default obligations. Worker campaigns are
source-specific, separate from the combined upstream refresh above.
Native VARIANT recovery equivalence, WAL capability handoff, general stored
default expressions and the wider milestone remain active work. The combined
workspace, exploratory Kani, native-file, strict performance and full upstream
identity/prefix checks complete this checkpoint's pre-push regression review.
README matches `origin/main`; the user's pre-sync stash is unchanged.

## Eighth integrated implementation checkpoint

Source `4b2a027` passes ordinary workspace check/tests and all-target clippy with
warnings denied. The same two external-CLI analytics tests remain ignored.
Library 56, nested 37, numeric 35, contracts 29, DATE 11, temporal 25, casts 12,
checkpointing 11 and recovery 14 tests pass; the recovery tail sweep took
65.72 seconds. Coverage reports 315 files, 2,919 functions, 214 interface methods,
with no missing instrumentation. Trace compatibility passed in 61.635 seconds,
with zero errors, panics or open spans; temporary telemetry was deleted.

Full `python3 scripts/verify_kani.py` under Kani 0.67.0 passed all six maintained
harnesses, zero failures: TIMETZ 45.299 s, unsigned keys 0.790 s, dense offsets
0.477 s, ROWS clipping 2.196 s, uniform bounds 2.835 s and packed byte counts
83.864 s. Caller-location (1) and foreign-function (2) warnings remained
unreachable; atomic fences (4) and subtracts (5) were modeled sequentially.
These proofs do not establish native codec/state-machine correctness, exact
VARIANT equivalence, general rounding/binding semantics or concurrent durability.

The [native publication report](nested-publication-values.md) records repaired
4/4 independent C++ file mutation campaigns. Numeric precision's retained
[repaired report](numeric-precision-values.md) reaches 606/607 development SQL
cases, with bare maximum UHUGEINT literal binding still open; native cases pass
3/3 on both pins. The MAP worker's initial/follow-up evidence retains three
integer-literal inference gaps. Worker evidence is source-specific; these
campaigns are not a fresh full upstream run on the combined engine.

The lead's [nested-NaN investigation](nested-checkpoint-exactness.md) confirms
an additional maintenance failure after the checkpoint. Both readers can reopen
the committed log; C++ can checkpoint it and Rust can read that result. Rust's
strict layout checker needs recursive bit-exact comparison. This and subsequent
family repairs receive their own connected checks before the follow-up push.
No new faster-reference timing or complete upstream campaign has run. README
still matches `origin/main`, and the user's pre-sync stash remains unchanged.

## Seventh integrated implementation checkpoint

The later [versioned native nested publication increment](nested-publication-values.md)
repairs all four recorded committed-writer failures. Independent C++ files now
survive Rust mutation and reopen with development-correct content and retained
versions/identifiers. Selected services, recursive version gates and connected
transaction tests are in place. WAL capability handoff and exact VARIANT recovery
layout validation remain explicit integration work. This increment is queued
for the next combined workspace/Kani checkpoint, not a new timing acceptance.

At `8bd50a3`, `cargo check --workspace`, `cargo test --workspace`, and all-target
clippy with warnings denied pass. The same two external-CLI analytics tests
remain ignored, not passing obligations. The suite includes 51 library tests,
32 numeric, 31 nested, 25 temporal, 9 DATE, 12 cast, 29 contract, 11 checkpoint,
14 compatibility and 14 recovery tests. The full recovery tail sweep took
63.73 seconds. Coverage reports 305 files, 2,796 functions, 212 interface methods
and no missing instrumentation. Trace compatibility passed in 57.989 seconds,
with zero errors, panics or open spans; temporary telemetry was deleted.

`python3 scripts/verify_kani.py` ran with Kani 0.67.0: all six maintained
harnesses passed, zero failures. Times were TIMETZ packing 48.149 s, unsigned
keys 0.789 s, dense offsets 0.498 s, ROWS clipping 2.319 s, uniform bounds
2.686 s and packed byte counts 57.611 s. Caller-location (1) and foreign-function
(4) warnings remained unreachable; atomics (5 subtract, 4 fence) were modeled
sequentially. These proofs do not cover the new publication-state protocol,
native VARIANT codec, SQL binding, general temporal semantics or concurrency.
The command was run at the combined checkpoint; that is not a proof of every
changed invariant.

The new native publication diagnostic retains
four independent C++-produced files: development unshredded/shredded VARIANT,
release shredded VARIANT, and development TUPLE/empty STRUCT. Original reads and
rollback paths match development in all four. Committed Rust mutations fail
explicitly in all four at the still-disabled writer paths; later mutation stages
are unexecuted. The production binary was built with `--release --no-default-features`.
Both pinned reader outcomes, fixture and binary hashes are retained. This is
not an upstream refresh, timing gate, or full file-compatibility claim.

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
checkpoint campaign
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

The immutable numeric campaign
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
| Numeric | 38/38 | 20/38 | 3/3 |
| BLOB/UUID | 25/25 | 23/25 | 3/3 |
| ENUM | 28/29 | 28/29 | 6/6 |
| Temporal functions | 437/437 | 412/437 | 3/3 |

The numeric/BLOB refreshes use engine `28063ef`, preceding the catalog-order
repair below. Development remains authoritative for the retained release
differences. ENUM boundary row-zero broadcasting remains open. The
binding corpus preserves
all 92 records against each reference. These are selected workloads, not full
type/function/catalog/native parity.

### Regressions investigated before pushing

The first full upstream run
increased passing files but lost two previously passing negative-test files and
three passing records in another file. Shared scalar-call refactoring had moved
catalog lookup after argument binding: unsupported ANY/star/tuple arguments
masked a missing-function error. `2536548` restores catalog-first lookup and
passes the same selected function into binding. Regression tests retain this
ordering and still check known functions' arguments.

The repaired run accounts for all
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
| Numeric | 8 | 0.968104 |
| Native API | 12 | 0.955703 |
| Grouping | 3 | 0.936352 |
| Ordering | 1 | 0.748576 |
| Relational | 10 | 0.950477 |

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
  The clock campaign records development
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

## Continuing integration and regression repair, 2026-09-10

The first frozen follow-up at `0e8a4f0` passed the full workspace suite, check
and all-target clippy, with the same two external-CLI analytics omissions.
Coverage reported 276 files, 2,437 functions, 207 interface methods and no
missing attributes. The trace check passed in 139.48 s with zero errors,
panics or open spans and deleted its temporary telemetry. These instrumented
times were collected during parallel development, not performance acceptance.

Full `python3 scripts/verify_kani.py` ran on that compiled source with Kani
0.67.0: 6/6 harnesses passed, zero failures. Times were TIMETZ packing 49.654 s,
unsigned keys 0.807 s, dense offsets 0.539 s, ROWS clipping 3.258 s, uniform
bounds 3.309 s and packed byte counts 82.338 s. Caller-location (1) and foreign
function (2) constructs remained unreachable; atomic fence/subtraction remained
sequential. The parser verifier build retained its unused-variable warning.
The proofs do not establish the parser, recursive recovery, cast protocol or
native interchange behavior described below; later integrated changes require
their own combined checkpoint.

Confirmed repairs and usable subsequent increments include:

- Literal-sensitive binding had caused a plan regression: a constant CASE
  predicate stopped selecting its existing index. New optimizer and index
  witnesses failed before repair. Constant normalization now runs after
  overload binding; independently selected index/filter passes also recognize
  the relevant execution constants. Result types and selected adapters remain
  unchanged. This is a repaired plan regression, not a measured latency claim.
- FLOAT/DOUBLE integral casts use development's ties-to-even rounding before
  checked conversion. DECIMAL rounding remains distinct. The retained
  floating campaign reports 62/62
  development cases and 34/62 release cases, with native producers 3/3 on both.
- Shared recursive WAL vectors now carry nested temporal, decimal and BIT
  values through prepared mutations and reopen. Physical STRUCT child and
  validity records stage in a transaction-private tree and validate together,
  including temporarily inconsistent UNION states. Independent fixtures cover
  12 committed states and 10 writable recovery/checkpoint/reopen continuations.
  Scalar WAL traversal borrows values, removing an introduced deep-copy cost.
  The interchange refresh records
  all four producer paths passing against both references; no throughput claim.
- Calendar timestamp construction checks the same ordered intermediate
  arithmetic as development. The previously failing mixed nested-clock WAL
  obligation now passes unchanged and is an ordinary component test. The
  boundary campaign records
  development 688/688 SQL and 3/3 native producers, release 623/688 and 2/3.
  Subsequent renderability work separates valid raw instants, infallible
  diagnostic display, fatal SQL casts and fallible result serialization; its
  696-case campaign preserves exact
  reference categories and remaining lower-nanosecond parsing work.
- Expanded BIT modifier testing exposed a new parser bug after the original
  signed-modifier repair: custom `BITSTRING(+1)` must reject a nonconstant
  expression, while BIT's own grammar discards it. The failed 69/70 development
  trial is retained in the [BIT report](bit-values.md). The repaired parser
  preserves quoted-string versus integer modifier identity and lexical
  parameter positions. Its refreshed differential campaign remains pending.
- BIGNUM's compact sign/limb representation now has selected casts, comparisons,
  keys, SQL parameters and initial native-codec/mutation/reopen paths. This is
  a prerequisite, not complete arithmetic, native interoperability or dynamic
  VARIANT parity; the [BIGNUM report](bignum-values.md) lists those obligations.
- Scalar specializations can declare per-argument cast modes. The language
  binder inserts retained checked casts through its selected registry, keeping
  literal identity, typed parameters, NULLs and fatal failure origin separate.
  The interface and integrated renderability paths pass contracts (26), casts
  (11), nested (21), temporal (20), BIGNUM (2), check and clippy. Coverage reports
  281 files, 2,509 functions, 208 interface methods and no missing attributes.

The milestone remains active. No fresh full upstream or faster-reference
performance campaign has yet validated these follow-up commits. The preceding
34 passing measurements belong to checkpoint three. The last pushed source is
still `ae0cd51`; regression investigation and the controlled combined checkpoint
must precede the next push.

## Fifth integrated checkpoint, 2026-09-10

Frozen engine `b1cb75c` combines exact BIGNUM operators/SUM/windows, nested
BIGNUM keys, physical child/validity WAL recovery, temporal text/renderability
repairs and scalar/LIST/ARRAY concat. Function-selected argument cast modes
survive binding: an explicitly selected assignment cast is not replaced by
SQL literal privilege. Its new witness failed before repair. Scalar concat
uses retained VARCHAR conversions instead of diagnostic Display; sequence
concat retains child types/casts and nested NULLs through relational execution,
prepared mutations, rollback and native reopen.

The selected Boolean/integral common-type rule now reaches CASE, VALUES and
ordinary set operations. Shared combination binding first asks the selected
implicit cast, then retains an explicit cast when needed after common-type
selection. It does not widen normal function overloads or table-assignment
casts. Tests exercise both evaluator/optimizer compositions and selected
implicit/explicit replacements. The prior implicit-only implementation fails
the new test. The paired combination corpus
passes unchanged against Rust and both C++ pins, including typed results,
joins, grouping, windows, table mutations and rollback. Broader CASE/literal
and recursive-CTE type selection remain separate obligations.

The full upstream refresh retains
all 5,638 identities with the same three-second deadline and two workers:
423 passed, 2,090 failed, 3,111 unsupported, 11 timed out and three incomplete.
It records 19,241 passed SQL instances, including prefixes of failing files;
these are not all passing files. None of checkpoint three's 391 full passes
is lost. Its sole shorter record prefix is the already-timing-out million-row
`constant_columns.test_slow` (seven records instead of eight) during concurrent
correctness builds. A quiet three-second retry
also reached seven. The extended diagnostic
preserves all 13 successful assertions before the existing 512 MiB checkpoint
writer limit, so it covers the previously successful prefix without declaring
the file passed. The limit exists unchanged in `ae0cd51`.

A separate clean worktree rebuilt `ae0cd51` with the same production flags.
The paired deadline investigation
ran that source and current source alternately, three pairs, without worker
build/proof load. Both completed seven, eight and eight records in the same
pairs before the unchanged three-second deadline. All outcomes and source/
binary identities are retained. This does not reproduce a new source regression;
it does not waive the timeout, writer limit or unmeasured storage performance,
and is not the C++ performance acceptance campaign.

Additional retained family evidence includes development BIT 77/77 with 3/3
native producer paths per pin, BIGNUM 45/45 with 3/3 per pin, temporal lower-NS
709/709 with 3/3 development native paths, and nested concat 16/16 with two
independent native producers and three mutation/reopen stages each. Every
report keeps its own source identity and release disagreements. The VARIANT
report remains failing 11/14: independent optimizer-setting probes demonstrate
that stored NULL/count behavior is optimizer-sensitive, not evidence to invent
a present-NULL physical representation. Default development mismatches remain
open; disabling a reference optimizer does not redefine the authority.

Normal check, full workspace tests and all-target clippy pass, with the same
two ignored external-CLI analytics tests. Harness self-tests pass 36/36.
Coverage reports 284 files, 2,559 functions, 208 interface methods and no missing
attributes. The trace check completes in 45.67 s, with zero error returns,
panics or open spans, and deletes its temporary telemetry. It is not a timing
benchmark. Full `python3 scripts/verify_kani.py` ran with Kani 0.67.0: all six
maintained harnesses passed, zero failures. Times were TIMETZ packing 45.481 s,
unsigned keys 0.810 s, dense offsets 0.496 s, ROWS clipping 2.449 s, uniform
bounds 2.357 s and packed byte counts 70.708 s. Caller-location (one) and foreign
function (one) constructs were unreachable; atomic fences/subtractions remain
sequential. The vendor parser retains its unused-variable warning. These
bounded proofs do not prove BIGNUM arithmetic, recursive recovery, temporal
parsing/rendering, native interchange or concurrent durability. Faster-reference
performance acceptance and the pre-push report update are still pending.

The first isolated faster-reference campaign records 33/34 passing workloads.
The numeric matrix
fails `decimal_total_cents`: release C++ is faster at 50,292 ns, while the two
Rust medians are 49,792 ns and 54,042 ns (maximum ratio 1.074565). Passing each
pin separately does not satisfy the faster-reference rule. The other numeric
cases and all native/grouping/ordering/relational cases pass. All 21 paired
samples per pin and all failed results remain retained; the follow-up push is
held for repair of this failed gate. The shared aggregation repair is ongoing.

The second numeric trial
on `c3a03f6` keeps all 34 workload identities and again passes 33. Independent
arithmetic/validity lanes reduce the worst decimal ratio to 1.020428, but the
threshold is not rounded into a pass: the exact baseline is 50,959 ns,
and Rust medians are 49,125 ns and 52,000 ns. This trial remains failed.

The third numeric trial
on `487d8f0` replaces conditional-zero/validity bookkeeping with checked
coefficient loads; arithmetic lanes, prefix bounds, cancellation and wide
fallbacks remain. Again 33/34 cases pass: decimal total has Rust medians
49,541 ns and 52,166 ns against the faster release median of 51,625 ns,
maximum ratio 1.010479. This is still a failure, not a rounded pass. All three
campaigns retain every sample and unchanged workload identity. Shared scan and
aggregation overhead require further investigation before the follow-up push.

On that frozen source, full workspace tests pass with the same two external-CLI
ignores; focused numeric/execution/grouping/BIGNUM tests and all-target clippy
also pass. Coverage reports 284 files, 2,562 functions, 208 interface methods,
no missing attributes. The trace check completed in 39.15 s with zero errors,
panics or open spans and deleted its telemetry. Full Kani 0.67.0 verification
passed all six harnesses: TIMETZ 49.038 s, unsigned keys 0.837 s, dense offsets
0.506 s, ROWS clipping 2.319 s, uniform bounds 2.824 s, packed counts 86.956 s.
The same unreachable/unsupported constructs and sequential-concurrency limits
apply. No new proof of SUM arithmetic or end-to-end engine correctness is claimed.

The post-reduction full upstream run
again executes all 5,638 files: 423 passed, 2,091 failed, 3,110 unsupported,
11 timed out and three incomplete, with 19,240 passed records. No full-file
pass is lost. The dictionary-emission join file reaches 29 records before its
three-second deadline, instead of the preceding 30 then unsupported result.
The first focused diagnostic
was named `quiet` prematurely: another worker's already-running compilation
was still active; it reached 26. The actual isolated retry
reaches 29. An extended diagnostic
passes all 30 prior assertions in 3.040 s, then reaches the same unsupported
`IS NOT DISTINCT FROM` join. This restores assertion coverage, not the
three-second outcome; source-paired timing investigation remains open. The
other changed outcome is the already-failing large grouping-set insert reaching
its existing 512 MiB writer limit before the deadline instead of timing out.

The source-paired join diagnosis
now compares a clean production rebuild of last-pushed `ae0cd51` with `7812862`:
three alternating pairs reach 26/29, 30/30 and 30/30 previous/current records.
Both final pairs reach the same unsupported expression; neither loses a prior
assertion in the current engine. This does not reproduce a new deadline
regression, and does not waive either file's timeout or unsupported join. Source
and binary identities are unchanged throughout; this is not C++ performance
acceptance. The clean temporary baseline worktree was removed afterward.

Sampling `487d8f0`'s prepared decimal-total query placed 1,104 of 1,404 sampled
executor stacks in the coefficient reduction. This was diagnostic sampling,
not a timing acceptance result; temporary profiler files were deleted. The next
candidate (`7812862`) moves loader/magnitude selection outside individual blocks
and widens the final proven block rather than each arithmetic lane. Its new
scalar-versus-column regression covers widths 12/15/16/18, signed tails, empty
input, block boundaries and totals beyond i64. Focused checks/tests/clippy pass.
The fourth numeric trial
still fails decimal total: 49,083/51,291 ns against release 50,750 ns, maximum
ratio 1.010660. Only the eight numeric cases were rerun in this trial (seven
pass); the remaining 26 were not repeated for this failed candidate. No new
full-suite/Kani completion is claimed for this ongoing reduction experiment.

The fifth numeric trial
on `88e9094` uses four checked arithmetic lanes with the retained column-level
selection and completed-block widening. Decimal total passes with Rust medians
46,917/48,125 ns against the faster release median of 50,458 ns (maximum ratio
0.953764). No validation, overflow bound or cancellation check was removed, no
unsafe code or persistent coefficient cache was added, and earlier failures
remain retained. The smaller checked loop is a measured improvement; a specific
CPU scheduling explanation has not been independently established.

The same quiet campaign completes all 34 unchanged workload identities, each
with three warmups and 21 paired samples against both pins. All pass the strict
faster-reference gate, with one identical Rust source fingerprint across the
ten reports. Maximum ratios by suite are numeric 0.953764,
native 0.964852,
grouping 0.933296,
ordering 0.696011,
and relational
0.926531. Implementation workers paused builds/proofs/reference probes during
measurement; ordinary user applications remained running. This closes the
recorded decimal timing gap for this source/configuration, not unmeasured CPU,
memory, cold I/O, durability, concurrency, new-family workloads or full parity.
Focused numeric 28, execution 51, grouping 9, BIGNUM three and reduction units
two pass with check/clippy. The final full workspace/upstream/Kani refresh is
running before publication; the last pushed source remains `ae0cd51`.

The concurrent final-source upstream refresh
records all 5,638 identities on `88e9094`: 420 passed, 2,090 failed,
3,110 unsupported, 15 timeouts and three incomplete; 19,235 passed records.
Three prior full passes now hit the unchanged three-second deadline:
`insert_from_many_grouping_sets.test` (9/10),
`insert_from_many_groups.test_slow` (13/14), and
`large_string_null_update.test_slow` (5/7). The already-incomplete complex-group
insert and unaligned-update files also have shorter prefixes (1 versus 2,
and 5 versus 6). No new result assertion mismatch is recorded. These losses
are not waived: this campaign ran alongside workspace/proof work, and a single
controlled full-suite rerun is planned after all heavy jobs are idle. It will
retain the same assertions, all file identities, deadline and worker count.
The current follow-up remains unpushed pending that investigation and Kani.

Final-source workspace tests complete successfully (the same two pre-existing
external-CLI analytics ignores); recovery tail truncation completes in 67.30 s.
Coverage is 284 files / 2,565 functions / 208 interface methods, with no missing
attributes. Trace compilation finishes in 44.58 s with zero errors, panics or
open spans; temporary telemetry is deleted. Full Kani 0.67.0 passes all six
maintained harnesses: TIMETZ 46.413 s, unsigned keys 0.796 s, dense offsets
0.487 s, ROWS 2.188 s, uniform bounds 2.302 s, packed byte counts 72.068 s.
Caller-location (one) and foreign-function (two) unsupported constructs remain
unreachable in these proofs; atomic operations remain sequential, and the
vendor parser's unused-variable warning remains. These bounded results do not
prove the new reducer, full SQL/storage behavior or concurrency. The controlled
full upstream investigation is the remaining pre-push check for this source.

The controlled full-suite refresh
is now complete, with all implementation builds/proofs/reference jobs paused.
It uses the same frozen source and binary hashes as the preceding concurrent
run, all 5,638 file identities, unchanged assertions, three-second deadline and
two workers. Results: 423 passed, 2,091 failed, 3,111 unsupported, 10 timeouts,
three incomplete; 19,241 passed records including failing-file prefixes. All
three transiently lost full passes are restored, and no full-file pass or
record prefix is lost against either fifth-checkpoint run 5-a or 5-b. No earlier
checkpoint-three full pass is lost. Its sole shorter checkpoint-three prefix
remains the constant-column timeout already investigated with paired sources
above; neither that timeout nor the 512 MiB writer limit is waived.

The concurrent failures remain retained as environment-sensitive deadline
outcomes, not deleted or reclassified as passes. This whole-suite controlled
run, the 34/34 faster-reference campaign, workspace/check/clippy and exploratory
Kani checkpoint complete the regression investigation for this follow-up.
README still matches `origin/main`, and the user's pre-sync stash is unchanged.
The queued floating formatter, Base64, temporal aliases/strict VARIANT casts,
sequence aliases and native VARIANT reader are not part of this frozen source;
they require continuous integration and their own combined validation. The
large value-and-expression milestone remains active and incomplete.

## Sixth integrated implementation checkpoint, 2026-09-10

Source `748dfba` combines the selected floating VARCHAR formatter, Base64,
nanosecond/TIMETZ functions and temporal aliases, strict VARIANT clock casts,
sequence-concat aliases, native VARIANT read-side decoding and the lead's
qualified nested-path binder repair. The new defaulted cast source context
preserves ordinary selected overrides and lets retained VARIANT child casts
request the development-specific strict clock policy without ambient lookups.
The SQLLogicTest worker also now applies the source-backed empty/NUL rendering
rules to rendered values rather than only VARCHAR payloads; its initial Base64
8/17 failure and repaired 17/17 file are retained separately.

The [qualified-path report](qualified-nested-values.md) records mixed decimal,
nanosecond, LIST and STRUCT paths across scalar/batch evaluation, parameters,
joins, grouping, sorting, windows, indexed-table mutations, rollback and native
reopen. The native VARIANT follow-up
passes 49/49 against development semantics across three independent producer
fixtures; all three earlier direct qualified-subscript join failures are fixed.
Native VARIANT publication/WAL remain unsupported. The qualification campaign
retains same-SELECT-list alias reuse and nested-shell-JSON representation gaps,
not waived or normalized away.

Ordinary check, all-target clippy, nested 28, contracts 27, grouping nine and
subqueries 15 pass. Before the binder increment, the combined scalar/temporal
suite also passed numeric 28, temporal 25, casts 12, floating seven and binary
eight. The full workspace suite now passes, with the same two external-CLI
analytics tests ignored. The recovery tail-truncation suite completed in
63.77 seconds. Instrumentation coverage reports 297 files, 2,688 functions and
209 interface methods with no missing entries. The traced all-target check
completed in 66.54 seconds with zero errors, panics or open spans; temporary
telemetry was deleted.

`python3 scripts/verify_kani.py` passes all six maintained harnesses under Kani
0.67.0, with zero failures: TIMETZ 51.321 s, unsigned keys 0.799 s, dense offsets
0.489 s, ROWS 2.284 s, uniform bounds 2.360 s, packed byte counts 103.182 s.
Caller-location (one) and foreign-function (four) unsupported constructs remain
unreachable in these proofs; atomics remain sequential and the vendor parser's
unused-variable warning persists. No proof of general native VARIANT decoding,
qualified SQL binding, strict clock parsing or concurrent durability is claimed.
New worker DATE/numeric/Object increments are not part of this source.

No sixth-checkpoint performance or full upstream acceptance campaign has run.
The fifth checkpoint's passing 34-workload campaign does not cover these new
changes. The last remote PR checkpoint remains `135eece`; subsequent local
commits require regression validation before another follow-up push. Native
file-version/successor capability preservation and retained closed default
expressions through selected statement services are the next shared storage
integration work. Family implementations continue alongside that work.

### Native publication follow-up

Source `12586bf` repairs the confirmed file-version/identifier/generation reset
in ordinary checkpoint commits. A selected format-owned encoder retains only
publication metadata; the file layer prepares and binds the next image before
I/O and advances state only after durable success. It neither rereads nor caches
all preceding table bytes per commit. Definite failures keep the prior state;
uncertain publication blocks writes until reopen.

The [publication report](native-publication-values.md) retains the initial six
failing metadata cases and repaired 6/6 production campaign across independently
produced storage 64, 65, 68 and 69 files. Both pinned producers read the expected
mutated values; versions/identifiers survive and generations advance. This does
not enable TUPLE/VARIANT layouts, change the fresh-file default version, or
establish native performance parity. Focused replacement-format, native,
checkpoint, recovery and typed SQL tests pass; instrumentation and the next
combined Kani/upstream/performance checkpoints remain tracked separately.
