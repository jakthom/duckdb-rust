# Calendar diagnostic follow-up

This follows [the calendar/MIN checkpoint](temporal-calendar.md). It separates
supported source rejections from missing rewrite capability and repairs overload
metadata without claiming original SQL spans or optimizer-name parity.

## Retained initial diagnosis

The development calendar checkpoint has 101 exact diagnostic differences:
74 Unsupported-prefix cases, 19 Binder candidate/location cases and eight
optimizer-sensitive lower-bound Conversion cases. The last group, native stored
DEFAULT execution and original SQL/span integration remain lead-owned.

The 74 cases are exactly four source-recognized rejection bodies: 18 mixed-month
bucket widths, 36 nonpositive widths, 12 unsupported DATETRUNC specifiers and
eight DATETRUNC statistics specifiers. C++ throws NotImplementedException for
these conditions. Its [final error renderer](../../duckdb/src/common/error_data.cpp)
combines the [exception category](../../duckdb/src/common/exception.cpp) with
`Error: `; a missing word in the old Rust display accounted for the textual
difference, but display alone was not the full contract.

An initially approved global Unsupported-prefix patch (`6c5a16d`) was evaluated
and then reverted (`6fbe73c`) after cross-family review established the distinction.
The [text-only trial](temporal-calendar-prefix.json) gained all 74 development
exact matches (800/827) and 62 release matches (700/827), with unchanged broader
value-or-rejection outcomes and 3/3 mixed native paths per pin. Its
[regression refresh](temporal-calendar-prefix-regressions.json) retained all
1,288 development and 1,206 release passes of 1,289. These trials are retained,
not accepted as a capability-classification repair.

`Error::Unsupported` still means missing rewrite capability and keeps the worker's
`unsupported: true` control signal. A separate scalar-owned
`Error::NotImplemented` represents a source-supported rejected operation/input,
with the exact native display and without the missing-capability flag. Neither
category is a recoverable TRY_CAST input failure merely because a callback asks
to recover it. Calendar consumers migrate only the four source-confirmed bodies;
an absent ICU adapter or frontend capability stays Unsupported.

The accepted category consumer `6886f1d` reproduces the text-only gains in a
separate [classification-aware paired trial](temporal-calendar-rejection.json):
development 800/827 exact, 827/827 value-or-rejection outcomes; release 700/827
exact, 742/827 outcomes. All three native calendar paths pass per pin. The
complete-error comparator is unchanged, and the driver correctly exits nonzero
for the remaining differences. The original text-only trials remain retained.

## Selected overload metadata prerequisite

The new request supplies explicit selected call identity and owned fixed-arity
`ScalarSignature` candidates. It validates every candidate before scoring and
never evaluates arguments. SQL literal/NULL metadata and full-width integer
hints remain distinct from ordinary typed columns, parameters or folded values.
Cast availability and errors come from the selected registry; no private built-in
conversion is consulted. A frontend that cannot supply this capability rejects
it explicitly. Callers validate the returned index before retaining metadata.

The isolated temporal cost prerequisite `4223e0d` supplies development's missing
target ranks: NS 119, US 120, MS 121, S 122, TZ 123 and TZNS 124. Identity remains
zero and absent casts remain absent. This changes ranking, not availability.

Source [candidate selection](../../duckdb/src/function/function_binder.cpp) keeps
later equal-cost candidates then appends the original best last. No-match
diagnostics list the complete advertised set. That set's actual initial order
comes from [extension placeholders](../../duckdb/src/include/duckdb/main/extension_entries.hpp),
not the implementation function's declaration order. Registering
[those placeholders](../../duckdb/src/function/built_in_functions.cpp) also
explains why ICU signatures appear without an available ICU implementation.
Describing a placeholder does not import or implement it.

The SQL [source-context renderer](../../duckdb/src/parser/query_error_context.cpp)
uses original query bytes and expression location/length. The current Rust AST
Function span is partial and omits closing delimiters, and parser-rendered SQL
does not preserve original spacing or comments. Candidate-body tests therefore
remain scoped evidence. The unchanged exact driver must keep full-message
failures until retained original-source integration supplies those spans.

Five prerequisite contracts cover candidate order, literal/NULL/parameter
distinctions, full unsigned domains, no evaluation of failing/effectful children,
selected custom ranking and failure, explicit literal-cast unavailability,
malformed metadata, invalid selected indices, cancellation and missing frontend
capability. They do not prove named/variadic overload or complete catalog parity.

The calendar consumer `fdade43` removes its independent type inference and uses
that selected request for both truncation aliases and bucketing. It validates
selected arity as well as index, retains the signature and origin/offset choice,
and selects a valid overload before NULL-template/statistics probes. ICU remains
an advertised but unavailable implementation. Three family tests exercise all
19 development candidate bodies, malformed frontend index/arity/result metadata,
ICU rejection and selected custom casts through scalar, batch, nested and
prepared execution. These body-only assertions do not remove the original-source
suffix obligation from the unchanged paired driver.

The [post-consumer campaign](temporal-calendar-overloads.json), at merged source
`edb2fde`, retains every prior calendar pass: development 800/827 exact and
827/827 outcomes, release 700/827 exact and 742/827 outcomes, with native 3/3
per pin. All 19 development candidate bodies now agree, while their full messages
remain failed. Release's advertised signatures differ and correctness continues
to follow development. Every one of the 74 Rust supported rejections has the
worker's missing-capability flag false.

The [broader regression refresh](temporal-diagnostics-regressions.json) retains
all 1,288 development and 1,206 release passes of 1,289, with no lost passes.
Its development SQL failure remains the optimizer-sensitive raw timestamp
DEFAULT witness. Rust-produced native paths pass, giving 2/3 native paths per
pin. The development producer still exposes default expression class 9, kind
140, whose decoder remains lead-owned; the release producer cannot create this
fixture because its TIME-to-TIME_NS cast is unavailable.
The [MIN refresh](temporal-diagnostics-minimum.json) is likewise unchanged:
development 23/32 exact and 32/32 outcomes, release 20/32 exact and 29/32 outcomes.
All four independent native MIN paths pass per pin, with 36 development and
30 release typed C++ Value/API checks passing. Neither report is presented as
full parity; each complete driver retains its nonzero result for existing gaps.

## Validation progress

The cost prerequisite passed casts 13, numeric 49, temporal 38, check and clippy.
The generic resolver passed all 65 contracts (five new tests) and clippy; coverage
found no missing instrumentation. The initial traced compilation succeeded, but
its command returned nonzero because the fifth contract test changed the source
fingerprint during the run. That run is not counted as a passing trace checkpoint.
A frozen-source incremental traced check passed in 0.09 seconds and deleted its
temporary telemetry. The full maintained prerequisite Kani command passed all
six harnesses with zero failures, in 51.07, 0.84, 0.51, 2.19, 2.65 and 70.65
seconds. These are the maintained key/aggregate/window/TIMETZ invariants, not a
proof of the new overload resolver.

The calendar consumer passed all 41 temporal tests, all 65 contracts, ordinary
check and clippy. One initial new-test compilation used a nonexistent
`QueryContext::default`; it was repaired to the existing `background` API before
these passing checks. Coverage found no missing instrumentation (372 files,
3,554 functions and 232 interface methods). After merging the combined root
base, coverage remains complete at 373 files, 3,579 functions and 236 interface
methods.

At frozen `edb2fde`, `cargo test --workspace --all-targets` and all-target clippy
passed, including temporal 41, contracts 68, numeric 51, casts 13, compatibility
19 and recovery 14. The two pre-existing analytics tests remain opt-in/ignored;
they were not counted as executed. Python discovery passed 40 tests. The full
traced all-target check passed in 95.93 seconds with zero error returns, panics or
open spans; its temporary telemetry was deleted.

`python3 scripts/verify_kani.py` ran the entire maintained suite and passed 6/6
with zero failures. Harness times were 20.22, 0.93, 0.56, 3.67, 4.61 and 113.43
seconds. The existing caller-location/foreign-call reachability warnings and
sequential atomic/concurrency limit remain explicit. These proofs cover their
bounded maintained invariants, not original SQL diagnostics, the new overload
resolver, every calendar algorithm, native DEFAULTs or concurrent visibility.
No performance acceptance was run by this worker.
