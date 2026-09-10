# Temporal values: first integrated engine path

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
offsets outside ±15:59:59. Value remains 32 bytes and DataType at most 16 bytes.
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

The repaired campaign fixes a real standalone-clock parsing mismatch: offsets
after HH:MM are rejected, whereas HH:MM:SS offsets are valid. Timestamp suffix
parsing has a different rule. It also fixes the public TIMESTAMPTZ_NS type name
and development's last-representation behavior for equal INTERVAL min/max
inputs. Native release cases exercise eight temporal columns, nulls, defaults,
an indexed key, Rust checkpoint and WAL producers, a C++ producer, rollback,
subsequent mutation, and reopening from both readers.

The two remaining release SQL differences are explicitly development-authoritative:
release lacks TIMESTAMPTZ_NS, and its interval extrema preserve the first equal
representation while development preserves the last. Neither is a Rust failure
against development. Initial failures remain in their original report.

The development native cases still expose the separately owned shared file
compatibility gaps: version 999 input and C++ catalog reconstruction of the
legacy writer output. No temporal codec success is counted as solving these.
The integration lead is handling version negotiation and catalog ordinals; this
report must be refreshed after those changes land.

## Checkpoint validation

Routine checks pass: `cargo check`; six temporal component tests plus seven DATE
and eight operator tests; and workspace/all-target clippy with warnings denied.
The relational test crosses two expression evaluators, two join implementations,
and batch sizes 1, 7, and 2048. Persistence crosses JSON/native formats and
hash/B-tree indexes; native WAL recovery is a separate test.

Coverage currently reports 219 Rust files, 1911 functions, 195 interface methods,
and no missing instrumentation. The final native/arithmetic checkpoint ran
`python3 scripts/verify_kani.py` with pinned Kani 0.67.0: six of six maintained
harnesses passed, none failed. The new adjacent TIMETZ harness checks packing
round-trip and equality-key injectivity across arbitrary valid local times and
offsets; it passed in 19.909 seconds. Its assumptions are precisely the physical
time/offset validity ranges, not any desired equality result. The other five
harnesses passed in 0.690, 0.527, 2.533, 2.408, and 88.683 seconds. An earlier
five-harness checkpoint also passed before the temporal proof was added.

Kani's reported atomics are modeled sequentially; unsupported foreign calls and
caller-location constructs must remain unreachable in a successful harness.
No proof here establishes SQL completeness, concurrency, durability, native
file interoperability, or performance parity.

## Remaining work in the same assignment

This is not a stopping boundary. Temporal scalar functions, EXTRACT and interval
unit syntax, remaining DATE/text semantics, complete conversion/overload/error
matrices, timestamp precision extrema, and broader indexed-key/native codec
cases remain open. Existing arithmetic covers the core DATE/TIME/TIMESTAMP and
INTERVAL overloads; it does not assert complete function-catalog coverage.

Named IANA timezone semantics require an explicitly selected timezone adapter
and settings/context behavior, not host timezone inference. Both pinned core
reference builds currently lack ICU; they reject named-zone timestamp input.
Core numeric offsets are implemented, but ICU behavior, DST gaps/ambiguities,
calendars, and session-relative conversion remain unverified. Optional IANA
experimentation must not silently widen the default core reference behavior.

INTERVAL index keys are rejected by C++ even though joins/grouping can use them.
The native ART writer already rejects them defensively; lead-owned binding
capability validation is still being integrated. Type-by-type negative index
tests must accompany that shared change.

No temporal performance gate has been run. Existing 34-workload performance
baselines must be preserved and remeasured during a coordinated quiet period;
new temporal workloads need both pinned references and the faster-reference
acceptance rule. Unmeasured behavior does not inherit the prior passing gate.
