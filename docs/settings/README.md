# Settings, ordering and shared execution contracts

This records the current settings and sorting implementation and its measured
scope. The complete rewrite, SQL/catalog parity, upstream test parity and zero
regressions across all workloads remain unfinished. The
[rewrite principles](../../specs/rewrite-principles.md) and
[strict acceptance requirements](../../specs/testing/parity.md) remain unchanged.

## Implemented contracts

`DatabaseBuilder::configuration` selects a typed configuration provider.
`SettingRegistry` owns definitions, aliases, types, supported scopes and defaults;
ordinary `Setting` implementations normalize proposed values. Owned
`SettingChange` values are checked before publication. Each statement retains an
immutable `SettingsSnapshot`; prepared statements rebind settings on execution.

`SnapshotConfiguration` shares immutable global maps and copies them on write.
`LockedConfiguration` takes snapshots by copying a mutex-protected map. Both use
the same session and registration interfaces. A provider instance defines the
global domain: separate default builders have separate domains; explicitly shared
providers share it. Session overrides take precedence, survive transaction
rollback and disappear on RESET or disconnect. Global changes become visible to
other sessions at their next statement.

`SET` and `RESET` publish only after the scheduler returns successfully from
executing the statement exactly once. Rejected values, cancelled work and
scheduler omission/replay/failure cannot partly change configuration. Normalized
values are checked before publication, so an inconsistent custom normalizer
cannot leave subsequent statement snapshots unusable. Floating values preserve
IEEE representation, including NaN payloads and signed zero. Ordinary equality
was insufficient for this boundary because NaN is not equal to itself.

`ScalarFunction::bind` and `ScalarBindArguments` provide optional contextual
binding through the selected type/expression interfaces. Registered
`current_setting` captures a typed value through this interface. The caller does
not branch on the function name. The built-in settings currently cover
`default_order`, `default_null_order` and its `null_order` alias. Most upstream
settings and pragmas remain unimplemented.

`ORDER BY ALL` expands the projected output after wildcard expansion and orders
by output ordinals. It supports duplicate aliases, set-operation results and
explicit/default direction and NULL placement without reevaluating projections.
Pure, total projections use checked column evaluation; effectful or potentially
failing expressions retain row evaluation.

`SortAlgorithm` is explicitly selected through
`NativePhysicalPlanner::with_sorting`. `ComparisonSort` performs fallible stable
merge sorting; `RadixSort` performs stable integer radix passes with comparison
fallback. Their common contract owns key evaluation, ordering, NULL placement,
stable ties, independent state, result ownership, cancellation and row limits.
Each comparison key is validated before sorting, even for a singleton input.
Radix eligibility requires a separate type ordering capability; integer equality
does not authorize bypassing a custom comparator. Both implementations retain
the selected expression and logical type adapters.

The original measured `ORDER BY ALL DESC` regression was
**32.826 ms Rust versus 6.239 ms C++ v1.5.5**, or **5.26 times the C++ latency**.
The original report remains intact.
The final campaign below measures the replacement on the same 50,000-row workload
and reruns the existing fifteen workloads. This is regression removal, with no
relaxed threshold or omitted slower workload.

## Shared verification

- Settings run their 40-record corpus, the 25-record named-session corpus and
  the 20-record ordering corpus across 48 combinations of configuration provider,
  optimizer, expression evaluator, result executor and batch size.
- Sorting runs the same 20 ordering records across 48 combinations of sort
  algorithm, optimizer, expression evaluator, result executor and batch size.
- Sorting contracts cover every signed integer width and extremes, both key
  directions and NULL placements, stable ties, empty/singleton inputs, flat,
  dictionary and constant encodings, custom integer comparison, invalid
  capabilities, logical validation, malformed key batches, cancellation and row
  limits. Fallback checks cover strings, Booleans, NaNs, infinities and signed
  zero, including exact retained floating payloads. Full 50,000-row output order
  is checked independently for both sorting algorithms.
- Effect tests check row evaluation order, single evaluation of hidden sort
  expressions, first errors and owned result chunks after mutation/disconnect.
- Settings failure tests cover malformed registrations, logical values, scheduler
  failures, scope/lifetime behavior, NaN representations and invalid normalization.

The native SQL harness now owns default and named connections per file, all from
the selected database composition. Query and statement connection directives keep
session and transaction state independent. Unknown directives fail; an
`Unsupported` result can no longer satisfy an expected-error record. This fixes
the previously recorded `query T a` harness failure without skipping that file.
It does not add the complete upstream SQLLogicTest language to the native runner.

The persistent reference harness builds the current release Rust worker by
default and records source, binary, harness and actual adapter identities. An
explicit prebuilt worker is marked as having unverified source. Source and binary
stability are checked after the campaign. The C++ worker retains native named
connections and uses framed, bounded transport with read/write deadlines.

Regression evidence is preserved in
[value-boundary failures](value-boundary-before.log),
[value-boundary fixes](value-boundary-after.log),
[sorting failures](sorting-contracts-before.log) and
[sorting conformance](sorting-contracts-validated.log). The last pair demonstrates
two rejected-key failures before the comparison-sort boundary fix.

## Current evidence

Final native source SHA-256:
`ed3247685d71acc28c6670d2f7000c26daedb57554d369c5d19e0742f5cde1c7`.
Measurement binary SHA-256:
`544137bbe6d264dc8a10578fbaa32bb8c7bcbaeb702b45311b1da70aecbdfba7`.
All six performance reports use that same source and binary. The final source,
test and harness fingerprints remained unchanged during validation.

Formatting and Clippy with warnings denied pass. **209 local Rust tests pass**;
**87 selected component tests also pass in release mode**. All **24 Python
harness tests pass**. There are no failed or ignored local Rust tests.

The final validation manifest records exact commands,
exit codes, test counts, source fingerprints and logs. Its debug suite covers all
local targets; selected execution, settings, grouping, recursion, ALTER, index,
SQL and type contracts also run in release mode. The Python harness tests run
separately. These are local test counts, not upstream case-parity counts.

The persistent SQL comparison runs all 120 local records
against **v1.5.5** (`d8cdaa33fda8df955cc76ef58a280f68f4cd43fa`) and pinned
development **v2.0.0-dev84019** (`99063af2bd7092aff02e14184a20e24699d34d71`).
The development reference is not asserted to be the latest remote main. Its
source, executable and loaded library identities are verified separately.

| Local corpus assertions | Rust, each reference run | C++ v1.5.5 | C++ development |
| --- | ---: | ---: | ---: |
| Settings | 40/40 | 39/40 | 40/40 |
| Named sessions | 25/25 | 25/25 | 23/25 |
| Grouping | 35/35 | 34/35 | 34/35 |
| Ordering | 20/20 | 20/20 | 20/20 |

The overall comparison remains failing. Differences are preserved at their
original assertions and described below; neither C++ baseline substitutes for
the other.

The unchanged upstream campaigns select all eight
grouping-set files, including slow cases, and
all 35 ordering files. Unsupported capabilities,
failed assertions and missing settings remain failures or explicit gaps.

| Unchanged upstream selection | Passed files | Failed files | Unsupported files |
| --- | ---: | ---: | ---: |
| Grouping sets | 5 | 2 | 1 |
| Ordering | 3 | 16 | 16 |

## Performance evidence

**All sixteen measured workloads pass against both C++ references.** Sorting
now has shared conformance coverage as well as the measured latency result:

| ORDER BY ALL DESC, 50,000 rows | Rust median | C++ median | C++ / Rust |
| --- | ---: | ---: | ---: |
| v1.5.5 | 3.592 ms | 6.310 ms | 1.76× |
| Pinned development | 3.629 ms | 14.513 ms | 4.00× |

The initial release campaign recorded Rust at 32.826 ms, or 5.26 times C++
latency. The earlier inspection's sorting-conformance exclusion is now addressed
by the shared tests and the two comparison-key validation fixes. Its original
report remains an accurate record of that earlier source.

Each campaign uses optimized builds, one execution thread, in-memory data,
three warmups and 21 paired samples with alternating engine order. Every sample
validates expected row count and integer checksum; independent contract tests
check full values, types and ordering. The maximum Rust/C++ median ratio is 1.0
per workload. Failed earlier reports remain in their original paths.

Timings include execution and complete result consumption/checksum through the
embedded APIs. Initial preparation, setup and startup are excluded. DDL resets
the catalog before each sample and verifies the resulting change after timing;
execution, result consumption and required prepared rebinding are timed.
Durable DDL, cold I/O, concurrency and memory/CPU/I/O cost parity are not covered.

Raw reports include every sample, SQL, expected result, build identity, workload
hash and adapter selection:
core release,
grouping release,
ordering release,
core development,
grouping development,
ordering development.

## Remaining requirements

Most settings, pragmas, configuration-dependent engine behaviors and setting
introspection remain missing. Sorting still materializes its input and output;
there is no byte-accounted allocator, spilling, bounded Top-N or parallel ordered
execution. Richer type/collation ordering depends on the missing type system.

Known SQL differences remain explicit: the release's empty `current_setting`
key diagnostic differs; the development build aborts an explicit transaction
after an invalid setting where Rust and v1.5.5 retain it; both C++ references
disagree with Rust on an EXISTS/duplicate-empty-grouping-set LIMIT/OFFSET case.
Rust also accepts bare ASC/DESC setting values that both C++ parsers reject; the
initial ordering comparison preserves that finding.

Upstream grouping files still encounter missing `random()`, NATURAL/USING joins
and an empty-ROLLUP parser diagnostic difference. Full SQL/catalog and native-file
compatibility, original-suite mappings, clients, UI and extension compatibility
remain open. See the [SQL/catalog worklist](../sql-catalog-parity.md) and
[testing parity report](../testing-parity.md).
