# Calendar period differences: bounded checkpoint

This adds usable `date_diff`/`datediff` and `date_sub`/`datesub` paths to the
ongoing value-and-expression assignment. It is not full function, temporal,
native-file, or performance parity. The shared metadata prerequisite is
`77e46c5`; family source is `f3806d4`, based on integrated `4b2a027`.

## Semantics and selected services

The pinned development source is authoritative. Its
[period-crossing implementation](../../duckdb/extension/core_functions/scalar/date/date_diff.cpp),
[complete-period implementation](../../duckdb/extension/core_functions/scalar/date/date_sub.cpp),
[specifier aliases](../../duckdb/src/common/enums/date_part_specifier.cpp), and
[function NULL binding](../../duckdb/src/function/function_binder.cpp) are the
reference for these provisional algorithms.

DATE, TIME and TIMESTAMP overloads retain BIGINT results. Other supported
timestamp units pass through selected microsecond conversions, including
development's rounding rules. Calendar differences retain BC years, ISO years,
end-of-month clipping, leap years, reversed arguments and time-of-day thresholds.
Weeks count seven-day distances rather than Monday crossings. Clock-unit
crossings floor negative epochs; complete periods truncate checked elapsed time.

Wide DATE day/week/hour/second and calendar differences remain distinct from
units that first construct microseconds or TIMESTAMP. Intermediate conversion
and subtraction errors remain errors even when a final quotient could fit.
Physically valid but unrenderable timestamp instants stay valid inputs for
operations that do not require calendar reconstruction. This does not narrow
their storage domain or change diagnostic display.

`ScalarBindArguments::is_closed` supplies dependency/effect metadata without
evaluation. The family then uses the separate selected `is_provably_null` probe
only after selecting a valid overload. A successful NULL specialization keeps
BIGINT metadata and the existing TypeOnly contract. Failed speculative data
conversions do not establish NULL; selected Resource/Internal/logical-validation
failures remain fatal. The ordinary path keeps selected VARCHAR casts, including
replacement ENUM conversions, and prepares again with supplied typed parameters.
Constant specifier dispatch precedes dynamic endpoint NULL/infinity checks.

The four new family tests exercise both evaluators, prepared parameters, nested
DATE/TIME children, both index implementations, joins, groups, ordering, windows,
defaults, rejected writes, updates, rollback, native WAL, checkpoint and reopen.
Two independent metadata contract tests cover both optimizers, ordinary and
replacement evaluators, lazy failing children, effects, unsupported frontends,
argument bounds and interruption during the selected bind callback.

## Retained trials and current comparison

All reports retain exact SQL, errors, binary/source hashes and both pinned
reference identities. SQL rejection comparison in this driver is deliberately
coarse; it is not proof of exact error-category or capability parity.

| Trial | Development SQL | Release SQL | Development native producers |
| --- | --- | --- | --- |
| [Initial family](temporal-differences-reference-initial.json) | 1,261 / 1,262 | 1,178 / 1,262 | 2 / 3 |
| [Context investigation](temporal-differences-context-initial.json) | 1,263 / 1,267 | 1,180 / 1,267 | 0 / 3 |
| [Repaired source](temporal-differences-reference-repaired.json) | 1,284 / 1,289 | 1,201 / 1,289 | 2 / 3 |
| [Final state-aware refresh](temporal-differences-reference-final.json) | 1,284 / 1,289 | 1,202 / 1,289 | 2 / 3 |

The context trial exposed premature evaluation of bad casts in dead CASE
branches. The metadata hook repairs both witnesses; valid-overload-first binding
also preserves Binder rather than prematurely reporting Conversion errors.
The integrated MAP literal path closes the earlier cross-family MAP witness.
No previously passing development SQL statement was lost, including all 917
passes in the prior finalized DATE-syntax report and all 1,263 passes in the
context trial.

The initial native fixture exposed the unrenderable timestamp DEFAULT behavior
below. Its positive workflow now supplies that same raw timestamp explicitly
and retains the failing DEFAULT statement separately. The context trial also
contained an extra closing parenthesis in the expanded native fixture: all three
producer failures in that trial are a test-fixture error, not engine regressions.
The repaired fixture is retained without deleting the failed report.

Both Rust checkpoint and Rust WAL producers now pass their dual-reader checks
against each reference. They carry mixed clock boundaries, wide dates, nested
children, period-difference defaults/results, raw timestamp extrema, updates,
rollback and reopened state. The development C++ producer reaches a new native
catalog gap rather than a temporal value-codec mismatch: its stored function
default has parsed expression class 9, kind 140, which Rust currently rejects.
Release still cannot produce the existing TIME-to-TIME_NS fixture; development
remains the correctness authority.

## Explicit remaining obligations

- A projected constant column from `(SELECT 'bad' p)` and a scalar-subquery
  specifier produce constant vectors in C++. For each difference function with
  an infinite endpoint, development rejects the bad specifier but Rust returns
  NULL. The equivalent `(VALUES ('bad')) t(p)` path correctly returns NULL.
  Bound-expression closedness is not a substitute for physical vector provenance.
- Development rejects insertion using `DEFAULT make_timestamp(-9223372036854775806)`
  with `Conversion Error: Date out of range in timestamp conversion`; explicit
  insertion of the same physical instant succeeds. Rust currently accepts both.
  The driver keeps this failure distinct from physical value validation. A final
  SELECT is used in the final refresh to compare state, not different DML
  row-count transport conventions; the earlier transport mismatch is retained.
- A C++-produced `DEFAULT datesub('month', DATE '2000-01-31', DATE '2000-02-29')`
  cannot yet be decoded by the native catalog reader. Generic stored-expression
  binding/evaluation belongs to the selected integration services, not a private
  temporal-function whitelist in storage.
- C++ reports NotImplemented for unsupported TIME/calendar units. Rust reports
  its Unsupported capability error with the corresponding body. The unchanged
  upstream runner stops there rather than treating it as a completed expected-
  error assertion; no assertion or capability boundary was weakened.
- Bare `FROM VALUES (...)` syntax blocks the unchanged epoch regression file
  before its temporal expressions run. Parenthesized VALUES paths are exercised
  by the family tests and paired SQL driver, not substituted into that source file.
- Zoned/ICU overloads, broader function catalog coverage and temporal performance
  acceptance remain open. Both reference binaries are unchanged and lack ICU.

## Validation

Ordinary check, all-target/workspace clippy, 29 temporal tests, 31 contract tests
and 11 DATE tests pass. No test was newly ignored. The metadata prerequisite's
coverage check found no missing instrumentation, and its all-target traced check
passed in 60.20 seconds with temporary telemetry deleted.

The unchanged [extreme-DATE file](upstream-temporal-differences-extreme.json)
passes all seven records. The [slow-file refresh](upstream-temporal-differences-repaired.json)
preserves 89 date_diff and 135 date_sub records before the explicit TIME/year
capability boundary, matching the [initial trial](upstream-temporal-differences-initial.json).
The [epoch-file run](upstream-temporal-differences-epoch.json) records the separate
bare-VALUES blocker at its first query. Filtered upstream runs keep
`full_suite_passed: false` and return nonzero even when their selected file passes.

The frozen-source full workspace passes with only two preexisting external-CLI
analytics tests ignored. Coverage reports 318 files, 2,971 functions and 215
interface methods with no missing instrumentation. All-target traced compilation
passes in 72.26 seconds and deletes its temporary telemetry. Full
`python3 scripts/verify_kani.py` with pinned Kani 0.67.0 passes all six maintained
harnesses, with zero failures (52.298, 0.860, 0.526, 2.344, 2.995 and 84.542
seconds). Their existing packing/key/index/window/byte-count scope is unchanged;
the new closed-expression and calendar algorithms are not formally proven.
Kani models atomics sequentially, and its warned foreign/caller-location
constructs must remain unreachable within each successful proof.
Worker timings are correctness diagnostics, not isolated performance evidence.
