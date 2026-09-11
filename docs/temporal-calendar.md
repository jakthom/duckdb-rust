# Core calendar grids and timestamp minima

This is a connected increment of the value-and-expression assignment, not full
temporal, function-catalog, native-file or performance parity. Calendar source
is `dbeb541`; the required physical timestamp/native-validity repair is
`fc387e9`, checked with integrated base `6ad70df`. Development remains the
correctness authority. Neither reference binary was changed, and neither has
the ICU extension required to verify the zoned calendar overloads.

## Implemented paths

Selected temporal adapters now register `date_trunc`/`datetrunc` and
`time_bucket`. Shared date-part aliases have moved to one family-owned module;
the existing difference functions retain those same aliases. This slice adds
no shared binder or evaluator hook.

Truncation preserves DATE/TIMESTAMP-to-microsecond-TIMESTAMP and
INTERVAL-to-INTERVAL return types. Selected casts carry other supported timestamp
units into the chosen overload, including development nanosecond rounding.
Calendar year/month/quarter/week/ISO-year, fixed timestamp widths and signed
interval components keep separate algorithms. Closed DATE/TIMESTAMP specifiers
are required selected constants during binding, matching the reference
statistics callback; INTERVAL has no such callback. Actual executed provenance,
not closedness or equal values, controls runtime constant dispatch. Existing
selected NULL-template and Constant-NULL behavior retain validation and lazy
child demand.

Bucketing separates positive fixed durations from positive pure-month widths,
with the source's intermediate overflow checks and rejection of mixed widths.
Default origins are Monday 2000-01-03 for fixed durations and 2000-01-01 for
calendar months. Interval offsets and temporal origins remain distinct overloads,
including different NULL/infinity/classification order. Development's TIME
overloads wrap across midnight; release does not supply these overloads.
None of these operations consult ambient host timezone state.

The four calendar tests exercise both evaluators, identity/pipeline optimization,
selected replacement ENUM casts and fatal failures, prepared parameters, nested
results, both index implementations, joins/groups/sorts/windows, duplicate and
failed-update atomicity, rollback, native WAL, checkpoint and reopen.

Source authority: [truncation](../../duckdb/extension/core_functions/scalar/date/date_trunc.cpp),
[bucketing](../../duckdb/extension/core_functions/scalar/date/time_bucket.cpp), and
[specifier aliases](../../duckdb/src/common/enums/date_part_specifier.cpp).
Development fixed-unit truncation uses unchecked lower-bound multiplication;
the rewrite explicitly models the measured modular result without Rust overflow
or undefined behavior. Release's checked error remains a recorded disagreement.

## Required physical-domain repair

Both pins produce `-9223372036854775808` from:

```sql
SELECT epoch_us(time_bucket(INTERVAL '4us',
                           make_timestamp(-9223372036854775806)));
```

The old rewrite rejected this valid result. Both pins also accept physical MIN
through `make_timestamp` and `make_timestamp_ns`; direct text-result failures
are rendering failures, not constructor rejection. The common
[timestamp base](../../duckdb/src/include/duckdb/common/types/timestamp_base.hpp)
stores every i64 payload for all precision/zone variants. Only positive and
negative INT64_MAX are infinities. MIN is finite and compares below negative
infinity under the reference's physical ordering.

The repair keeps all six timestamp payload domains complete and preserves MIN
through identical-unit scaling. Checked precision changes, unsupported cast
edges, calendar arithmetic and selected text-render failures stay separate.
TIME/TIME_NS/TIMETZ/DATE domains are unchanged. Infallible diagnostic Display
does not authorize a successful SQL VARCHAR or text-result fallback.

The native reader previously erased timestamp MIN before reading validity.
[Fixed-size storage](../../duckdb/src/storage/compression/fixed_size_uncompressed.cpp)
uses its NULL placeholder only for debugging; the
[validity column](../../duckdb/src/storage/table/standard_column_data.cpp)
determines NULL. Both primitive numeric-codec and fixed-width temporal dispatch
now preserve valid timestamp MIN. The existing writer already emits independent
validity, so no writer encoding or version bypass was necessary. Codec-specific
inline validity for other families is not changed.

Three additional connected tests cover all six units as owned values and typed
parameters; selected casts/comparisons, static nested and dynamic VARIANT values;
keys/joins/distinct/grouping/sorting/windows; both evaluators and index adapters;
failed mutations/rollback; native WAL/checkpoint/reopen. VARIANT publication uses
explicit version 69, rather than bypassing its version requirement.

The independent [C++ helper](../test/runner/temporal_minimum.cpp) constructs raw
Value/Appender inputs and reopens them using typed C++ getters. Six development
columns and five release columns each retain MIN, negative infinity, MIN+2,
epoch, typed NULL and duplicate MIN: 36/30 exact raw values. The six compressed
checkpoint/WAL fixture files are covered by ordinary Rust tests. The paired
driver also copies each independent checkpoint and WAL, mutates through Rust,
publishes Rust checkpoint or WAL, and inspects the result with the pinned C++
reader. All four paths pass per pin; NULL and MIN remain distinct.

## Retained comparisons and failures

The calendar and MIN drivers compare full result types/values or complete error
messages. `outcome_passed` separately records equal successful values or both
sides rejecting; it is not substituted for an exact pass. These reports return
nonzero while any exact mismatch remains.

| Calendar trial | Development exact / outcomes | Release exact / outcomes | Native paths per pin |
| --- | --- | --- | --- |
| [Initial](temporal-calendar-initial.json) | 340 / 459 of 827 | 318 / 495 of 827 | Not run |
| [Bucket implementation](temporal-calendar-bucket-first.json) | 726 / 827 of 827 | 638 / 742 of 827 | Not run |
| [First native campaign](temporal-calendar-native-first.json) | 726 / 827 of 827 | 638 / 742 of 827 | 3 / 3 |
| [Combined checkpoint](temporal-calendar-checkpoint.json) | 726 / 827 of 827 | 638 / 742 of 827 | 3 / 3 |

The [initial MIN campaign](temporal-minimum-initial.json),
[native MIN campaign](temporal-minimum-native-first.json) and
[final MIN checkpoint](temporal-minimum-checkpoint.json) retain development
23/32 exact and 32/32 outcomes, release 20/32 exact and 29/32 outcomes. The latter
two add the eight successful native publication/readback paths; the final
helper also checks every logical type before raw typed extraction. Reports retain
source, binary, script, library and fixture hashes plus the exact pinned
identities. Performance was not measured here.

The [unchanged broader temporal refresh](temporal-calendar-regressions.json)
retains all 1,288 development and 1,206 release SQL passes out of 1,289, with no
lost previously passing SQL compared with
[the provenance checkpoint](temporal-provenance-family-reference.json).
Its existing comparator treats paired rejection more coarsely than the new
drivers. Both Rust native producers still pass per pin. The preexisting raw
DEFAULT planning discrepancy and C++ function-default catalog decoding gap
remain separate integration obligations, not temporal codec fixes.

Initial implementation/test failures are retained rather than presented as
engine gains: the calendar fixture first requested only HashJoin for a computed
predicate and required its standard NestedLoop fallback; a prototype used a
nonexistent planner constructor. Two prior temporal assertions wrongly treated
MIN as invalid; independent source/API evidence justified replacing them with
explicit physical-validity and text-render failure assertions. New MIN tests
initially assumed diagnostic Display always prints raw ticks and that NS-to-TZNS
is a supported cast. Those assumptions were corrected without widening the cast
matrix. Running VARIANT WAL tests on the old base exposed missing retained
capabilities, then explicit version-64 rejection; the integrated version-69
path resolves the fixture setup without suppressing either rejection.

## Remaining boundaries

- Exact Binder/Unsupported prefixes, overload-candidate/location diagnostics,
  precision-overflow wording and INTERNAL stack-trace formatting still differ.
  Unsupported NS-to-TZNS fails at binding in Rust versus conversion in C++.
  The 101 development calendar exact mismatches are 74 Unsupported-prefix,
  19 Binder and eight optimizer-sensitive Conversion diagnostics.
- A lower calendar-boundary diagnostic depends on C++ optimizer naming. For
  `date_trunc('month', make_timestamp(-9223372036854775806))`, disabling the
  optimizer or supplying a VALUES column exposes the actual calendar-function
  error; ordinary constant planning can fail earlier in diagnostic rendering.
  No physical-domain restriction or premature render guard was added to mimic
  that earlier optimizer effect. Generic planning/default integration is lead-owned.
- Zoned/ICU overloads, the broader function catalog, exhaustive native codec
  coverage and performance acceptance are still open. These correctness runs
  did not share an acceptance timing window or change measured C++ binaries.
- Release differences in TIME bucketing, lower-bound fixed truncation, precision
  rounding and fatal cast category remain explicit. Development governs results;
  future performance acceptance still compares against whichever pin is faster.

## Checkpoint validation

`cargo check --workspace --all-targets`, all-target workspace clippy, focused
temporal 38 and compatibility 19 pass. Coverage reports 353 Rust files, 3,350
functions and 228 interface methods with no missing instrumentation. All-target
traced compilation passes in 71.87 seconds, with temporary telemetry deleted.

Full maintained `python3 scripts/verify_kani.py` passes six harnesses, zero
failures: 49.912, 0.825, 0.504, 2.302, 2.787 and 74.301 seconds. The existing
TIMETZ, key, aggregate-offset, window and packed-count proof scope is unchanged;
calendar algorithms and timestamp/native paths are not formally proved. Kani
models atomics sequentially; warned foreign/caller-location constructs must stay
unreachable in successful proofs. Full workspace/all-target tests also pass,
with only the two preexisting external-CLI analytics tests ignored.
`python3 -m unittest discover -s scripts -p 'test_*.py'` passes all 38 tests in
72.38 seconds. No test was newly ignored, and no acceptance benchmark was run.
