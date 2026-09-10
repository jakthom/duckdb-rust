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
stored TIMETZ offsets outside ±15:59:59. Timestamp text accepts numeric offsets
through ±23:59:59. Value remains 32 bytes and DataType at most 16 bytes.
No host clock, timezone, or ambient process setting participates in these paths.

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

Constant date_part normally returns BIGINT (epoch returns DOUBLE); dynamic
specifier expressions return DOUBLE. Integer interval epoch units normalize
months to thirty days with checked intermediate additions, whereas floating
epoch uses 365.25-day years. This distinction is intentional reference behavior.
INTERVAL n UNIT lowers through selected DOUBLE casting, truncation when required,
the unit's declared integer width, and its selected constructor. Tests replace
date_part/trunc adapters to verify the syntax does not bypass registration.

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

Kani's reported atomics are modeled sequentially; unsupported foreign calls and
caller-location constructs must remain unreachable in a successful harness.
No proof here establishes SQL completeness, concurrency, durability, native
file interoperability, or performance parity.

## Remaining work in the same assignment

This is not a stopping boundary. The remaining function catalog includes
date_trunc/date_diff/date_sub/time_bucket, formatting/parsing functions, richer
date_part specifiers and struct results, current-time functions and broader
calendar arithmetic. Remaining DATE/clock/timestamp text semantics, parser error
diagnostics and unit syntax, non-interval long-input cancellation, timestamp
physical/display extrema, complete
conversion/overload/error matrices, and broader indexed-key/native codec cases
remain open. Existing selected matrices do not assert catalog completeness.

Named IANA timezone semantics require an explicitly selected timezone adapter
and settings/context behavior, not host timezone inference. Both pinned core
reference builds currently lack ICU; they reject named-zone timestamp input.
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
