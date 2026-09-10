# Temporal values and expression paths

This is a working increment of the value-and-expression assignment, not full
temporal or engine parity. The representation and algorithms remain provisional.

The family now carries TIME, TIME_NS, TIMETZ, TIMESTAMP_S, TIMESTAMP_MS,
TIMESTAMP, TIMESTAMP_NS, TIMESTAMPTZ, TIMESTAMPTZ_NS, and INTERVAL from SQL typed
literals and casts through ordinary vectors, selected/batched expressions,
comparisons and canonical keys, joins, grouping, extrema, sorting, and window
aggregates. Prepared typed values, mixed temporal/decimal schemas, defaults,
index-backed timestamp constraints, updates, deletes, rollback, native WAL
recovery, checkpoints, and reopen are exercised together.

`TemporalValue` retains logical units and interval components. TIME is measured
from midnight; timestamp units are measured from the Unix epoch. Infinite
timestamps remain distinct from the reserved signed-minimum NULL slot. Physical
value validation rejects invalid clock ranges, reserved timestamp slots, and
stored TIMETZ offsets outside ±15:59:59. Timestamp numeric offsets require two
digits per supplied field but do not enforce per-field civil ranges: development
accepts +99:99:99 and normalizes the total. Value remains 32 bytes and DataType at
most 16 bytes.
No host clock, timezone, or ambient process setting participates in these paths.

Physical clocks and accepted text have separate provisional domains. Selected
SQL constructors/casts are closed over zero through 24 hours plus 500,000
microseconds for TIME/TIMETZ, and the equivalent nanosecond interval for TIME_NS.
`make_time(23,59,60.5)` reaches that upper boundary: whole-second conversion uses
ties-even rounding, and the remaining fraction is rounded separately. Direct
TIME/TIMETZ text still stops at 24 hours; TIME_NS text permits only the additional
999 nanoseconds retained after its microsecond range check. Casting those final
nanoseconds to TIME can carry a microsecond past midnight, and casting back to
TIME_NS preserves it. Arbitrary native/API raw clocks outside this measured
SQL-closed domain remain unsupported; storage-word width alone is not validation.

Intervals retain signed months, days, and microseconds for calendar arithmetic,
formatting, and persistence. Equality/order keys normalize a month to thirty
days and a day to twenty-four hours. Calendar addition instead clips the day to
the destination month before adding day and clock components. TIMETZ equality
retains local time and offset identity; ordering uses adjusted UTC time with
the reference's offset tie-breaker. Equal instants with different offsets are
not interchangeable TIMETZ values.

The temporal type, cast, and operator implementations use the existing selected
adapter contracts. Scalar and batched engine paths consume the same bound
semantics. Existing vector representation experiments remain available; this
increment does not prescribe specialized temporal vector layouts.

Calendar extraction, EXTRACT, constant/dynamic date_part, epoch unit functions,
date/time/timestamp constructors, interval constructors and unit syntax,
calendar names/last_day, and finite/infinite predicates now use selected scalar
binding. TIMESTAMP(p) resolves to the pinned development precision. Explicit
timestamp reductions round nearest with ties away from the epoch; narrow
timestamp text first parses at microsecond precision. These rules differ from
release truncation. TIME_NS-to-TIME also rounds, while TIME_NS epoch/extraction
functions truncate sub-microsecond values according to their own core overloads.

`nanosecond` retains exact TIME_NS and TIMESTAMP_NS/TIMESTAMPTZ_NS overloads;
ordinary clock and interval overloads preserve local components and signed
interval remainders. Untyped NULL is ambiguous, typed NULL and infinite dates/
timestamps propagate NULL, and the earliest physical nanosecond timestamp day
retains development's fatal INTERNAL error. This does not add a `nanosecond`
date_part specifier or a plural function alias. `timetz_byte_comparable` returns
the core's biased UBIGINT key, preserving the same offset tie-breaker as TIMETZ
comparison. Prepared boundary tests compare all pairs in a 25-value clock/offset
grid, including constructor-produced values past midnight.

TIMESTAMP_US aliases TIMESTAMP; DATETIME accepts precisions zero through nine.
DATETIME(10) rejects even though development's separate TIMESTAMP syntax accepts
precision ten. Native default metadata recognizes TIMESTAMP_US and exactly one
integral literal precision for the timestamp type constructor. It does not
evaluate stored expressions or broaden unknown/named/other parameterized types.

Constant date_part normally returns BIGINT (epoch returns DOUBLE); dynamic
specifier expressions return DOUBLE. Integer interval epoch units normalize
months to thirty days with checked intermediate additions, whereas floating
epoch uses 365.25-day years. This distinction is intentional reference behavior.
INTERVAL n UNIT lowers through selected DOUBLE casting, truncation when required,
the unit's declared integer width, and its selected constructor. Tests replace
date_part/trunc adapters to verify the syntax does not bypass registration.

Checked clock/timestamp scanning shares the calendar-prefix parser while keeping
ordinary DATE casts fully consuming. It handles one-/two-digit clock fields,
optional final fields, exact fractional units, numeric offsets, timestamp-to-time
fallback, and development's different standalone/timestamp suffix rules. Long
calendar, whitespace, fractional and zone-name scans check cancellation without
unbounded temporary allocations. Standalone TIME ignores a non-strict final
suffix; TIMETZ parses an offset, not Z. Named timestamp suffixes are ignored for
naive values but only UTC is interpreted for zoned values without ICU. No named
zone lookup or host-timezone fallback occurs. Development's date-only timestamp
text with trailing whitespace is rejected, even though ordinary DATE accepts it.

## Evidence and regressions

`scripts/temporal_reference.py` verifies both pinned source/binary identities and
preserves typed results, errors, source and executable hashes, and native-file
outcomes. It uses isolated worktree-local debug workers: these are correctness
measurements, not performance evidence. Error-presence comparisons deliberately
do not establish diagnostic-category parity.

| Campaign | Development SQL | Release SQL | Development native | Release native |
| --- | --- | --- | --- | --- |
| [Initial](temporal-core-reference-initial.json) | 15/18 | 16/18 | 0/3 | 3/3 |
| [Repaired](temporal-core-reference-repaired.json) | 18/18 | 16/18 | 0/3 | 3/3 |
| [Functions initial](temporal-functions-reference-initial.json) | 40/45 | 37/45 | 0/3 | 3/3 |
| [Functions/casts repair](temporal-functions-reference-repaired.json) | 162/167 | 152/167 | 2/3 | 3/3 |
| [Function/type matrix](temporal-functions-reference-matrix.json) | 432/433 | 408/433 | 2/3 | 3/3 |
| [Integrated functions](temporal-functions-reference-integrated.json) | 437/437 | 412/437 | 3/3 | 3/3 |
| [Interval text initial](temporal-text-reference-initial.json) | 443/448 | 418/448 | 3/3 | 3/3 |
| [Interval text repair](temporal-text-reference-repaired.json) | 448/448 | 422/448 | 3/3 | 3/3 |
| [Interval units/diagnostics](temporal-diagnostics-reference.json) | 455/455 | 429/455 | 3/3 | 3/3 |
| [Clock text initial](temporal-clock-reference-initial.json) | 548/662 | 532/662 | 3/3 | 3/3 |
| [Clock text repair](temporal-clock-reference-repaired.json) | 661/662 | 598/662 | 3/3 | 3/3 |
| [Clock domain investigation](temporal-clock-domain-reference.json) | 666/667 | 601/667 | 0/3 expanded | 0/3 expanded |
| [Clock domain integrated](temporal-clock-domain-integrated.json) | 667/667 | 602/667 | 2/3 expanded | 1/3 expanded |
| [Calendar boundary/nested WAL integration](temporal-calendar-boundary-integrated.json) | 688/688 | 623/688 | 3/3 expanded | 2/3 expanded |
| [Temporal renderability](temporal-renderability-integrated.json) | 696/696 | 631/696 | 3/3 expanded | 2/3 expanded |
| [Nanosecond lower-bound initial](temporal-nanosecond-lower-bound-initial.json) | 702/709 | 635/709 | 3/3 expanded | 2/3 expanded |
| [Nanosecond lower-bound repair](temporal-nanosecond-lower-bound-repaired.json) | 709/709 | 642/709 | 3/3 expanded | 2/3 expanded |
| [Functions/keys/aliases initial](temporal-function-keys-reference-initial.json) | 749/750 | 679/750 | 0/3 expanded | 2/3 expanded |
| [Functions/keys/aliases repair](temporal-function-keys-reference-repaired.json) | 750/750 | 680/750 | 3/3 expanded | 2/3 expanded |

The repaired campaign fixes a real standalone-clock parsing mismatch: offsets
after HH:MM are rejected, whereas HH:MM:SS offsets are valid. Timestamp suffix
parsing has a different rule. It also fixes the public TIMESTAMPTZ_NS type name
and development's last-representation behavior for equal INTERVAL min/max
inputs. Native release cases exercise eight temporal columns, nulls, defaults,
an indexed key, Rust checkpoint and WAL producers, a C++ producer, rollback,
subsequent mutation, and reopening from both readers.

The integrated campaign adds an 11-by-11 explicit cast matrix (ten temporal
types plus DATE), a 24-function-by-11-type matrix, unit syntax, precision
boundaries, dynamic specifiers, invalid signatures and constructors. Floating
results allow only exact numeric equivalence of textual formatting (0 versus
0.0), never a numerical tolerance. Both raw results remain in each report.

It repairs five early parser/cast defects, then the development-only timestamp
rounding differences exposed by fractional matrix inputs. The final function
defect was accidental exposure of epoch(TIMESTAMPTZ), which needs unavailable
ICU in the pinned core. The 25 remaining release differences are development-
authoritative: precision rounding/modifiers, newer cast pairs, TIMESTAMPTZ_NS,
dynamic date_part, and equal-interval extrema representation. They are not
silently discarded. All prior reports remain unchanged.

Lead integration repaired version 999 input, catalog ordinals, segment metadata,
and development UNBOUND parsed-type metadata in defaults. The final native
campaign passes all three producers against both readers, including Rust
rollback/update/publication of the C++-produced file. The intermediate functions
trial's zero native count was a harness configuration error: the development
CLI was incorrectly asked to use the unavailable JSON extension. Its next trial
corrected the pinned target flag and exposed the real UNBOUND input gap before
the lead fixed it. These native reference schemas contain eight common temporal
types; the Rust mixed-schema tests additionally exercise TIME_NS/TIMESTAMPTZ_NS.

The interval scanner now preserves input-order component overflow checks,
attached aliases, optional leading @, final AGO, unitless seconds, and the
core's non-strict final clock suffix behavior. Fractional years round to months;
fractional quarters, months and weeks follow their distinct component rules.
Fractional MICROSECONDS are discarded while fractional seconds round to whole
microseconds in development; release differs. Arbitrarily long fractional input
uses a bounded decimal prefix and sticky tail for binary64 conversion. Unit
tests compare a rounding midpoint and nonzero tail to ordinary binary64 parsing.
Cancellation is checked during interval whitespace, integer/fraction/clock and
repeated-unit scans; deterministic cancellation tests cover each shape.

The unchanged retained development interval files were also executed with
`scripts/run_upstream.py`. The [first upstream result](upstream-temporal-interval-text.json)
is explicitly incomplete: two of ten files pass (fractional parsing and interval
operators), seven fail and one is unsupported. First failures identify exact
diagnostics, plural unit syntax, INTERVAL type qualifiers/alias(), scalar range(),
and a boolean expectation normalization issue. Selected differential passes do
not erase those broader obligations. Their original results/journal are retained.

The [interval diagnostic follow-up](upstream-temporal-interval-diagnostics.json)
advances to three passing and seven failing files, with none unsupported. Plural
unit syntax uses the same retained cast/truncation/function path as singular
syntax, including prepared mutations under both evaluators. The colon-overflow
file now passes all eight records. The constants file advances from zero to 20
records before a shared numeric-cast diagnostic mismatch; the TRY_CAST file
advances from zero to 18 of 19 records before an exact Invalid Input category
mismatch. General parsing, unrecognized/unsupported specifiers and AGO/checked
addition messages are repaired, but native cast categories and TRY_CAST failure
classification still require a shared contract change. Boolean harness
normalization, missing UNNEST/INTERVAL type qualifiers and general overload
diagnostics remain explicit failures. Assertions and prior evidence are unchanged.

Clock-domain native cases expand the earlier flat schema with TIME_NS, cast-
produced clocks past midnight, nested STRUCT/LIST children, index uniqueness,
calendar rollover and subsequent rollback/update/reopen. Their initial zero
counts exposed a nested scalar-quoting gap, native nested WAL dispatch and the
release's unavailable TIME-to-TIME_NS cast, not regressions in the earlier flat
native schema. The combined VARIANT formatter repairs quoting; both development
C++ and Rust checkpoint producers now pass the expanded file workflow. Rust's
native nested WAL producer still fails, and release cannot create its extended
producer schema with the selected cast. Both pins read the Rust-produced boundary
checkpoint correctly. Failed trials and their exact messages remain preserved.

The same cross-family assertion matrix is executable with
`cargo run --example temporal_nested_wal_obligation`. At the clock-domain
checkpoint it exited with the native STRUCT WAL fixed-width dispatch error;
that failure was neither ignored nor counted as a pass. After the nested WAL
integration, the unchanged executable now succeeds and its full assertion matrix
also runs as an ordinary temporal test, alongside the retained checkpoint modes.
The latest independent campaign passes all three development-native producers
and both Rust producers against release readers, including subsequent rollback,
update and reopen. Release still cannot produce the expanded TIME-to-TIME_NS
schema. Native min/max calendar timestamp rows and no-op calendar arithmetic
are included. This closes the recorded mixed-WAL witness, not every native-file
or arbitrary raw-value obligation.

The [unchanged TIME/TIMESTAMP upstream run](upstream-temporal-clock-text.json)
passes nine of 26 files, fails eleven and reports six unsupported (five depend on
ICU, one on a missing TIMESTAMP_US alias). Passing files include time/timestamp
2411 cases, time limits, TIME/TIMESTAMP TRY_CAST, alternative timestamp casts,
BC timestamps, millisecond timestamps and core TIMESTAMPTZ. Failures retain
exact diagnostic differences, infinity abbreviations, missing nanosecond and
timetz_byte_comparable functions, timestamp avg, and boolean harness normalization.
This broader run is not full upstream parity despite the selected 667-case pass.

The [cast-provenance interval follow-up](upstream-temporal-diagnostic-cast-interval.json)
passes four files and retains six failures. All nineteen interval TRY_CAST records
now pass: the selected temporal adapter marks its own conversion/narrowing/range
failures as invalid input while preserving public Conversion, Invalid Input and
Out of Range categories. Source/output validation and inner-expression failures
stay fatal; nested TRY_CAST tests retain partial NULL children without suppressing
an inner strict cast. The constants diagnostic still precedes the separately
delivered shared floating-cast repair in this report.

The [text diagnostic follow-up](upstream-temporal-diagnostic-cast-time.json)
passes eleven files, fails nine, and retains six unsupported. TIME range wording,
TIMESTAMP format/calendar-range distinctions, numeric-offset diagnostics and
precision-specific INT64 conversion errors now match selected source behavior.
Exact `inf`/`-inf` abbreviations are accepted only at actual end of input; full
`infinity` still allows trailing whitespace. Diagnostics copy a bounded prefix
of malformed input, and checked scanner cancellation remains intact. General
TIME and incorrect-TIMESTAMP files now pass. The broader run advances to the
VARIANT-specific strict TIME parser gap and, importantly, reveals a real calendar
intermediate-overflow bug in timestamp arithmetic. That witnessed failure is
retained for repair, not classified as an expected diagnostic-only difference.

The calendar reconstruction repair checks the day-times-microseconds product,
then the clock and only then a zoned offset. A mathematically representable final
sum cannot repair an earlier overflow. Interval arithmetic retains checked day
components, normalizes the clock carry, and checks the reconstructed day start;
DATE and raw timestamp inputs also take their source calendar-conversion path.
The [targeted unchanged boundary follow-up](upstream-temporal-calendar-boundary.json)
advances from four to eighteen of twenty-three records. All reached boundary
rejections now pass; the next failure is integer-typed SQLLogicTest normalization
of epoch's DOUBLE output. Both independent C++ CLIs render the same fractional
DOUBLE as Rust, but the unchanged harness expectation is retained as a failure.
The later age function remains absent. Native WAL/index mutation tests verify
that rejected minimum/maximum updates leave state unchanged, positive adjacent
updates persist, rollback works, and checkpoint/reopen retains both extrema.

Physical timestamp validity is distinct from calendar text renderability. Huge
TIMESTAMP_S/MS payloads and a partial earliest microsecond/nanosecond day can be
valid stored instants while the C++ formatter rejects them. Temporal diagnostic
Display is now infallible, including invalid public offset extrema, and uses a
typed raw fallback when its calendar date cannot be represented. Selected SQL
VARCHAR casts check renderability first, including nested children; pinned
development surfaces these underlying string-cast failures as INTERNAL due to
its non-fallible operator metadata. TRY_CAST does not suppress them. Release's
fatal Conversion category remains an explicit diagnostic disagreement.

CLI plain/JSON output and the verification transport check the complete result
before emitting text, so direct SELECT cannot leak diagnostic fallback as a
successful C++ text-result match. Typed API results, keys, native WAL, rollback
and reopen still preserve the raw values. The initial new nested-VARCHAR test
exposed its direct Display bypass; the single selected nested-cast guard repaired
it without changing retained child conversions. The recursive renderability
walk is bounded and cooperative. Each of the eight independent render-error
witnesses uses fresh Rust/C++ databases, preventing an earlier C++ INTERNAL
error from poisoning later evidence. Those rows verify retained category/message
behavior explicitly; the full matrix's comparator still only asserts rejection
presence for errors and is not an exhaustive diagnostic-equivalence claim.

The lower-nanosecond text trial exposes seven additional mismatches. Parsing
`1677-09-21 00:12:43.145224194` into TIMESTAMP_NS must reject the overflowing
microseconds-times-1000 intermediate even though adding the positive nanosecond
tail would produce a representable integer. The repair preserves that source
operation order for TIMESTAMP_NS and TIMESTAMPTZ_NS. Prepared strict/TRY casts,
adjacent accepted boundaries, invalid-update atomicity, rollback, native WAL,
indexed lookup and checkpoint/reopen now exercise the rule. Raw epoch constructors
still retain the same physically valid instants, and TIME_NS timestamp-text
fallback retains its independently supported clock parsing behavior. The initial
trial remains unchanged; the repaired development matrix passes all 709 cases.

At the renderability checkpoint, string `concat` still bypassed selected VARCHAR
casts and could render a physically valid but SQL-unrenderable timestamp. The
combined scalar/nested [concat repair](concat-cast-regression.md) now uses retained
string casts and the distinct LIST-returning overload. No temporal formatting
exception is embedded in concat.

The function/key trial catches the DATETIME(10) distinction and missing native
TIMESTAMP_US default metadata. Its other two development-native failures expose
a transport difference: the C++ shell emits large unsigned keys as JSON strings
while Rust emits exact integers; the release shell's outer JSON encoding can
round numeric values. The repaired native projection casts keys to VARCHAR in
both engines for exact comparison, while typed SQL and API tests still require
UBIGINT. Neither a float tolerance nor a lossy numeric normalization is used.
Expanded default schemas include TIMESTAMP_US and DATETIME(9) declarations and
literals, and both readers repeat the functions after rollback/update/reopen.
The failed initial report remains unchanged.

The [unchanged function/key upstream refresh](upstream-temporal-function-keys.json)
passes eleven of 26 TIME/TIMESTAMP files, fails ten and leaves five ICU-dependent
files unsupported. No previously passing file is lost. The formerly unsupported
TIMESTAMP_US file now passes nine records before a separate `FROM VALUES` parser
gap. The TIME_NS nanosecond query now executes, but its integer-declared
SQLLogicTest column preserves a `0.0` versus `0` expectation mismatch; TIMETZ
collation similarly retains boolean spelling normalization. VARIANT strict TIME,
timestamp avg, NULL-precision diagnostics, epoch DOUBLE normalization and later
catalog functions remain explicit obligations. No upstream assertion is changed.

## Checkpoint validation

Routine checks pass: `cargo check`; ten temporal component tests plus DATE,
numeric, nested and binary-family tests; and workspace/all-target clippy with
warnings denied. The full workspace suite passed before the latest combined
merge, with two preexisting external-CLI tests ignored; postmerge temporal,
nested and binary suites also passed.
The relational test crosses two expression evaluators, two join implementations,
and batch sizes 1, 7, and 2048. Persistence crosses JSON/native formats and
hash/B-tree indexes; native WAL recovery is a separate test.

The new combined workload places rounded timestamps and exact decimals inside
structs and timestamp lists, then uses prepared interval parameters, computed
joins/groups/windows, timestamp uniqueness, rollback, mutation and reopen.
It crosses both expression evaluators, both index choices, and JSON/native
checkpoint formats. This is a combined engine path, not only constructor tests.

The stable premerge function checkpoint reports 238 Rust files, 2088 functions,
201 interface methods and no missing instrumentation. Its all-target traced
check passed in 50.50 seconds and deleted temporary telemetry. An earlier trace
overlapped a merge and failed on inconsistent Enum compilation; it was not
counted as passing and was rerun on stable sources. The native/arithmetic checkpoint ran
`python3 scripts/verify_kani.py` with pinned Kani 0.67.0: six of six maintained
harnesses passed, none failed. The new adjacent TIMETZ harness checks packing
round-trip and equality-key injectivity across arbitrary valid local times and
offsets; it passed in 19.909 seconds. Its assumptions are precisely the physical
time/offset validity ranges, not any desired equality result. The other five
harnesses passed in 0.690, 0.527, 2.533, 2.408, and 88.683 seconds. An earlier
five-harness checkpoint also passed before the temporal proof was added.

After the combined native/scalar integration, coverage reports 246 files,
2130 functions and 201 interface methods with no missing instrumentation.
The production `cargo build --release --no-default-features` completed
successfully. Compilation time is not execution-performance evidence.

The final function/cast checkpoint again ran the full maintained Kani suite:
six of six passed, none failed. TIMETZ took 17.662 seconds; the remaining
harnesses took 0.833, 0.519, 2.186, 3.168 and 92.955 seconds. Timestamp precision
rounding has ordinary half-boundary/extrema/infinity tests, not a separate
rounding proof. The prior intermediate six-harness run also passed.

The interval-text checkpoint passes eleven temporal component tests, seven DATE
tests and two adjacent scanner tests, ordinary check/clippy, coverage (247 files,
2149 functions, 201 interfaces, no missing instrumentation), and all-target trace
compilation with temporary telemetry deleted. Its full Kani run again passes
six of six maintained harnesses (19.987, 0.814, 0.521, 2.479, 3.304 and 78.827
seconds). Scanner cancellation/rounding and full parser behavior are not formal
proof claims. The upstream worker built successfully in release mode, but this
checkpoint has no new execution-performance gate.

The small plural-unit/diagnostic follow-up passes twelve temporal tests, ordinary
check/clippy, instrumentation coverage (255 files, 2228 functions, 205 interfaces,
no missing entries), and all-target trace compilation (53.38 seconds, temporary
telemetry deleted). It does not add a separate formal-proof claim; the preceding
interval-text checkpoint was its latest full Kani run.

The clock/domain stage passes fourteen ordinary temporal tests, seven DATE tests,
four adjacent temporal tests and the DATE prefix boundary/cancellation test;
post-integration nested fourteen and BIT three tests also pass. Check/clippy pass.
The full Kani 0.67.0 suite passes six of six harnesses, zero failures, with the
TIMETZ physical-domain assumptions expanded to include constructor/cast carry.
Times were 48.202, 0.753, 0.473, 2.194, 3.625 and 78.710 seconds. This verifies
the existing packing/equality claims over the expanded bound, not arbitrary raw
native payloads, parser correctness or SQL closure as a formal theorem.
After the literal-provenance integration, coverage reports 268 files, 2355
functions and 206 interfaces, no missing instrumentation; all-target trace
compilation passes in 32.11 seconds and deletes temporary telemetry. The prior
clock-stage trace also passed in 50.95 seconds. Production build with
`cargo build --release --no-default-features` passes (1 minute 34 seconds).
No new execution-performance gate is claimed.

The cast-provenance/text-diagnostic checkpoint passes sixteen temporal tests,
seven DATE tests, eleven cast tests, the two adjacent DATE tests and four adjacent
temporal tests. Ordinary check and workspace/all-target clippy pass. Instrumentation
coverage reports 273 files, 2414 functions, 207 interfaces and no missing entries.
The all-target traced check passes in 66.35 seconds and deletes temporary telemetry.
The full maintained Kani 0.67.0 suite again passes six of six harnesses, none failed
(49.647, 0.825, 0.503, 2.577, 3.163 and 103.760 seconds). This is the maintained
packing/key/index/window/byte-count suite, not a proof of parsing or TRY_CAST
completeness. The compiler reports caller-location and foreign-call constructs
that remain unreachable in successful harnesses; atomics are sequentially modeled.

The calendar-boundary checkpoint passes seventeen temporal and seven DATE tests,
ordinary check and workspace/all-target clippy. Coverage reports 273 files, 2417
functions, 207 interfaces and no missing entries; all-target trace compilation
passes in 30.16 seconds and deletes telemetry. Full Kani again passes six of six
maintained harnesses (56.627, 0.817, 0.508, 2.270, 2.390 and 104.033 seconds),
with no failures. The new calendar overflow contract has executable SQL/native
regression coverage, not a new formal arithmetic proof.
The premerge full workspace test suite also passes, with only two preexisting
external-CLI analytics tests ignored. After the combined native WAL/floating
integration, temporal seventeen, DATE seven, casts eleven and nested nineteen
tests pass, as do ordinary check/clippy and the standalone mixed-WAL executable.
Its promotion adds an eighteenth ordinary temporal test without changing the
matrix's assertions. The latest 688-case differential run is correctness
evidence; performance measurements remain the lead's isolated combined gate.

The renderability stage passes twenty temporal, eleven cast and nineteen nested
tests, its adjacent cancellation/raw-validity test, check and workspace/all-target
clippy. Coverage reports 276 files, 2445 functions and 207 interfaces, with no
missing instrumentation; all-target trace compilation passes in 36.65 seconds
and deletes telemetry. Full Kani passes six of six maintained harnesses, none
failed (50.659, 1.121, 0.710, 3.287, 3.381 and 102.139 seconds). These existing
harnesses do not prove calendar rendering or error propagation. The new API,
cast, transport and native regression tests supply the relevant executable checks.

The lower-nanosecond stage passes twenty-one temporal, seven DATE and eleven
cast tests, ordinary check and workspace/all-target clippy. Coverage reports
281 files, 2500 functions and 207 interfaces with no missing instrumentation;
all-target trace compilation passes in 76.81 seconds and deletes telemetry.
Full `python3 scripts/verify_kani.py` again passes six of six maintained harnesses,
none failed (52.151, 1.268, 0.838, 2.545, 2.994 and 96.191 seconds). The nanosecond
parser boundary has executable regression coverage, not a new formal proof.
No performance claim follows from this worker-local correctness campaign.

The function/key/alias stage passes twenty-three temporal, eleven cast and seven
DATE tests, its adjacent native-metadata/truncation test, check and workspace/
all-target clippy. The native test rejects unknown and other parameterized types,
non-literal expressions, NULL/invalid precision and every truncated prefix of a
valid object. An initial unit-fixture encoding omitted a nested logical-type
field and failed; the corrected fixture and decoder pass without general stored-
expression evaluation. Coverage reports 284 files, 2565 functions and 208
interfaces with no missing entries; traced all-target check passes in 33.93
seconds and deletes telemetry. The upstream worker builds in release mode.
Final-source Kani 0.67.0 passes all six maintained harnesses, none failed
(51.677, 0.885, 0.529, 2.341, 2.835 and 75.134 seconds). The earlier pre-repair
function-source run also passed six of six. These proofs retain their existing
packing/key/index/window/byte-count scope; neither metadata parsing nor the
new function catalog is proven complete. Performance remains a separate,
lead-coordinated combined measurement.
The final `cargo test --workspace` run also passes, including all native recovery
tail boundaries, with only the two preexisting external-CLI analytics tests
ignored. No test is newly ignored or weakened for this increment.

Kani's reported atomics are modeled sequentially; unsupported foreign calls and
caller-location constructs must remain unreachable in a successful harness.
No proof here establishes SQL completeness, concurrency, durability, native
file interoperability, or performance parity.

## Remaining work in the same assignment

This is not a stopping boundary. The remaining function catalog includes
date_trunc/date_diff/date_sub/time_bucket, formatting/parsing functions, richer
date_part specifiers and struct results, current-time functions and broader
calendar arithmetic. Remaining DATE/clock/timestamp text semantics, parser error
diagnostics and type aliases, timestamp
physical/display extrema, complete
conversion/overload/error matrices, and broader indexed-key/native codec cases
remain open. Existing selected matrices do not assert catalog completeness.

Named IANA timezone semantics require an explicitly selected timezone adapter
and settings/context behavior, not host timezone inference. Both pinned core
reference builds currently lack ICU; zoned input rejects non-UTC names, while
naive timestamp input can ignore those names without interpreting them.
Core numeric offsets are implemented, but ICU behavior, DST gaps/ambiguities,
calendars, and session-relative conversion remain unverified. Optional IANA
experimentation must not silently widen the default core reference behavior.

INTERVAL index keys are rejected by C++ even though joins/grouping can use them.
The native ART writer rejects them defensively; lead-owned binding capability
validation and a negative test now reject PRIMARY KEY/UNIQUE before publication
without disabling the valid grouping/join paths.

No temporal performance gate has been run. Existing performance
baselines must be preserved and remeasured during a coordinated quiet period;
new temporal workloads need both pinned references and the faster-reference
acceptance rule. Unmeasured behavior does not inherit the prior passing gate.
