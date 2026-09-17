# DuckDB parity: measured baseline and implementation plan

Measured 2026-09-15 against engine source at
`c2fdae761d0f24c8628437901c27f0ae22b1d6b2`, with G01 measurement-only tooling
changes identified in the reports. This is the single maintained work plan;
update the relevant entry when behavior changes instead of adding another
checkpoint/progress report. Source and tests take precedence over this snapshot.

## Target and evidence

The target is the local C++ DuckDB development checkout at
`99063af2bd7092aff02e14184a20e24699d34d71`, with a separate compatibility
obligation against v1.5.5 at `d8cdaa33fda8df955cc76ef58a280f68f4cd43fa`.
Development behavior wins demonstrated disagreements. See
[acceptance](../specs/testing/parity.md) and [reference builds](reference-builds.md).
This is a bounded source target, not an assertion about every future DuckDB
release or every community extension ever published.

Functional parity means observable SQL, results/types/errors, catalog and
transaction behavior, durable files, configuration, resource behavior, APIs,
clients, and the extensions included in the agreed target. It does not require
copying C++ classes or algorithms. The rewrite's
[replaceable-subsystem requirements](../specs/rewrite-principles.md) still apply.
Alternative OLTP, graph, AI and non-DuckDB formats are separate expansion goals;
they should not silently enlarge this parity backlog.

The G01 measurement slice runs both complete SQL-suffixed source populations,
inventories source declarations and installed reference configurations, enumerates
the available compiled native registry, and exercises existing independent
compatibility and benchmark probes. This is a comprehensive accounting of what
the current tooling can and cannot assess, **not a full engine-parity score**.
First blockers hide later assertions; the harness still lacks important upstream
semantics. No external client/platform population is claimed as executed.

### Measured population

| Source inventory | Release v1.5.5 | Development pin |
| --- | ---: | ---: |
| Tracked source assets | 14,601 | 15,646 |
| SQL-suffixed candidate assets | 4,835 | 5,638 |
| SQL candidates under C++ core discovery roots | 4,834 | 5,637 |
| Native test declarations | 610 | 1,506 |
| Python test declarations | 308 | 561 |
| Swift test declarations | 132 | 132 |
| Benchmark declarations | 980 | 1,160 |
| Test configuration files | 42 | 65 |
| CI workflow files | 39 | 31 |
| External-extension reference/config files | 23 | 23 |

These are source declarations/files, not executed assertions, generated instances
or supported-feature counts. The suffix inventory includes the binary fixture
`data/parquet-testing/orders_small_parquet.test`; it is not a SQL program. Keep
that inventory correction visible rather than silently changing a campaign's
denominator. C++ `test/sqlite/test_sqllogictest.cpp` discovers `test/`,
`third_party/sqllogictest/test/` and configured loaded-extension test roots, not
repository-wide suffixes. All 4,834 eligible release source paths occur in the
compiled registry; no core source paths are missing. The retained
[manifest](../test/upstream/duckdb/manifest.json) remains
the development archive identity; it is not an assertion of runnable eligibility.

The release Catch registry lists **5,502 unique names: 4,834 SQL file names and
668 other registrations**; 917 names are hidden and 747 SQL cases are tagged
slow. The development Catch registry lists **7,189 unique names: 5,637 SQL file
names and 1,552 other registrations**; 935 names are hidden and 791 SQL cases are
tagged slow. Both listings have exact source, binary, CMake-cache and output
identities, and every core SQL discovery path appears exactly once. Listing is not
execution. Catch does not expose runtime `GENERATE`/`SECTION` instances: source
inventory finds 12 generator sites on each pin and 205 release / 389 development
section sites without claiming them as executed cases. The source matrix contains
42 / 65 test configs, 27 / 3 CI test-config invocations and 103 / 80 CI platform
declarations; these are declarations, not built matrix instances. Rust has 48
Cargo targets and 853 textual `#[test]` declarations across the workspace, not 853
mapped upstream tests. Neither count measures native/API invariant coverage.

Both pinned CLI/library identities are checked. Release has core_functions,
JSON and Parquet available; development lacks JSON. Neither installed reference
has ICU/TPCH/TPCDS available. Extension autoinstall/autoload is disabled. The
captured build configurations differ (release C++ `-O3`, development `-O1`);
results describe those exact builds and the local macOS arm64 host, not a full
configuration/platform matrix. Runtime function catalogs are inventoried for each
reference build; no Rust overload-completion percentage is inferred from them.

Raw, revision-specific evidence lives under ignored `target/g01-20260915/`:
`inventory-complete/inventory.json`, `sql/accounting-final.json`,
`compatibility/summary.json` and `performance/*-fastest.json`.
The authoritative G01 Wave A paired runner evidence is under ignored
`target/g01-wave-a-20260915/performance/acceptance-6.json`. The retained
`acceptance-1.json` attempt is rejected because its serializer discarded raw
observations; `acceptance-2.json` through `acceptance-5.json` predate later
audited regex/output/fixture repairs. None of the earlier reports is acceptance
evidence for the final tree.
Wave B evidence is under `target/g01-wave-b-20260915/`:
`registry-integrated-2/inventory.json`,
`api-map-integrated-1/api-contract-inventory.json`,
`extension-pins-integrated-1/extension-pin-inventory.json`,
`api-lifecycle-acceptance-3/report.json`, and
`performance/g01-2c-acceptance-4.json` are the retained final-tree results.
Source/binary/harness hashes and exact commands belong in those reports. Historical
pass counts and the superseded milestone baseline have been removed from this plan;
they must not be added to fresh scoped results.

### SQL outcomes and their limits

The complete first passes used a 10-second per-file deadline and four workers
per pin. Only their 12 development / 11 release timeouts were retried at 60 seconds.
The table uses the retry outcome for those IDs, excludes the one binary fixture,
and preserves both original campaigns. The effective result is not a second full
60-second campaign. No oracle assertions were weakened.

| Effective first-blocker outcome | Release | Development |
| --- | ---: | ---: |
| Entire file passed under current harness | 221 | 528 |
| Engine explicitly unsupported | 2,633 | 1,491 |
| Assertion/error mismatch, attribution requires triage | 722 | 2,009 |
| Harness directive/oracle unsupported | 1,249 | 1,597 |
| Harness text/grammar parse barrier | 1 | 2 |
| Still timed out at 60 seconds | 5 | 7 |
| No SQL records: two empty/comment files | 2 | 2 |
| Control-only storage lifecycle file: not scored as SQL pass | 1 | 1 |
| Total core candidate files | 4,834 | 5,637 |

The original 10-second runs passed 218 release / 526 development files; retries
added 3 / 2. Effective passing record executions are **13,912 / 22,389** and
include successful prefixes of failed files and loop repetitions. They are not
whole-file passes or unique assertions. The retained reports' SQL-attempt and
unreached-loop totals are not trustworthy enough to aggregate; final accounting
marks them unknown. Current runner counters distinguish SQL from control requests.
No crash was observed; zero observed crashes is not a crash-safety proof.

The largest first blockers include release `PRAGMA enable_verification` (1,906
files), development `require parquet` (431), `require json` (189), and the
unrecognized `profiling_renderer_settings` setting (135). `require` itself is a
harness barrier here, not proof that every dependent query fails in Rust. The
release and development pass counts have different populations, controls and
semantics; comparing them as a release regression rate is invalid.

G01 Wave A now implements G01.2a, G01.2b and G01.2d in the Rust runner: a
byte-preserving parser with explicit declaration/attempt/pass/skip/fail/unreached
accounting; source-rooted includes, bounded fixture staging and gzip extraction;
`require`, mode, environment, tag and sleep controls; and the typed numeric,
sort/hash/label, external-file, full-match regex and error oracle. Skips are not
reported as passes, and unsupported/internal/verification engine errors cannot
satisfy expected SQL errors. The development invalid-UTF-8 input is byte-preserved
through parsing and rejected explicitly by the current UTF-8 SQL transport at byte
offset 14; the engine is not invoked, so this is not evidence for JSON diagnostic
or invalid-byte engine parity. Both pins' invisible-space inputs are preserved and
parsed in byte-level regression tests. The production diagnostic boundary remains
with G02.4.

The focused integrated targets pass 109 runner/corpus tests, including deliberate
wrong-value, wrong-hash, invalid-regex, fixture-escape and skip false-green cases.
Both pinned native runners and the Rust production runner pass the two declared
Wave A SQLLogic workloads. On nine paired samples after three warmups, Rust was
at most 0.291x the faster pinned wall-time median and 0.293x its peak RSS; measured
CPU and block I/O were independently no worse. Gate P therefore records
**at-parity or better performance: pass** for each of G01.2a/b/d. These bounded
workloads do not change the older 4,834/5,637-file campaign counts above.

Wave B has landed the bounded G01.1b registry/configuration inventory and the first
G01.3a API mapping. The mapping ties the open-transaction connection-destruction
assertion at `test/api/test_api.cpp:70` / `:73` to the Rust public-contract test
`transactional_ddl_constraints_and_abandonment`, including destruction with an
open DDL transaction followed by same-name table recreation. That is **1** mapped assertion
out of 5,368 selected release / 10,571 selected development source assertions;
the remaining 5,367 / 10,570 are explicitly unmapped. G01.1b remains open because
runtime generated instances and the full built configuration/platform matrix are
not observable in the current artifacts, and its inventory invokes the C++
registries rather than an equivalent Rust candidate, so Gate P is open. G01.3a's
bounded lifecycle operation passes its dual-pin Gate P workload: after three
warmups and nine paired samples per candidate/reference, the two Rust medians are
at most 0.332x the faster C++ wall median and 0.297x its peak RSS; CPU and block
I/O are independently no worse. Thus **at-parity or better performance: pass**
for this one mapped lifecycle operation, while G01.3a remains open for the other
5,367 / 10,570 assertion mappings. The bounded
G01.2c adapter now implements the development pin's unsigned `idx_t` loop schedule,
foreach collections and execution-time variable-token expansion, `std::stoll`
condition prefixes and `continue`, shuffled concurrent connections with
join-all/opportunistic-stop behavior, named database/connection identity, and
distinct load/restart/reconnect lifecycles. Restart preserves the implemented
global and main-session configuration, including `search_path`. Its source-matched local
fixture passes both pins (20 Catch assertions each) and Rust (16 executed records,
zero skips). Its final-tree three-warmup, nine-sample Gate P campaign records a
worst-case 0.331x Rust/faster-C++ wall ratio and 0.308x RSS ratio; measured CPU is
0.000x at timer resolution and block I/O is equal. Thus **at-parity or better
performance: pass** for G01.2c. Raw evidence is retained in
`target/g01-wave-c-20260915/api-lifecycle-acceptance-10/report.json`. Explicit
remaining runner limits are the
integrated test-config max-thread surface, `load ... VERSION`, the engine's absent
`unnest(getvariable(...))` variable-iteration surface, Python concurrent-stream
precompilation for mutation-dependent nested variables, and Python-proxy sibling-tail
stopping for result/label mismatches that are detected only after a concurrent batch
returns. The Rust adapters also fail closed after 100,000 expanded loop iterations
instead of attempting a source runner's effectively unbounded range. G01.4 must
rerun both complete populations before the remaining first-blocker counts can
reliably rank all engine families.

### Independent compatibility measurements

These probes exercise selected already-implemented families, not the full upstream
population. Their overlapping cases must not be summed into a global score.

| Existing probe | Release | Development |
| --- | --- | --- |
| Numeric SQL | 719/1,091 | 1,090/1,091; remaining IEEE-setting diagnostic mismatch |
| BIGNUM SQL | 39/45 | 45/45 |
| BIT SQL | 60/77 | 77/77 |
| BLOB/UUID/base64 SQL | 134/136 | 136/136 |
| Anonymous ENUM SQL | 29/29 | 29/29 |
| Temporal-minimum exact comparisons | 20/32 (29/32 outcome categories) | 23/32 (32/32 outcome categories) |
| Broad supported-subset native campaign | 39 checkpoints of the script passed | HUGEINT declared-value comparison repaired; next source boundary remains to be measured |
| Named ENUM and current-timestamp native handoffs | Passed selected bidirectional cases | Passed selected bidirectional cases |

Numeric and ENUM native persistence each pass six selected paths per pin;
BIGNUM/BIT/binary-scalar each pass three per pin. Typed-value codec checks pass
157/157, VARIANT WAL reference-reader checks 5/5, and the selected VARIANT
checkpoint SQL probes 49/49. These do not establish all codec/version support.

Two important classifications from focused follow-up:

- **Repaired G03.3a engine mismatch:** `enum_range_boundary(k, NULL)` now preserves
  its source-shaped batch endpoint semantics as a naked root and through casts,
  scalar/binary parents, predicates, lazy selected branches and shared projections;
  fallible children are vector-evaluated before the boundary callback, so valid
  VARCHAR-to-ENUM casts work without weakening cast errors or selected laziness;
  eager enclosing parents materialize immediate children in source order so a
  later physical descendant cannot overtake an earlier sibling error. Predicate
  AND/OR trees retain recursive selection-vector short-circuiting and expose the
  demanded selected batch, not singleton rows, to the physical callback, including
  inside CASE conditions. Null-on-constant parents retain the development pin's
  actual runtime vector encoding without inferring constancy from equal values: an
  executed constant NULL suppresses later arguments through all-selected
  CASE/NULLIF branches, casts, unary and binary operators, `BETWEEN`, `IN`, and
  multi-argument scalar wrappers, while a mixed or flat NULL does not.
  The final-tree independent suite is 29/29 SQL plus six native paths per pin.
- **Repaired typed harness comparison:** declared HUGEINT/UHUGEINT values now compare
  by their exact declared integer value when a JSON transport renders a finite exact
  integer as a number. This does not classify ordinary VARCHAR numeric text as equal.

Release FUNCTION-default WAL recovery retains an upstream C++ internal replay
failure; development default handoffs pass. ALTER and settings-session probe
failures include C++/corpus expectation differences needing isolated contract
review. Release-only numeric/type divergences remain visible and do not override
the development contract. Raw reports record exact commands and source identities.

### Performance baseline

All **34 existing microbenchmarks** ran sequentially against both pinned builds,
with three warmups and 21 paired alternating-order samples. Tracing was disabled;
Rust used release/no-default-features. No SQL campaign or other build ran alongside
timed sampling. Each workload selects the faster C++ median and gates **both**
Rust campaign medians at `Rust/C++ <= 1.0`; speedups do not offset slowdowns.

| Manifest | Meets faster-reference gate | Fails gate |
| --- | ---: | ---: |
| Native scan/DDL/recursion | 4/12 | 8 |
| Numeric | 1/8 | 7 |
| Relational | 3/10 | 7 |
| Grouping | 0/3 | 3 |
| Ordering | 1/1 | 0 |
| Total | 9/34 | 25 |

Largest observed Rust/faster-C++ ratio ranges across the two Rust medians:
ADD COLUMN **258.8–268.6×**, decimal total-cents aggregation **39.4–40.3×**,
ordinary aggregation **16.3–17.9×**, decimal filtering **13.7–14.7×**. These are
fresh performance failures, not claims about their cause or a cross-revision
regression. Investigate against pinned source without changing default evaluation
demand or checked numeric semantics merely to improve the numbers.

The optimized Wave C source-bound workloads were remeasured serially with three
warmups and nine samples against both pins. SQL evidence is retained under
`target/g01-wave-c-20260915/performance-current-final-8/`; lifecycle evidence is
`target/g01-wave-c-20260915/api-lifecycle-acceptance-10/report.json`. The SQL
reports retain one final source and release-binary digest, and every measured
invocation exited successfully. Every bounded workload has **at-parity or better
performance: pass** against the faster C++ median in every metric: G01.2c
lifecycle (wall 0.331x, CPU 0.000x at timer resolution, RSS 0.308x); G03.3a ENUM
(wall 0.507x, CPU 0.500x, RSS 0.888x); G04.2a `make_date(STRUCT)` (wall 0.762x,
CPU 0.500x, RSS 0.591x); G06.1a case conversion (wall 0.324x, CPU 0.000x at timer
resolution, RSS 0.468x); G07.3a DISTINCT ON (wall 0.557x, CPU 0.500x, RSS
0.909x); G08.1a `product` (wall 0.842x, CPU 1.000x, RSS 0.280x); and G10.4a
profiling with `no_output` (wall 0.370x, CPU 0.000x at timer resolution, RSS
0.615x). Block I/O is equal for every workload and throughput passes by the same
wall-time comparisons. G10's broader functional scope remains open: a passing
`no_output` performance workload does not implement forced-external execution or
the remaining catalog/configuration families.

The timed harness checks row counts and sums (and DDL effects), not an exhaustive
typed-value oracle. Scope is serial embedded, primarily in-memory execution;
setup/initial preparation/startup are untimed. The 1,160 development benchmark
declarations, cold/warm file I/O, CPU, peak memory, durable commits/recovery,
concurrency and client/API timings remain unmapped/unmeasured. **Performance parity
still fails the older 34-workload baseline outside these repaired slices.** No
percentage of overall performance completion is inferred from 9/34.

## What already exists

| Area | Implemented foundation | Remaining boundary |
| --- | --- | --- |
| SQL | SELECT/VALUES, ordinary DML, schemas/tables, selected ALTER, transactions, EXPLAIN | Many statement families/modifiers and exact binding behavior |
| Relations | Ordinary outer/semi/anti joins, NATURAL/USING, subqueries, set operations, recursive UNION, grouping sets, core windows/QUALIFY | Lateral/ASOF/positional joins, richer CTEs, PIVOT/UNPIVOT, sampling, remaining frames and syntax |
| Values | Signed/unsigned integers, DECIMAL, floating, BLOB/UUID/ENUM/BIT/BIGNUM, temporal and nested families including VARIANT/TUPLE | Complete coercion/function coverage, remaining named/user-defined types, GEOMETRY, context-sensitive behavior |
| Expressions | Selected registered casts/operators/scalars/aggregates, scalar/batch evaluators, and a closed retained-default lifecycle | Broader retained-expression consumers and complete function/overload semantics |
| State | Copy-on-write snapshots, rollback, basic uniqueness/NOT NULL, prepared rebinding, hash/B-tree indexes | DuckDB conflict timing, independent concurrent writers, catalog dependencies and incremental index maintenance |
| Native persistence | Selected storage versions 64–69, many native readers, selected WAL/checkpoint/recovery and crash tests | All objects/types/versions, compressed writing, large/partial I/O, concurrent maintenance and encryption |
| Execution | Pull/eager alternatives, selected joins/grouping/sorting and cancellation | Cost-based planning, byte accounting, buffer management, spilling, parallel/pending execution |
| Integration | Rust library and a small SQL shell | DuckDB C APIs, Arrow/ADBC, language clients, general external scans and extension loading |

Do not re-port unsigned/decimal/nested values, windows or NATURAL/USING from
scratch because an old report called them absent. Build on the existing source.
No broad subsystem in this table has evidence of full parity.

## How to assign this work

Each **Gxx group** is a goal to hand to an owning agent. Each **Gxx.n chunk** is
a smaller implementation outcome, with several concrete tasks. A group can span
many commits; it is not a promise of completion in one agent turn. If a chunk
exceeds one coherent change, split it again while retaining its parent ID.

For every assigned chunk:

1. Resolve the pinned source/tests for its precise behavior. Classify existing
   passing behavior, engine gaps, harness gaps and reference divergences.
   Declare its validation manifest: fast commands, exact affected cases, full
   acceptance commands and performance workloads/configurations before editing.
2. Implement through the selected public contracts. Include names, types, NULLs,
   errors, effects, ownership and applicable configuration semantics.
3. Carry it through its actual consumers: scalar/batch, prepared use, transaction
   failure/rollback, persistence/reopen and other relevant APIs. A type declaration,
   parser branch or isolated codec is a prerequisite, not the complete outcome.
4. Run affected unchanged upstream cases and adversarial regressions; preserve
   known passing cases. Map non-SQL assertions explicitly to Rust contracts.
5. Treat the agent's "100% complete" assessment as ready for verification; freeze
   the integrated tree. At that boundary, delegate `python3 scripts/verify_chunk.py` to
   the configured low-cost verifier. Follow [AGENTS.md](../AGENTS.md). Kani findings
   must be investigated/reported under the exploratory policy; proof success is
   not a substitute for ordinary correctness or upstream execution evidence.
6. Require **at-parity or better performance** for every affected workload under
   [gate P](#at-parity-or-better-performance). Run this acceptance campaign on the
   final tree, separately from concurrent agents' work and the regression sweep.
   Functional success or improvement over a slow Rust baseline is insufficient.
7. Update this group's status with the source revision, tested population and
   remaining cases. Keep raw logs/JSON under ignored `target/`; report compactly.

One integration owner coordinates changes to `DataType`/`Value`, catalog identities,
bound expressions/plans, shared registries, `DatabaseBuilder`, transaction/storage
contracts and wire formats. Family owners propose focused changes to those files;
uncoordinated parallel edits to the same shared enums are not independent work.
Use separate worktrees. Do not mutate the pinned references or shared test workers
during another campaign. Performance measurements require a quiet host.

## Goal map and dependencies

Dependencies below name required capabilities, not a requirement to finish every
subtask of a large predecessor before beginning design or a usable increment.

| ID | Agent goal | Main prerequisites |
| --- | --- | --- |
| G01 | Reliable parity inventory and faithful harnesses | Existing source/test inventory |
| G02 | SQL parsing, binding, naming and diagnostics | Existing binder; coordinate G09/G10 |
| G03 | Scalar types, numeric coercion and math | G02 binding contracts |
| G04 | Temporal and timezone completeness | G03 conversion contracts; G10 settings; G23 ICU |
| G05 | Nested values, lambdas and core GEOMETRY | G02/G03; coordinate G12 codecs |
| G06 | Text, collations and utility functions | G02/G03; G23 ICU where required |
| G07 | Remaining relational SQL | G02; G05 for UNNEST/PIVOT values |
| G08 | Aggregate and window completeness | G03–G07 |
| G09 | Retained expressions and default lifecycle | Existing stored-expression prerequisites |
| G10 | Catalog objects, settings and attachments | G02/G09; coordinate G14 |
| G11 | Mutations, constraints and schema evolution | G09/G10/G14/G15 contracts |
| G12 | Native checkpoint/file compatibility | G03–G05/G09–G11 as types/objects land |
| G13 | WAL, recovery and checkpoint lifecycle | G12/G14 |
| G14 | Transaction and connection semantics | Existing snapshots; coordinate G10/G13 |
| G15 | Index DDL and selective/incremental access | G10/G14; coordinate G12 |
| G16 | Statistics, optimizer and physical planning | G07/G08/G15; later G17/G18 |
| G17 | Memory accounting, buffers and spilling | Storage/execution contracts |
| G18 | Parallel and pending execution | G14/G17 and operator state contracts |
| G19 | Table functions and external scan contracts | G02/G05; G17/G21 |
| G20 | CSV, JSON, Parquet and COPY | G19/G21; G05 nested values |
| G21 | Filesystems, secrets and encryption | G10/G12/G13; coordinate G23 |
| G22 | Embedding APIs, Arrow and ADBC | G02/G10/G14/G18/G19 |
| G23 | Extension runtime and required extension capabilities | G10/G19/G21/G22 |
| G24 | Clients, shell, distribution and full acceptance | Integrate all relevant groups |

## Parallel execution order and model budget

Port the pinned C++ behavior and unchanged tests; do not redesign a subsystem merely
because its implementation language changes. Preserve the existing Rust interfaces
unless the source contract demonstrates that they cannot express required behavior.
G09's closed-default slice, the named-ENUM lifecycle and the LIST/ARRAY set slice
are existing foundations, not new assignments.

Use one integration lead and at most three active implementation workers in this
environment. Every worker has a separate worktree and one owned chunk. The lead owns
shared enum/registry/plan/catalog interfaces and integration; leaf owners do not
independently edit them. A completion verifier takes a worker slot. Integrate and
verify one stable final tree at a time; no source edits during its full sweep.

| Wave | Parallel lanes | Dependency / handoff |
| --- | --- | --- |
| A — remove measurement blind spots | G01.2a parser/accounting; G01.2b fixture resolver; G01.2d oracle | Separate new parser, fixture and oracle modules; one owner integrates `sqllogic.py`. Agree record/result interfaces first. |
| B — finish measurement contracts | G01.2c concurrent runner; G01.1b native/configuration registry; G01.3a API/client mapping with G23.1 external pins | Runner requires A. Registry/mapping can begin during A when a slot is free; no engine source ownership overlap. |
| C — remeasure and deliver bounded core slices | G01.4 campaign owner; G10.4 verification/settings controls; one G03/G04/G05/G06 or G02/G07/G08 family owner | Settings block 1,906 release files at `enable_verification` alone. Port observable verification behavior; do not accept-and-ignore the PRAGMA to inflate passes. Rerun both populations after integration and select exact newly exposed source-case manifests. |
| D — state and native consumers | G10 catalog object slice; G14 transaction lifecycle; G12/G13 native codec slice | Integrator serializes shared catalog/transaction/wire changes. G11/G15 follow the specific identity/conflict contracts they need. |
| E — execution and external sources | G17 byte/buffer contracts; G19 table-function contracts; G16 statistics/optimizer | Agree value/scan/resource interfaces first; then independent operator/CSV/JSON/Parquet owners. G18 waits for the required resource/state contracts. |
| F — public ecosystem | G22 ABI/Arrow; G21/G23 filesystem/extension capabilities; G24 client/shell packaging | G01 inventories exact external pins before implementation; APIs/clients run against the Rust artifact, never installed C++ DuckDB. |

These are dependency waves, not a requirement to finish an entire group before
starting a dependent slice. G24 accounting runs throughout. Performance campaigns
run serially on a quiet host, outside builds, SQL campaigns and verification sweeps.

Model recommendations below are risk/cost judgments, **not measured model bake-off
results**. Prices are standard API USD per million input/output tokens, checked
2026-09-15; they do not estimate Codex subscription charges or total task cost.
Cached input, tool time, retries and context size change the bill.

| Code | Model | Input / output | Assignment rationale |
| --- | --- | --- | --- |
| L | `gpt-5.6-luna` | $0.20 / $1.20 | Mechanical inventories and report reconciliation with deterministic checks; not semantic engine ports. |
| T | `gpt-5.6-terra` | $2 / $12 | Default for bounded, source-explicit ports with strong reference tests. |
| S | `gpt-5.6-sol` | $4 / $20 | Cross-module binding, ownership and execution semantics where a cheap incorrect design creates rework. |
| A | `gpt-6-astra` | $10 / $50 | Reserve for especially difficult conflict, fault-publication, scheduling and foreign-handle state machines. |

Pricing/capability sources: [model comparison](https://developers.openai.com/api/docs/models/compare),
[Luna](https://developers.openai.com/api/docs/models/gpt-5.6-luna),
[Astra](https://developers.openai.com/api/docs/models/gpt-6-astra).
Sol pricing is currently promotional. Recheck prices when dispatching future waves.
Start L/T at medium reasoning and S/A at high; escalate only a concrete unresolved
contract/counterexample, not every task in the group. The mandated completion
verifier remains **Terra, low**, regardless of the implementation model.

### Minimal validation contract

Declare a per-chunk validation manifest in its backlog entry or handoff before
editing: owned paths, fast check commands, named test targets/filters, unchanged
upstream IDs, negative/boundary cases, full functional acceptance commands and
performance workload IDs/configurations/metrics. This is a test selection contract,
not a second implementation plan. Broaden it when shared contracts or scope change.

During editing, coalesce small logical edit batches and check only the affected
package/target (`cargo check -p duckdb-rust --lib` for library-only changes).
After a behavior change, run the named test target/filter and small upstream case
set; do not add a redundant check before a test that already compiles that code.
For Python harness edits, run the affected `scripts/test_*.py` tests, including
deliberately wrong results and incomplete selections that must fail. Keep caches
worktree-local, profile/features consistent and at most one validation process
active per worktree. Any watcher must debounce edits, record the tested source
state and invalidate stale results. Zero tests or stale binaries are not green.
No full Clippy, recovery sweep, Kani or acceptance benchmarks per edit; focused
timings for diagnosis remain allowed. The debug upstream/cache/watch acceleration
is planned in G01.4a below, not yet available.

Before assigning an engine chunk, freeze its exact source/test manifest, inputs,
required configurations and expected outcomes. A large Gxx.n item is a queue of
bounded source families: split it into Gxx.n.a/b/etc before assigning multiple workers.
The parent is not complete until every child population is covered. The table below
states the **smallest functional evidence**, not permission to reduce the assigned
population to one happy-path smoke test. New behavior must pass all assigned cases,
the relevant negative/NULL/boundary cases and its actual public consumers. Record
release divergences independently; development is authoritative.

Every chunk also gets exactly the repository-required completion workflow:
delegate `python3 scripts/verify_chunk.py` to the configured verifier on the final
integrated tree once the agent reports ready. All ordinary stages must pass, and
Kani must run and be reported honestly under its exploratory policy. The complete
assigned functional population and performance gate P are additional requirements;
the common sweep alone does not run them or prove chunk completion. Any later edit
requires a new full sweep and refreshed affected acceptance evidence. No full-sweep
shortcuts are implied below.
If interfaces or Rust files change, include `cargo dev coverage` and
`cargo dev trace check --workspace --all-targets` as required by AGENTS.md.

### At-parity or better performance

**Gate P applies to every row and every child chunk below, not only optimization
work.** Each chunk validation must explicitly record **at-parity or better
performance** for the changed operation and affected existing consumers. Declare
representative workloads/configurations before implementation; add missing
comparable cases instead of borrowing an unrelated passing result.

For each comparable workload, require
`Rust median / min(release median, development median) <= 1.0` and throughput
at least `max(release throughput, development throughput)`. Gate relevant CPU,
peak memory and I/O independently. Use equivalent correct operations, both exact
pinned references, production release/no-tracing builds and a quiet host. Preserve
samples, identities and failed runs under `target/`. Existing latency manifests
use `compare_native.py` for each pin followed by `fastest_reference.py`; other
workloads/metrics need their corresponding measurement adapters.

Report P as **pass**, **fail** or **open**. A slowdown, missing workload, unexecuted
reference or incomparable evidence cannot close an implementation chunk. Do not
grandfather known baseline failures or defer the gate to G24. Prior "implemented"
labels describe functional progress, not this performance acceptance. Strictly
documentation-only changes may record **not applicable: documentation-only**, with
a reviewed diff proving no executable/build/configuration/fixture/workload effect;
this is not a measured pass. Tooling/runtime changes are not exempt. See the
[full testing rule](../specs/testing/parity.md#per-chunk-performance-acceptance).

### Dispatch matrix: scope, ownership, model and validation

Paths in the goal's Sources paragraph and [architecture map](architecture.md)
define its ownership area. New adapters live beside that subsystem; shared files
remain with the integration lead. `SQL` below means unchanged tests from **both**
pins plus exact types/errors, not just result row counts. `native exchange` means
C++→Rust and Rust→C++→Rust, including continued mutation/reopen.

| Chunk(s) | Model | Owned responsibility | Smallest functional validation | Performance gate |
| --- | --- | --- | --- | --- |
| G01.1a | L | Source/client/extension manifest reconciliation in inventory tooling | Exact pin/hash, unique IDs, known declaration fixtures and explicit absent populations. Current source inventory exists; do not recount it manually. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G01.1b | T | Compiled/generated/config/platform enumeration | Compare source IDs with an actual compiled registry; enumerate hidden/parameterized cases; missing runner/config cannot produce success. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G01.2a | S | Byte-preserving parser and record accounting | Invalid-UTF-8 and invisible-space upstream files; failed attempt, loop-tail, skipped and zero-selection unit tests. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G01.2b | T | Source-rooted fixture/include/require/mode adapter | Upstream include/unzip/expected-file cases; missing fixture, traversal, hash and ineligible-config tests; scratch isolation. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G01.2c | S | Concurrent loop/session/restart runner | Pinned parallelism case with deterministic barriers; named-session isolation, cancellation, crash and restart failure tests. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G01.2d | S | Numeric/RE2/hash/sort/error oracle | C++ oracle agreement and deliberately perturbed values/types/order/errors that must fail. Never accept engine Unsupported as an expected SQL error. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G01.3a | T | Native/API/client assertion mapping | Trace each selected source assertion to a Rust public-contract test, including destruction/failure; unique mapping and unmapped-count checks. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G01.3b | S | Foreign-client test adapters | Test actually loads Rust-built library; wrong symbol/layout/ownership mutation fails. Depends on the corresponding G22 interface. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G01.4 | T | Dual-pin campaigns and regression accounting | All selected IDs have one outcome; retain first runs/retries, immutable provenance and old-pass losses; no skipped/unknown counted as passed. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G02.1 | T | Parser syntax family | SQL parser acceptance/rejection plus parse→bind→execute for each new form. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G02.2 | S | Name/scope resolution | SQL ambiguity, alias/star/correlation matrix and prepared rebinding. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G02.3 | S | Binding contexts and SQL preparation | Literal/parameter/typed-constant overload matrix, repeated execution and invalidation. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G02.4 | T | Diagnostic categories/spans | Exact error assertions with malformed UTF-8, location and unsupported-vs-rejection controls. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G03.1 | S | Cast-family matrix | SQL source/target/boundary matrix through constants, columns and parameters. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G03.2 | T | Numeric overload/function family | Existing `numeric_*` component tests plus source-enumerated SQL signatures/NULL/overflow cases. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G03.3 | T | Binary/UUID/ENUM/BIT family edges | Scalar and batch SQL plus native exchange for changed value representation. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G03.4 | S | Context/IEEE/mixed-family behavior | Prepared-setting changes, lazy errors and nested/group/index key regressions. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G04.1 | T | Temporal physical/text boundaries | `temporal_*` boundary tests plus exact native payloads and reference casts. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G04.2 | T | Calendar/format/current function family | Constant/column/NULL/error SQL and transaction-stable clock tests where applicable. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G04.3 | S | Named-zone/ICU adapter | Provisioned pinned ICU build; DST fold/gap and timezone-setting SQL, explicit absent-build failure. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G04.4 | S | Temporal consumer integration | Prepared/default/group/index tests and checkpoint/WAL exchange for changed types. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G05.1 | T | One nested accessor/constructor family | `component::nested` tests and SQL child NULL/type/shape/error matrix. Preserve already implemented slices. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G05.2 | S | Lambda binding/capture and higher-order family | Nested capture/shadowing, transform/filter/reduce SQL and scalar/batch agreement. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G05.3 | S | Nested relational/mutation consumers | UNNEST/lateral/prepared/nested-update SQL, rollback and mixed-width literals. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G05.4 | S | Core GEOMETRY/type constructors | Pinned WKB/CRS/type SQL, invalid payloads and exact native metadata; not opaque bytes alone. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G05.5 | S | Nested native codecs | Mixed-child/tag/IEEE-bit exchange, version rejection and unchanged-file failures. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G06.1 | T | One text/encoding function family | SQL Unicode/NUL/empty/invalid-byte/NULL cases and batch boundaries. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G06.2 | S | Regex/collation integration | SQL options/errors and same comparison semantics across sort/group/join/index. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G06.3 | T | One utility/volatile function family | Source signatures plus seed/stability/effect-demand tests across statements. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G06.4 | T | Function metadata | Enumerated aliases/overloads/parameters match callable behavior and binder diagnostics. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G07.1 | S | One join/correlation form | SQL empty/NULL/cardinality/outer-row cases plus prepared and nested execution. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G07.2 | S | Recursive/materialized CTE form | SQL recurrence/multiple-reference/type tests plus cancellation/nontermination guard. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G07.3 | T | Relational modifiers/set alignment | SQL name/type/order alignment with aliases, empty and duplicate rows. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G07.4 | S | PIVOT/UNPIVOT/sampling | SQL discovered schemas/NULLs and deterministic seeded sampling cases. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G08.1 | T | One aggregate family | SQL empty/all-NULL/mixed-domain/overflow cases and batch partition equivalence. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G08.2 | S | Aggregate modifiers | ORDER/DISTINCT/FILTER/grouping-mask combinations, error/effect demand. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G08.3 | S | Window frame/function family | Peer/tie/NULL/dynamic-bound/exclusion SQL and invalid bounds. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G08.4 | S | Aggregate/window state adapters | Same result across batch/parallel/spill boundaries; cancellation/cleanup. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G09.1–G09.4 | — | Closed-default slice already implemented | Preserve `stored_*`, default-demand and independent native-default gates; route new consumers to G04/G10/G11. No repeat implementation budget. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G10.1 | S | Remaining identity/scope/dependency integration | Existing catalog-identity tests plus temp scope, rename/drop/cascade and prepared lifetime histories. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G10.2 | S | One catalog object family | CREATE/use/replace/drop, dependencies, rollback and native exchange. Named ENUM already exists. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G10.3 | S | Multi-catalog attachment/routing | Qualified cross-catalog SQL, read-only and forbidden multi-database writes, shutdown/reopen. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G10.4 | T | One metadata/settings family | SQL catalog reflects actual objects; scope/RESET/locking and prepared-setting tests. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G11.1 | S | One DML form | Changed rows/RETURNING metadata, multirow failure atomicity, prepared execution. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G11.2 | S | One constraint/generated-column family | NULL/self-reference/multirow failures, dependency DDL and rollback/reopen. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G11.3 | S | One schema-evolution form | Old/new snapshots, dependent defaults/indexes, reference error timing and native exchange. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G11.4 | T | DML lifecycle integration tests | Deterministic concurrent histories and bidirectional WAL/checkpoint cases for new forms. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G12.1 | S | One version/object metadata codec | Read/create/rewrite/upgrade matrix; metadata and unchanged-file rejection tests. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G12.2 | T | One compression codec/type/version slice | Independent compressed fixtures, decode/encode exchange and malformed boundaries. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G12.3 | S | Selective/large storage adapter | Partial reads return exact rows with measured block I/O and bounded allocation. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G12.4 | T | Native exchange campaign | Cross-producer mixed types/deleted IDs/corruption/version cases, continued writes. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G13.1 | S | One WAL record/version family | FLUSH/abort boundaries and exact state after independent replay. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G13.2 | S | Checkpoint/recovery maintenance | Concurrent/manual/automatic histories, sidecars and repeated recovery. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G13.3 | A | Failure/publication state machine | Short-write/sync/rename/process-kill histories; acknowledged commits survive, unknown outcome stays explicit. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G13.4 | T | Cross-engine recovery handoffs | Alternate WAL/recovery/checkpoint owners then continue writing on both pins. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G14.1 | A | Visibility/conflict domains | Pinned overlapping/disjoint row and catalog histories with exact allowed conflicts. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G14.2 | S | Statement/transaction error lifecycle | Bind/execute/commit/cancel failures, autocommit and prepared reuse transitions. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G14.3 | S | Database/connection ownership | Repeated opens, locks, active results, close/reopen and foreign-handle lifetime. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G14.4 | T | Deterministic history tests | Replay source-mapped read/write/DDL/index/checkpoint schedules, reject incorrect histories. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G15.1 | S | Index DDL/catalog integration | CREATE/drop/composite/expression index SQL, dependency/rollback/native reopen. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G15.2 | S | Incremental maintenance | Insert/update/delete/rollback plus NULL/NaN/nested-key uniqueness and old snapshots. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G15.3 | S | Range/gather access | SQL residual/effect correctness plus instrumented row/block selectivity. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G15.4 | T | Native ART interoperability | C++ visibility/use/mutation and Rust reread; corrupt-index rejection. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G16.1 | T | Statistics lifecycle | ANALYZE/invalidation SQL and measured cardinality fixtures. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G16.2 | S | One optimizer transformation | Optimizer on/off differential corpus with volatile/lazy-error counterexamples. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G16.3 | S | Physical cost/algorithm selection | Correct alternative plans under same inputs/resources plus targeted quiet-host timings. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G16.4 | T | EXPLAIN/profiling contracts | Exact required plan/metric fields, settings and executed-row accounting. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G17.1 | S | Byte ownership/reservations | Fault-injected allocation and nested/vector/operator peak-byte budgets; no leaked reservations. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G17.2 | S | Buffer manager | Pin/evict/dirty/read failure tests plus I/O counts and transaction lifetime. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G17.3 | S | One external operator | Dataset exceeding budget, exact in-memory/spill equivalence and cleanup on failure/stop. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G17.4 | T | Resource acceptance fixtures | Low-memory/disk-full/concurrent cases with peak bytes/I/O and explicit unsupported limits. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G18.1 | A | Scheduler/pipeline state machine | Deterministic exactly-once/barrier/dependency tests, one/many threads and cancellation races. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G18.2 | S | One parallel operator | Batch/partition equivalence, snapshots and mutation atomicity at thread counts 1 and >1. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G18.3 | S | Pending/backpressure API | Real WAITING/progress/cancel/close transitions, blocked source and early destruction. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G18.4 | T | Scheduler histories | Forced blocking, resource pressure, multiple connections and race/stress cases. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G19.1 | S | Table-function binding/lifetime | Independent test source binds schema/options, scans, fails and frees through public API. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G19.2 | S | Pushdown/source capabilities | Residual/projection identity tests, actual scanned rows/bytes and cancellation. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G19.3 | S | Multi-file source adapter | Glob/list/schema-union/partition/filename SQL and reopen/missing-file cases. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G19.4 | T | One core table-function family | Source signatures/output schema plus NULL/empty/correlated SQL. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G20.1 | T | Bounded CSV reader/writer slice | Unchanged CSV boundary/quote/NULL/error cases and independent read/write exchange. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G20.2 | S | Bounded JSON function/reader slice | Missing vs JSON-null vs SQL-NULL, path/number/nested/schema cases and malformed input. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G20.3 | S | Bounded Parquet encoding/type slice | Independently produced pages/files both ways, nested/decimal/time metadata and corruption. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G20.4 | S | COPY/finalization lifecycle | Partition/multi-file exact outputs, failed-write cleanup and import/export exchange. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G21.1 | S | Filesystem capability adapter | Short/range reads, globs, cancellation/locks and non-idempotent failure replay tests. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G21.2 | S | Secret/access-policy lifecycle | Provider scope/redaction/persistence tests, forbidden access and cleanup. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G21.3 | S | Encryption format/lifecycle | Independent encrypted DB/WAL/temp exchange, wrong-key/authentication/torn-write tests. Use audited primitives, not new cryptography. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G21.4 | T | One remote filesystem integration | Controlled server credentials/redirect/range/retry cases and actual transferred bytes. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G22.1 | S | Bounded C v1 symbol/handle family | Compile/link unchanged C test against Rust library; ABI, free/borrow/error tests. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G22.2 | A | C v2 handle/wait state machine | Native C/C++ consumer WAITING/CHUNK/FINISHED/CANCELLED and invalid/destructed-owner cases. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G22.3 | S | Relation/appender/registration adapter | Foreign caller buffering/flush/rollback/early-destroy and nested-value tests. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G22.4 | S | Arrow ownership/type slice | Independent Arrow consumer, offsets/dictionary/nested NULLs and exactly-once release callbacks. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G22.5 | S | ADBC driver slice | Pinned driver tests for metadata/parameters/transactions/streams/errors with Rust provenance. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G23.1 | L | Extension pin/ABI manifest | Resolve configured immutable refs; classify missing refs and ABI coupling, never silently omit. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G23.2 | S | Loader lifecycle | Independent compatible extension, bad signature/version/platform, repeat load and retained callbacks. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G23.3 | T | One built-in extension adapter | Its unchanged SQL/API cases under static/loadable required configs; missing dependency is not a pass. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G23.4 | S | One pinned external capability | Exact extension suite/remote fixture against Rust; port internal-C++-coupled capability explicitly. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G24.1 | S | One client adapter | Pinned conversions/relation/threading/lifetime tests loading newly built Rust artifact. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G24.2 | T | One shell behavior family | Process-level args/stdout/stderr/exit/interrupt tests; terminal fixture for interactive behavior. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G24.3 | T | One build/platform package | Clean install, exported symbols and provenance smoke on actual target OS/architecture. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G24.4 | T | Integrated correctness accounting | All mapped IDs/configs, prior-pass regression diff, faults/fuzz/ignored/zero-test accounting. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G24.5 | T | Performance acceptance campaign | Correct paired samples vs both pins, faster-reference <=1.0 per workload, then CPU/memory/I/O/scale dimensions. | [P: at-parity or better performance](#at-parity-or-better-performance) |

## G01 — Reliable parity inventory and harnesses

**Current measurement slice:** both source populations and both compiled Catch
registries are inventoried; source generator/section sites, test configurations,
CI config invocations and platform declarations are separately enumerated; full SQL
first-blocker campaigns, bounded timeout retries, 18 existing compatibility-probe
invocations and all 34 existing latency workloads have fresh evidence above. The
first API destruction/lifetime assertion is mapped to a Rust public contract. New
tools retain exact identities and reject incomplete or duplicate report selections.
This closes the requested current-state measurement snapshot, **not G01's full
exit**: runtime-generated and built-matrix instances, most native/client assertion
mappings and the explicitly recorded runner limits stay open. Gate P is open for
inventory-only G01.1b. The mapped G01.3a lifecycle slice has **at-parity or better
performance: pass**, and the bounded G01.2c runner now also has **at-parity or
better performance: pass** across wall time, throughput, CPU, RSS and block I/O.

- **G01.1 Inventory the acceptance population.** Enumerate both pins' SQL files,
  native registrations, generated/parameterized cases, slow tests, configurations,
  platforms, client suites and configured external extensions. Record immutable
  case IDs and an owner group; distinguish declaration counts from executed cases.
- **G01.2 Finish SQLLogicTest semantics.** Implement remaining directives, extension
  requirements, external expected files, paths, loops, restart/named-connection and
  concurrent controls. Match reference numeric conversion/tolerance, regex, hashes,
  ordering and labels without changing assertions. Wrong-result harness tests must fail.
- **G01.3 Map native and client assertions.** Translate meaningful API/internal
  invariants into Rust contracts with source-case mappings; do not replace native
  lifetime or concurrency assertions with a SQL smoke query.
- **G01.4 Refresh and classify.** Run a complete current campaign with reproducible
  identities and sensible recorded deadlines. Separate engine/harness/setup failures,
  unsupported, skipped, timed out, crashed and unexecuted cases. Preserve regressions
  and partial prefixes without counting them as full-file passes.

**Exit:** every inventoried obligation has a mapped outcome/owner; the harness
cannot count unsupported expected-error records or omitted work as success.
Sources: `scripts/{run_upstream,sqllogic,upstream_suite}.py`, `test/runner/`,
[testing specifications](../specs/testing/README.md).

Reproduce the inventory with
`python3 scripts/parity_inventory.py --output-dir target/<new-inventory>`.
Run each full SQL pin with
`python3 scripts/run_upstream.py --target development --timeout 10 --jobs 4 --report target/<new-report>.json`
(repeat with `--target release`); use `--retry-timeouts-from <prior-report>` and
`--timeout 60` for the recorded retry policy. `--path-list` selects exact IDs and
rejects unknown paths. `scripts/summarize_upstream.py --help` describes immutable
first-pass/retry reconciliation. Keep the suffix-fixture exclusion explicit until
source discovery is fully integrated into the runner; do not edit the pinned inputs.
The compatibility command list is in its raw `summary.json`. For each
`benchmark/*_workloads.json`, run `scripts/compare_native.py --target <pin>
--iterations 21 --workloads <manifest> --report <new-report>` serially for both pins,
then `scripts/fastest_reference.py` on that pair. Always choose fresh output paths.

### First source-bound follow-ups

The dispatch matrix supplies the rest of the portfolio. These smaller children
are directly motivated by the measured baseline and can be assigned without
inventing a new feature scope. G01.2a/b/d are the first three parallel workers;
the following independent children enter as slots and shared contracts permit.

| Child | Model / owner | Minimal functional gate | Performance gate |
| --- | --- | --- | --- |
| G01.4a — fast feedback orchestration | T; upstream-runner/orchestration owner, coordinate other G01 edits | Add debug-worker mode and hash-validated suite caching without weakening assertions; manifest-selected tests, debounced single-process validation, zero-selection/cache-tamper/stale-source negative tests; compare selected results with the release runner. Feedback timing is not an acceptance benchmark. Validation manifest (2026-09-16): own `scripts/run_upstream.py`, `scripts/measure_upstream_feedback.py`, their focused tests, `benchmark/g01_4a_feedback_workloads.json`, and this row; fast checks: `PYTHONPATH=scripts python3 -m unittest scripts/test_run_upstream.py scripts/test_measure_upstream_feedback.py`. In explicit `--path-list` debug/prebuilt feedback only, a separate selected-feedback cache fetches exactly requested source-root-relative SQLLogic files. Development files require the pinned retained-manifest digest plus archive digest; release uses pinned `git show`; cache hits revalidate canonical selected bytes and exact metadata. Unsafe, duplicate, missing, stale, tampered or non-SQL inputs fail closed. External fixture/include dependencies are not materialized by this narrow cache and are not claimed supported. A prebuilt worker requires a matching current-source/binary/profile provenance sidecar. Its report/inventory is labeled selected-feedback and cannot claim full-suite acceptance; any failed/incomplete selection exits nonzero after retaining its report. Ordinary campaigns retain full-population accounting. Debug mode compares normalized outcomes to release. `--watch` validates modes in its parent, hashes selection/retained-source/worker inputs, continuously debounces settled edits and propagates an unchanged-state child failure; a changed source makes an in-flight report stale. Negatives: empty/unknown/duplicate/traversal selection, selected byte/metadata tamper, stale manifest/archive/pin, stale worker provenance, stale source, and deliberately wrong result all fail. Functional acceptance: selected debug+release pair with `--compare-release`, followed by the ordinary release runner on the same list. Performance: after the release worker build, create its required current provenance sidecar with `python3 scripts/run_upstream.py --write-worker-provenance target/release/duckdb-rust-test-worker`; `measure_upstream_feedback.py` then performs three warmups and 21 serial **warm-cache** samples against both source-mapped pinned C++ unittest binaries; timed Rust feedback includes selected-cache validation, worker launch, unchanged assertions and report emission. The one-time fresh-cache materialization is retained separately as caller-visible `cold_setup` evidence because C++'s selected runner has no analogous extraction step. Raw wall time, throughput, CPU, peak RSS and block I/O evidence stays under `target/`; every warm metric is gated against the faster pin, while wrong pins/binaries/results fail closed. Status: **open** until its quiet-host command passes. | [P: at-parity-or-better-performance](#at-parity-or-better-performance) |
| G01.4a correction | Development selected-feedback now reads pinned development Git objects and verifies their selected bytes against the pinned retained-manifest digest/per-file hash; it no longer hashes or reads the 100 MiB retained archive on every warm feedback invocation. Full-suite campaigns retain archive/full-verify accounting. | — | — |
| G01.4a final status | The integrated selected debug/release and ordinary campaigns pass both pins on executable revision `ed7e62d`; the 45-test Python harness suite is green. | Gate P passed for wall, CPU, peak RSS, block I/O and throughput; this supersedes the earlier open status. |
| G01.2d.1 — typed native-probe comparison | T; `scripts/verify_reference.py` comparison helper/tests, coordinate oracle owner | HUGEINT number/string representation compares by declared type; changed value, NULL and VARCHAR numeric text still fail. Rerun development probe to expose its next genuine boundary. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G03.3a — ENUM range boundary | T; `src/function/enumeration.rs`, enumeration component/reference cases | Port both pins' constant/column/NULL endpoint behavior; `enum_reference.py` SQL 29/29 per pin plus existing native paths, scalar/batch and prepared coverage. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G04.2a — STRUCT date construction | T; temporal function owner | Match field binding/NULLs, checked INT64-to-INT32 errors and calendar errors through scalar, batch and prepared execution. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G06.1a — simple Unicode case conversion | T; text-function owner | Match `lower`/`upper` and aliases with the pins' exact utf8proc 2.9 / Unicode 15.1 table through scalar, batch and prepared execution. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G07.3a — DISTINCT ON | T; relational binder/executor owner | Match typed target de-duplication, NULL equality, ORDER-selected survivors, aliases, nested and prepared execution. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G08.1a — numeric product | T; aggregate owner | Match DOUBLE multiplication, NULL/empty groups, IEEE values, grouping and BIGNUM conversion; retain the source DOUBLE result type. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G10.4a — verification/settings controls | S; settings owner with binder integration lead | Source-map `enable_verification`, profiling and force-external settings before implementation; assert the promised setting changes execution/verification behavior. Rerun affected unchanged files, not merely their first PRAGMA. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G11.3a — ADD COLUMN execution cost | S; ALTER/default-demand owner, no concurrent G09/G11 shared edits | Preserve stored/default demand, rollback/holes and native exchange tests; profile source-matched ADD and rerun its full 12-case native manifest against both pins. | [P: at-parity or better performance](#at-parity-or-better-performance) |
| G16.3a — aggregation/numeric execution cost | S; aggregation/expression owner, coordinate G03 casts | Preserve checked arithmetic/NULL/error and scalar/batch semantics; rerun numeric+grouping manifests with all per-workload <=1.0 gates. Split by identified source algorithm after diagnosis. | [P: at-parity or better performance](#at-parity-or-better-performance) |

**G01.4a resume checkpoint (2026-09-16).** The compiled selected-feedback
campaign now initializes its per-workload cache map before timing, with a mocked
end-to-end `main()` regression covering both pins and 24 Rust invocations at the
minimum accepted 3-warmup/9-sample shape. The focused runner/measurement suite is
27/27. For the compiled Gate P path, build `target/release/sqllogictest` and use
`scripts/run_upstream.py --write-feedback-provenance
target/release/sqllogictest`; this supersedes the earlier worker-sidecar command
in the table row. The previous frozen 21-sample compiled report passed every
wall/CPU/RSS/I/O/throughput metric, but final integrated functional, performance
and completion-sweep evidence remains required after this manifest update.

No engine behavior was changed by this measurement slice. Tooling uses focused
Python regression tests (wrong results, identity/selection errors, retries, fixture
eligibility and record accounting). The final integrated tree also requires the
delegated chunk sweep; its outcome is reported in the handoff, not inferred from
these upstream measurements.

**G01.4a integrated acceptance (executable revision `ed7e62d`, 2026-09-16).** The selected
debug/release comparison and ordinary runner pass the exact retained path on both
pins (`target/final-g01-debug-release-ed7e62d.json` and
`target/final-g01-ordinary-release-ed7e62d.json`). The quiet
3-warmup/21-sample compiled campaign in
`target/final-g01-feedback-performance-ed7e62d.json` passes every faster-pin
gate: wall ratios are 0.746/0.744, CPU 0.75/0.75, peak RSS 0.266/0.266 and
block I/O 1.0/1.0; both Rust throughputs exceed the faster reference. Cold-cache
setup, exact pin identities, the Rust source/binary hashes and raw samples remain
in that report.

## G02 — Parsing, binding, namespaces and diagnostics

**Current:** the vendored sqlparser frontend and Rust binder support substantial
SQL, but many AST variants/modifiers end in explicit Unsupported.

- **G02.1 Complete syntax coverage.** Compare upstream statement/expression grammar
  with parser and binder handling; implement DuckDB-specific syntax and aliases.
  Reject unsupported modifiers explicitly instead of accepting and ignoring them.
- **G02.2 Complete scope rules.** Handle catalog/schema/table qualification, aliases,
  star EXCLUDE/REPLACE/RENAME and COLUMNS expressions, alias reuse, correlated names,
  struct-versus-qualified-column ambiguity and named/default function arguments.
- **G02.3 Complete binding context.** Preserve literal versus typed-constant versus
  parameter identity, common types, overload costs, qualified calls and dependencies.
  Add SQL PREPARE/EXECUTE/DEALLOCATE lifecycle where the API already has preparation.
- **G02.4 Match diagnostics.** Preserve error categories, query positions/lengths,
  candidate signatures and reference-required message content; keep missing engine
  capability distinct from a supported operation's legitimate rejection.

**Exit:** selected grammar families pass unchanged parser/binder tests with exact
names, schemas and error behavior, including prepared and nested expressions.
Sources: `src/parser/`, `src/planner/binder/`, upstream `src/parser/`,
`src/planner/binder/` and `test/sql/`.

## G03 — Scalar types, coercion and numeric functions

**Current:** signed/unsigned widths, DECIMAL, FLOAT/DOUBLE, BIT, BIGNUM, BLOB,
UUID and anonymous/catalog-named ENUM have implementations. Checked factorial and signed
BIGINT/HUGEINT GCD/LCM families, including aliases and selected casts, now work
through scalar/batch evaluation, mutations and reopen. A second numeric slice adds
the pinned trigonometric/hyperbolic, angle, exponential, cube-root, `even`, `pi`,
`signbit` and `nextafter` families, including strict-IEEE signed-NaN behavior. The
generated math tail now also includes power/log/square-root/bit-count, `gamma`,
`lgamma`, development-authoritative `binom`, exact FLOAT/DOUBLE `isnan`, and the
callable `**`, `^`, `!__postfix` and `@` aliases. Quoted and `main`-qualified scalar
calls share ordinary binding. Binary `unbin`/`from_binary` and `decode` now cover
partial leading bit groups and pinned strict/replace/ignore malformed-UTF-8 behavior,
including deferred mode validation and provable-NULL demand. SQL operator spelling
for exponentiation/postfix factorial, the conversion matrix and the remaining
scalar-family catalog stay open.
Rounding now preserves the pins' bind-time default-NULL demand: a provably constant
NULL argument suppresses failing siblings with the selected result metadata, while
row-sourced NULL retains ordinary source-order demand. Decimal known-NULL calls keep
their distinct NULL result metadata.
G03.3a now ports `enum_range_boundary` through a source-shaped scalar batch hook,
including casts, scalar/binary parents, predicates, lazy selected branches,
shared projections, constant/column/NULL endpoints and prepared execution; its
final-tree independent suite passes 29/29 SQL plus six native paths against each
pin. The function-family **at-parity or better performance** gate passes (wall
0.507x, CPU 0.500x and RSS 0.888x the faster pin, with equal block I/O).

- **G03.1 Close the conversion matrix.** Cover source/target types, literals,
  implicit/explicit/assignment/combination casts, overflow, rounding, textual forms,
  typed NULLs, mixed widths and decimal scale across every consumer.
- **G03.2 Complete numeric operations and catalog.** Inventory upstream overloads,
  aliases, constants, arithmetic/bitwise/math/rounding functions and BIGNUM/BIT
  operations. Extend existing registrations; verify return types and boundary errors.
- **G03.3 Complete scalar-family edges.** Finish binary/UUID/ENUM functions, coercion,
  formatting and batch-dependent behavior. Coordinate named types with G10 and
  remaining physical type/version/compression coverage with G12.
- **G03.4 Preserve context and mixed-family semantics.** Match IEEE-setting behavior
  for already prepared versus freshly bound expressions, lazy errors, unsigned keys,
  DECIMAL inside nested values, and scalar/batch results with retained adapters.

**Exit:** mapped family cases pass through SQL, parameters, joins/groups/order,
indexes, mutations and native reopen; a selected arithmetic matrix alone is insufficient.
Sources: `src/common/{types,numeric,scalar,bit,bignum}.rs`, `src/common/cast/`,
`src/function/scalar/numeric/`, upstream `extension/core_functions/`.

## G04 — Temporal and timezone completeness

**Current:** DATE, TIME/TIME_NS/TIMETZ, timestamp precisions/timezones and INTERVAL,
plus selected arithmetic, calendar difference/truncation/bucket/format functions.
`to_timestamp(DOUBLE)` retains ties-even microsecond rounding and its half-open
range; development's interval normalization carries and borrows with Euclidean,
saturating behavior. ISO/BCE-aware `era`, `isoyear`, `week`/`weekofyear`, `weekday`,
`yearweek` and `julian` extraction now works through named and generic aliases, with
pinned NULL-overload and invalid-specifier ordering. Core `timezone`,
`timezone_hour` and `timezone_minute` cover fixed-offset TIMETZ and zero-offset
DATE/TIME/TIMESTAMP behavior; unsafe offsets outside +/-15:59:59 are rejected
instead of constructing invalid physical values. Core `strptime` and `try_strptime`
accept scalar or constant-list formats, retain named argument order, distinguish
parse from conversion fallback, and match pinned nanosecond sentinel boundaries
through defaults and native reopen. `get_current_timestamp()`, `now()`,
`transaction_timestamp()` and bare/quoted `CURRENT_TIMESTAMP` read one replaceable
clock sample taken before snapshot acquisition and retain that TIMESTAMPTZ through
an explicit transaction. Column, alias and table-as-STRUCT binding wins before the
SQL-value fallback; the keyword has its exact bounded native parsed-node lifecycle.
The remaining parsing/format catalog, local/current calendar functions, named-zone
and ICU work stays open.
G04.2a now supports `make_date(STRUCT(year, month, day))` with case-insensitive
and reordered fields, NULL propagation, checked INT64-to-INT32 field errors before
calendar validation, scalar/batch and prepared
execution. Dictionary-encoded STRUCT batches are converted once per used physical
value, and proven-total COUNT/SUM states retain column updates without changing
row-ordered failure behavior for other aggregates. The temporal component passes
its assigned cases, and this slice's **at-parity or better performance** gate passes
(wall 0.762x, CPU 0.500x and RSS 0.591x the faster pin, with equal block I/O).

**G04.1a validation manifest — temporal physical minima audit.** This batch owns
only this maintained backlog entry: it audits the already implemented minimum
domain without changing executable code, fixtures, build configuration or a
workload. The focused Rust check is `cargo test -p duckdb-rust --test temporal
temporal_minimum`; the cross-pin acceptance command is `python3
scripts/temporal_minimum_reference.py --report target/next-batch/g04/<revision>.json`.
The report must retain every exact and outcome-only difference for both pinned
C++ revisions, including minimum timestamp precision conversion, `date_trunc`,
missing release `TIMESTAMPTZ_NS`, diagnostic wording and native payload limits;
a nonzero comparison is evidence of the remaining G04.1 backlog, not a pass.
Negative/boundary coverage includes all timestamp precisions and timezone forms,
the full-width minimum, conversion rounding and formatting. Performance is **not
applicable: documentation-only** for this batch after review of its final diff;
that is not a measured temporal performance pass and does not close the residual
engine gaps.
The final audit report is
`target/next-batch/g04/temporal-minimum-c958074.json`: all three focused Rust
tests, both 30-value C++ API fixtures and all eight native round trips pass.
Release matches 20/32 SQL results exactly and 29/32 by outcome; development
matches 23/32 exactly and 32/32 by outcome. The retained differences are the
declared precision-rounding, release `date_trunc`/`TIMESTAMPTZ_NS`, and diagnostic
text gaps, so G04.1 remains open rather than being relabeled complete.

**G04.1b validation manifest — development timestamp minimum precision
conversion.** This executable slice owns `src/common/cast/temporal.rs`,
`src/common/temporal.rs`, `src/function/temporal.rs`,
`test/component/temporal_minimum.rs`, `benchmark/g04_1b_temporal_workloads.json`, and this
entry. It freezes development's scale-first/ties-away-from-the-epoch conversion
for finite `TIMESTAMP_NS`, `TIMESTAMPTZ_NS`, `TIMESTAMP`, `TIMESTAMPTZ`,
`TIMESTAMP_MS`, and `TIMESTAMP_S` minima/boundaries, including the full signed
minimum payload and the development overflow category. Fast feedback is `cargo
test -p duckdb-rust --test temporal temporal_minimum`; the exact retained upstream inputs
are the six `MINIMUM` construction cases and 26 derived expressions in
`scripts/temporal_minimum_reference.py` (`SQL`, both pins), especially
`epoch_us(t::TIMESTAMP)`, `t::TIME`, and precision casts of
`make_timestamp_ns(-9223372036854775808)`. Unchanged cross-pin/native IDs are
the release C++ API fixture's 30 values, the development fixture's 36 values and
eight checkpoint/WAL round trips. Negative/boundary/shared-consumer coverage includes
non-renderability, infinities, `i64::MIN`, `-i64::MAX`, pre-epoch half ties,
overflowing precision expansion, scalar/batched evaluators, prepared parameters,
direct all-valid flat `TIMESTAMP_S/MS/NS/TZ/TZ_NS` batch casts, NULL, selected
dictionary and constant vectors, VARIANT/STRUCT/LIST consumers, unique indexes
and checkpoint/WAL reopen. Final
functional commands are `cargo test -p duckdb-rust --test temporal temporal_minimum`,
`python3 scripts/temporal_minimum_reference.py --report
target/next-batch/g04/g04-1b-temporal-minimum.json`, `cargo dev coverage`, and
`cargo dev trace check --workspace --all-targets`. The declared process workload
is `temporal_minimum_precision_casts` in `benchmark/g04_1b_temporal_workloads.json`: a
50,000-row exact-divisibility representative vector conversion scan, not a
rounding-boundary oracle (those semantics live in the focused functional tests). It is run
release/no-tracing with `scripts/compare_native.py` for both exact pins and
`scripts/fastest_reference.py`; wall median must be <= faster pin, throughput >=
faster pin, and independent CPU, peak RSS and read/write-I/O ratios must each be
<= 1.0. The current `compare_native.py` adapter records wall only, so CPU/RSS/I/O
require a retained companion measurement; until all five gates are measured and
pass, performance is open. Release's truncating minimum narrowing, missing
`TIMESTAMPTZ_NS`, and `date_trunc` difference remain documented divergences, not
acceptance baselines.

**G04.1b outcome (integrated executable revision `247079f`):** `cargo test -p
duckdb-rust --test temporal temporal_minimum` passes all five selected tests,
including scalar, batch, prepared, direct-vector and encoded-vector coverage.
The final cross-pin report is
`target/next-batch/g04/temporal-minimum-final-integrated.json`: development
matches 23/32 SQL results exactly and all 32 by outcome, release matches 20/32
exactly and 29/32 by outcome, and the release 30-value and development 36-value
C++ API fixtures and all four round trips per pin pass. The remaining release
precision, `date_trunc` and `TIMESTAMPTZ_NS` differences are the declared
residual G04.1 backlog.
`cargo dev coverage` reports no missing annotations and `cargo dev trace check
--workspace --all-targets` passes. The final 21-sample no-tracing reports are
`target/next-batch/performance/g04/g04-1b-integrated-{release,development,fastest}.json`;
both retained Rust runs pass against the faster pin. The companion resource
report is `target/next-batch/performance/g04/g04-1b-integrated-resources.json`;
wall, user/system CPU, peak RSS, block input/output and throughput pass
independently. This completes the G04.1b slice, not the remaining G04.1 domain.

- **G04.1 Finish physical and textual domains.** Cover minima/maxima, infinities,
  fractional rounding, offset limits, precision loss, interval forms, native/API
  payloads and exact cast failures; retain the repaired full-width timestamp minimum.
- **G04.2 Finish calendar functions.** Inventory extraction, construction, formatting,
  parsing, date arithmetic, series and current-date/time functions; match constant
  versus column execution, NULLs, errors and transaction-stable time semantics.
- **G04.3 Add timezone/ICU behavior.** Provide timezone settings, named zones, DST
  gaps/folds, calendar configuration and timezone conversions. Provision matching
  ICU reference builds; the current core-only builds cannot validate that population.
- **G04.4 Integrate every path.** Exercise mixed temporal/nested values, prepared
  settings, casts/defaults, aggregates/windows, indexes and native WAL/checkpoints.

**Exit:** complete mapped temporal and ICU populations pass separately against the
required configurations; exact diagnostic differences stay visible.
Sources: `src/common/temporal/`, `src/function/temporal/`,
[temporal contracts](implementation-notes.md#temporal-and-numeric-behavior), upstream `extension/icu/`.

## G05 — Nested values, lambdas and core GEOMETRY

**Current:** LIST/ARRAY/STRUCT/MAP/UNION/VARIANT/TUPLE have selected value, function
and native paths. LIST/ARRAY slicing now implements pinned 1-based inclusive,
negative, omitted-bound and stride semantics through both evaluators and reopen;
omitted syntax is retained as binder provenance and cannot be forged by a user
empty-list expression. LIST/ARRAY contains/position/select/resize/reverse families
now preserve ARRAY-to-LIST results, pinned NULL demand and bounded allocation.
LIST/ARRAY `where` masks and variadic `zip` additionally preserve long-mask NULL
padding, truncate-to-shortest, heterogeneous tuple children and NULL-list behavior
through scalar/batched execution, Rust-origin retained defaults and materialized
native values from both producer directions. Rust intentionally rejects a sequence
or shallow zipped expansion above 16,777,216 logical children before allocation;
the pinned implementation has no
equivalent fixed ceiling. The shared multi-family `contains` name, higher-order/lambda
functions and the broader nested catalog remain open. LIST/ARRAY `has_any`, `has_all`
and `intersect` families now include the pinned array aliases and `&&`, `@>`, `<@`
operators, common-child coercion, outer/child NULL behavior and duplicate removal.
Intersection retains the pinned shorter-side hash/probe ordering and a representative
from the left input rather than promising stable left order. Contextual keys propagate
through nested children and preserve physical INTERVAL components; representation-
sensitive VARIANT membership remains explicitly unsupported. Stable LIST/ARRAY sort
and grade-up aliases honor explicit or session-default order/NULL order, selected
child comparison, named argument reordering, ARRAY-to-LIST results and bounded
cancellation through scalar/batched execution. Retained native constructors
qualified as `main.list_value` now resolve through the same bounded built-in lookup
as ordinary `main`-qualified calls. Core GEOMETRY is present in the pinned C++ type
enum and absent from Rust's built-in DataType enum; it is not solely a
spatial-extension question.

- **G05.1 Complete nested semantics.** Finish slicing, constructors/accessors,
  UNION promotion, STRUCT field combination, ARRAY shapes, MAP duplicates/lookup,
  VARIANT dynamic semantics, TUPLE behavior and formatting with child NULL/type identity.
- **G05.2 Add higher-order operations.** Implement lambda binding/captures and the
  list/array/map function catalog, including transforms, filters, reductions and
  applicable mathematical operations; preserve selected child adapters and effects.
- **G05.3 Finish relational and mutation consumers.** Cover recursive UNNEST,
  lateral use, aggregates, qualified paths, nested mutations, prepared values and
  full-width literal inference in mixed containers.
- **G05.4 Add missing logical families.** Implement core GEOMETRY/WKB/CRS behavior,
  applicable casts/operations and native metadata. Inventory TYPE and other exposed
  type-constructor/pseudo-type behavior; do not turn internal-only IDs into columns.
- **G05.5 Finish nested persistence.** Cover native child streams, shredded VARIANT,
  update paths, named child types, defaults and version restrictions with independent
  bidirectional files. Preserve nested IEEE bits and NULL-versus-active-NULL tags.

**Exit:** mapped nested and core geometry behavior works end to end; arbitrary
opaque payload storage is not equivalent to the reference type semantics.
Sources: `src/common/{nested,variant}.rs`, `src/function/nested/`,
`src/storage/duckdb/nested/`, upstream `src/include/duckdb/common/types.hpp`.

## G06 — Text, collations and utility functions

**Current:** selected scalar/string operations and LIKE exist; `lower`/`upper` and
their `lcase`/`ucase` aliases now have a bounded source-matched case-conversion
slice with scalar, batch and prepared coverage. It uses the pins' byte-identical
utf8proc 2.9 / Unicode 15.1 table rather than host or current-Unicode full case
mappings. Its Rust FFI property layout and exported symbol set also match that
pinned header, including the full 16-bit combination index. The full upstream
function catalog and collation system do not. Its **at-parity or better
performance** gate passes (wall 0.324x, CPU 0.000x at timer resolution and RSS
0.468x the faster pin, with equal block I/O).

**G06.1c frozen validation manifest — VARCHAR search (`instr`, `strpos`, callable
`position`, and `POSITION(needle IN haystack)`).** Owned paths are
`src/function/scalar/text.rs`, `src/function/scalar.rs` only if registration needs
it, `src/planner/binder/expression.rs`, `test/component/text_search.rs`, the G06
search SQLLogic fixture, `benchmark/g06_1c_search*_workloads.json`, and this
entry. Fast checks are `cargo test -p duckdb-rust --test text_search`, the
scalar text unit module, and focused flat/constant/dictionary/selected and prepared
filters; the shared consumers are batched expression evaluation and a fused
`sum(instr(...))` aggregate. The unchanged upstream paths selected exactly against
both pins are `test/sql/function/string/test_instr.test` and
`test_instr_utf8.test`, plus the newly exposed `instr` prefixes of
`test_substring{,_utf8}.test`. The later `instr` record in `null_byte.test` is an
explicit unreached case: file-granularity selection stops at the earlier
unimplemented `chr(0)` record. Separately unimplemented `substring_grapheme`,
`chr`, `contains`, and regex records remain visible rather than counted as
G06.1c passes. Boundary/negative coverage includes Unicode character positions,
keyword-case-insensitive POSITION syntax and argument reversal, empty
needle/haystack, no match, embedded NUL, NULLs, invalid overloads,
constant/flat/dictionary/selected batches, prepared execution and scalar/batch
error order. Full functional acceptance runs the listed upstream paths with
`scripts/run_upstream.py --path-list` for each pin plus the complete text-search
target and relevant batched-expression target. Gate P declares native 50,000-row
low-cardinality fused and direct search workloads plus a process
SQLLogic workload with repeated aggregate consumers. Final release/no-tracing
measurements require three warmups and 21 serial samples against both exact pinned
C++ identities; wall latency/throughput, CPU, peak RSS and block I/O each gate
independently against the faster pin, with raw identities, hashes and samples under
`target/`.
**G06.1c final status (integrated executable revision `247079f`).** The
implementation registers callable `instr`, `strpos`, and `position` with
byte-preserving search and one-based Unicode character positions across scalar and
physical batch encodings. The binder lowers SQL's reserved
`POSITION(needle IN haystack)` AST to the same haystack/needle call contract;
the complete component target and focused scalar batch test pass. The exact
dual-pin report is `target/next-batch/g06/upstream-final-integrated.json`.
Development passes the complete `instr` 15/15 and `instr_utf8` 13/13 files;
release passes them 16/16 and 14/14. The selected substring files reach 44 and
13 records on development, and 42 and 12 on release, before the separately
unimplemented `substring_grapheme`; the NUL file stops at the earlier `chr(0)`
blocker, leaving its later 22 `instr` records explicitly unreached.
The final native reports are
`target/next-batch/performance/g06/final-{release,development,fastest}-21.json`:
both retained Rust runs pass against the faster pin for fused `instr` and
projected `strpos`. The companion process report is
`target/next-batch/performance/g06/final-process-21.json`; wall, CPU, peak RSS,
block input/output and throughput pass independently. This completes G06.1c;
`substring_grapheme`, `chr`, `contains` and regex work remain later G06.1 slices.

**G06.1b frozen validation manifest — VARCHAR length and substring.** Owned paths
are `src/function/scalar{.rs,/text.rs}`, the shared vector/batch/cast seams changed
by substring composition, `test/component/{text_substring,casts}.rs`, the G06
SQLLogic fixture, native/process workload manifests and benchmark adapters, and
this entry. Fast checks are the complete `text_substring` target, the scalar text
unit module, the focused length/dictionary/selected-lane filters and
`primitive_integer_overflow_reports_physical_types`. Full functional acceptance
also includes the complete grouping target because fused `sum(length(substring))`
is a shared consumer, plus the unchanged upstream files
`test/sql/function/string/test_length.test`,
`test/sql/function/string/test_substring.test` and
`test/sql/function/string/test_substring_utf8.test`, selected exactly against
both pins. Prefix failures at the separately unimplemented `instr` and
`substring_grapheme` families remain visible and cannot be claimed as substring
failures or full-file passes. Boundary coverage includes omitted/negative/zero
starts, omitted/negative lengths, empty/NUL/Unicode values, INT64 extremes,
overflow diagnostics, constant/flat/dictionary/selected encodings, prepared
execution and low/high-cardinality direct VARCHAR output. Gate P uses three
50,000-row native cases (fused low-cardinality length, direct low-cardinality
substring and direct high-cardinality substring) and the maintained SQLLogic
process workload. The comparable process fixture uses the same stored
50,000-row configuration and repeats two one-row aggregate consumers 128 times,
for 6.4 million relevant row-visits; direct VARCHAR transport remains in the
native gate so peak memory does not compare Rust row objects with C++ vectors.
Final release/no-tracing evidence uses three warmups and 21 serial samples against
both exact pins, with exact checksums and independent wall, CPU, peak-RSS,
block-I/O and throughput gates against the faster reference.
The implementation now evaluates `length` and character-indexed `substring`/
`substr` through flat, constant, dictionary and selected physical batches,
including fused aggregate consumers, without changing scalar error order. On
revision `aa89bbc`, the local G06 fixture passes 5/5. The exact upstream length
files pass 6/6 development and 7/7 release. Substring reaches 27 records on
development and 28 on release before the separate missing `instr` family;
UTF-8 substring reaches 13 and 12 respectively before the separate missing
`substring_grapheme` family. These are retained prefix results, not full-file
passes. Final-tree native reports are retained as
`target/next-batch/performance/g06/final-accepted-{release,development,fastest}-21.json`;
the comparable CPU, peak-RSS, block-I/O and throughput campaign is
`target/next-batch/performance/g06/final-accepted-process-21.json`. Both pinned
campaigns, the joint faster-reference latency gate and every independent process
metric pass for all declared workloads. This completes the G06.1b slice, not the
remaining G06.1 catalog.

- **G06.1 Finish text functions.** Implement length/substrings/search/replace/split,
  Unicode case and normalization, formatting/padding, encodings and relevant aliases.
  Match character versus byte indexing and invalid-input behavior.
- **G06.2 Add regex and collation contracts.** Match regex options/results/errors;
  support built-in and ICU collations across comparisons, sorting, grouping, joins
  and index keys. A custom ASCII test type is not a production collation system.
- **G06.3 Complete utilities.** Inventory random/seed/UUID generation, hashing,
  conversion/introspection helpers and other core scalar functions. Declare volatility,
  transaction/statement stability and external effects in binding/execution.
- **G06.4 Publish coherent catalog metadata.** Ensure aliases, named parameters,
  overload descriptions and qualified function lookup match G10 introspection and
  G02 binding; test error/effect behavior under folding, CASE and TRY-like consumers.

**Exit:** source-enumerated function families have correct registrations and behavior,
including Unicode, NUL, empty input, NULLs and retained prepared bindings.
Sources: `src/function/`, upstream `extension/core_functions/scalar/`,
`src/function/scalar/`, `extension/icu/`.

## G07 — Remaining relational SQL

**Current:** ordinary joins/NATURAL/USING, basic set operations, scalar/EXISTS/IN
subqueries and recursive UNION are implemented. G07.3a additionally implements
`DISTINCT ON` target de-duplication, typed NULL equality and source-shaped ORDER
selection through aliases, nested queries, prepared and batched execution. Its
assigned execution cases and **at-parity or better performance** gate pass (wall
0.557x, CPU 0.500x and RSS 0.909x the faster pin, with equal block I/O). This is
completion work, not a rewrite.

- **G07.1 Finish join and correlation forms.** Add lateral/dependent relations,
  ASOF and positional joins, remaining quantified/correlated subqueries and
  correlation through functions/aggregates, preserving empty/NULL/cardinality rules.
- **G07.2 Finish recursive and materialized CTEs.** Implement materialization controls,
  USING KEY/recurring relations, multiple references, recursive naming/type rules
  and cancellation/termination behavior.
- **G07.3 Complete relational syntax.** Finish DISTINCT ON, GROUP BY ALL, remaining
  SELECT/ORDER/LIMIT modifiers and set-operation alignment such as BY NAME where
  present in the pin; verify interactions with aliases and qualified columns.
- **G07.4 Add reshape and sampling.** Implement PIVOT/UNPIVOT and sampling forms,
  schema discovery, output naming, NULL handling and reproducibility settings.

**Exit:** each query family passes unchanged SQL cases and mixed prepared/nested/
transaction scenarios; neither a parsed AST nor a single join algorithm is enough.
Sources: `src/planner/binder/{query,table,recursive,subquery}.rs`,
`src/execution/operator/`, upstream query-node/table-reference tests.

## G08 — Aggregates and windows

**Current:** ten ordinary aggregate names and eleven dedicated window names are
registered in their built-in modules; grouping sets and core frames already work.
G08.1a adds unary numeric `product`, including NULL/empty behavior, IEEE DOUBLE
multiplication and BIGNUM conversion coverage; its source `product` cases pass
against both pins. Its **at-parity or better performance** gate passes (wall
0.842x, CPU 1.000x and RSS 0.280x the faster pin, with equal block I/O).

**G08.2a frozen validation manifest — aggregate argument `ORDER BY`.** Owned paths
are the aggregate binder/logical plan and grouped/batched execution modules,
`test/component/grouping.rs`, the G08 SQLLogic fixture, native/process workload
manifests and benchmark adapters, and this entry. The fast behavior check is the
complete grouping target. It covers order-insensitive SUM elision; FIRST/LAST
with ASC/DESC, NULL ordering, equal-key source stability, grouped/ungrouped and
multi-key inputs; filters/DISTINCT; scope and metadata errors; effects/fallible
expressions; resource/cancellation bounds; mixed ordinary and ordered states;
and conservative fallback outside the physical candidate capability. Full
functional acceptance runs the local G08 fixture and the unchanged upstream
`test/sql/aggregate/aggregates/test_sum.test` and
`test/sql/aggregate/aggregates/test_order_by_aggregate.test` files against both
pins. Any later `WITHIN GROUP`, LIST/DISTINCT/FILTER parser or missing aggregate
family blocker remains explicit rather than turning a reachable prefix into a
full-file pass. Gate P uses six 50,000-row native cases: total-key SUM, ungrouped
FIRST/LAST, 8,192- and 64-group FIRST/LAST, mixed SUM plus ordered candidates and
composite ordering; the process gate uses the maintained SQLLogic workload. The
comparable process fixture repeats ungrouped, 8,192-group and mixed
ordered consumers over the same stored 50,000 rows 128 times, for 19.2 million
relevant row-visits and one-row results; the separate one-million-row fixture
remains functional acceptance for generated-expression integration. Final
release/no-tracing evidence uses three warmups and 21 serial samples against both
exact pins, validates declared results, and independently gates wall, CPU, peak
RSS, block I/O and throughput against the faster reference.
The implementation binds aggregate argument ordering with scope validation,
elides only pure total keys for order-insensitive functions, preserves pinned
FIRST/LAST tie behavior, and uses bounded batched candidates for eligible integer
grouping while retaining the generic ordered driver as fallback. Revision
`881c4b2` additionally keeps all-valid BIGINT arithmetic in native lanes and
propagates a single-pass ascending proof so monotonic dense grouping avoids a
redundant range scan. On revision `aa89bbc`, the local workload passes 4/4 and
the full unchanged
`test_sum.test` passes 19/19 on both pins. `test_order_by_aggregate.test` reaches
13/14 attempted records on development and 14/15 on release before the unrelated
LIST/DISTINCT/FILTER parser case; that prefix is not a full-file pass. Final-tree
native reports are retained as
`target/next-batch/performance/g08/final-accepted-{release,development,fastest}-21.json`;
the comparable CPU, peak-RSS, block-I/O and throughput campaign is
`target/next-batch/performance/g08/final-accepted-process-21.json`. Both pinned
campaigns, the joint faster-reference latency gate and every independent process
metric pass for all six native cases and the declared process workload. This
completes the G08.2a slice while the remaining G08.2 modifiers stay open.

**G08.2b frozen validation manifest — aggregate `FILTER`/`DISTINCT`
composition.** Owned paths are the aggregate-call parser and binder, aggregate
grouped/batched execution only where composition requires it,
`src/common/vector.rs` and its in-file physical-vector tests,
`src/execution/operator/aggregate/batched/index.rs` and its in-file tests,
`test/component/grouping.rs`, the focused G08 SQLLogic and performance fixtures,
their native/process workload manifests, and this entry. Fast checks are
`cargo test -p duckdb-rust --test grouping` and
`cargo run --offline --bin sqllogictest -- test/sql/g08_filter.test`.
The exact unchanged upstream IDs for both the release and development pins are
`test/sql/aggregate/aggregates/test_order_by_aggregate.test` and
`test/sql/aggregate/aggregates/test_simple_filter.test`, selected separately with
`python3 scripts/run_upstream.py --target <release|development> --path-list ...`.
Coverage includes grouped and ungrouped shorthand `FILTER(expr)` plus standard
`FILTER(WHERE expr)`; false, NULL and all-filtered predicates; duplicate and NULL
arguments; `DISTINCT` with stable argument ordering for LIST/FIRST/LAST and
ordinary COUNT/SUM; the DISTINCT-ORDER-BY argument-list diagnostic before filter
binding; invalid/non-Boolean/out-of-scope predicates; prepared and batched use;
mixed aggregate consumers; and cancellation/resource limits. Missing aggregate
families such as STRING_AGG, histogram and mode stay explicit blockers rather
than broadening this slice. Full functional acceptance runs the complete
component grouping target, the local G08 SQLLogic fixture, and both unchanged
files against each exact pin. Gate P declares 50,000-row, single-thread,
release/no-tracing native workloads for filtered COUNT/SUM, filtered DISTINCT
LIST with argument ORDER BY, and grouped filtered FIRST/LAST with duplicate and
NULL inputs; the process workload repeats their stored-column equivalents with
one-row checks. Three warmups and 21 serial samples must validate results against
both pinned C++ references. Every native wall-time ratio and process wall,
throughput, CPU, peak-RSS and block-I/O ratio must be no worse than the faster
pin independently; raw samples, identities and failed runs remain under
`target/next-batch/performance/g08/`.

**G08.2b final status (2026-09-17).** Integrated executable revision `185092b`
passes the 68-test grouping target and all 5 focused local SQLLogic records.
Development passes 15/15 `test_order_by_aggregate.test` and 9/9
`test_simple_filter.test` records; release passes 16/16 and 10/10. The final report is
`target/next-batch/g08/upstream-final-integrated.json`. Coverage reports no missing
development instrumentation, and trace compatibility completes with zero
errors, panics or open spans. The final 3-warmup/21-sample native reports are
`target/next-batch/performance/g08/final-{release,development,fastest}-21.json`.
Both retained Rust runs pass the faster-pin gate for filtered COUNT/SUM,
filtered DISTINCT ordered LIST and grouped filtered FIRST/LAST. The process
report `target/next-batch/performance/g08/final-process-21.json` independently
passes wall, CPU, peak RSS, block input/output and throughput, while validating
1,025 reference assertions per pin and 385 Rust records. Both exact reference
identities, validated results, source/binary hashes and all raw samples are
retained in those reports. Gate P passes; STRING_AGG, histogram and mode remain
outside this slice.

The integrated completion sweep also owns the lock-lifecycle repair in
`dev/src/artifacts.rs`. Its fast checks are the Unix retained-descriptor unit
test and the complete `duckdb-dev` artifact integration target. Boundary
coverage includes kept and discarded sessions, early-drop cleanup, oversized
retention, active readers, write-budget failure and descriptors inherited
across `fork`. The unchanged upstream case IDs and G06/G08 acceptance commands
above remain the functional and performance gates: this development-only crate
is absent from the release/no-tracing engine and SQLLogic binaries, so their
recorded source and binary identities are unchanged. The complete final-tree
acceptance command remains `python3 scripts/verify_chunk.py`.

- **G08.1 Complete aggregate families.** Add ordered/list/string aggregates,
  statistical/regression/distribution functions, quantiles, approximate/sketch
  functions, arg-min/max and other source-registered families with exact return types.
- **G08.2 Finish aggregate modifiers.** Support applicable ORDER BY/DISTINCT/FILTER,
  multiargument signatures, empty groups, grouping masks and full grouping-set rules.
- **G08.3 Finish window semantics.** Add value-offset RANGE frames, remaining
  exclusion/dynamic-bound behavior and argument ordering. Inventory additional
  window functions from the pin and verify peers, ties, NULLs and invalid bounds.
- **G08.4 Integrate execution states.** Carry aggregate/window states through
  batching, parallel combination and spill contracts; preserve ownership, effects,
  cancellation and results under different partition/batch boundaries.

**Exit:** unchanged aggregate/window populations pass, including nested/decimal/
temporal inputs and bounded-resource execution where the reference supports it.
Sources: `src/function/{aggregate,window}.rs`, `src/planner/window.rs`,
`src/execution/operator/{aggregate,window}/`, upstream `extension/core_functions/aggregate/`.

## G09 — Retained expressions and default lifecycle

**Status: scoped retained-default functionality implemented; performance acceptance
open.** The measured ADD COLUMN slowdown must be resolved under G11.3a/gate P
before this slice can be called fully complete. Catalog columns and private
snapshots retain optional `StoredExpression` trees, including absence, declared
literal type and syntax provenance. SQL CREATE/SET/ADD captures and binds closed
defaults without executing during capture or binding; INSERT and demanded ADD
backfill evaluate later at their documented demand points. Omitted
INSERT defaults run column-major over each source batch requested with the fixed
2,048-row DuckDB maximum; a short natural child batch remains a boundary, while the
full row set remains staged so failure publishes no partial insert. ADD follows
the pinned development behavior where the references diverge: a constant or one non-TRY
cast around a constant is simple, while v1.5.5 treats only the bare constant as
simple. Non-simple ADD evaluates visible rows only; simple ADD evaluates retained
physical slots, including deleted slots, until checkpoint reclamation.

Physical order follows regular versus relocating updates: indexed-column and
unsupported nested updates retain a deleted old slot and append the replacement.
Successful manual and automatic checkpoints reclaim only the current acknowledged
snapshot; failed checkpoints leave slots, generation and the durable image unchanged,
and older snapshots retain their holes.
Native checkpoint/WAL expression codecs preserve selected function/default metadata,
including absent versus explicit `DEFAULT NULL`, with bidirectional FUNCTION, CASE,
predicate and interval acceptance against the pinned development process. Both release
checkpoint directions pass independently; its two Rust-origin WAL cases containing
FUNCTION nodes retain the known upstream startup failure.
Reference-origin UNBOUND type-expression metadata for retained built-in LIST/ARRAY,
MAP/STRUCT/TUPLE/UNION/VARIANT, DECIMAL and ENUM literals now resolves within the
enclosing codec budgets and reserializes canonically; qualified and extension-owned
types remain explicit catalog-binding work.

- **G09.1 Retain catalog expressions — implemented.** Optional owned expressions
  now retain declared types, aliases/argument provenance, qualification, operators
  and source spans through catalog alteration and private snapshot round trips.
  Closed CASE, NULL-test, BETWEEN, IN, LIKE and interval defaults share ordinary
  binding; conditional ADD no-ops are resolved before type/default binding.
- **G09.2 Connect DDL and evaluation demand — implemented.** Capture
  and bind CREATE/SET/ADD defaults without executing during those stages. Evaluate
  omitted INSERT values column-major per source vector while interleaving source
  effects and preserving short child-batch boundaries. Resolve demanded ADD backfill
  once in the applicable live-only or retained-physical demand order and reuse it
  across catalog-basis, current-snapshot and WAL paths.
- **G09.3 Connect native serialization — implemented representable scope.** Integrate
  parsed/value codecs, unresolved/named type binding, private format, native
  checkpoints and WAL. Selected FUNCTION, CASE, predicate and interval defaults plus
  absence/explicit-NULL metadata pass independent exchange. Reference-origin built-in
  nested literal type expressions resolve without exposing UNBOUND as a column type.
  Reject unrepresentable legacy argument provenance.
- **G09.4 Preserve lifetime/effect semantics — implemented for closed defaults.** Closed
  retained binding permits volatile/external effects. Prepared omitted INSERT uses
  execution-time settings; value versus selection predicate demand matches the pinned
  execution modes. Tests cover source/vector boundaries, failed-statement atomicity,
  CREATE/SET rollback, old catalog snapshots, update relocation, manual/automatic
  reclamation, reopen and deferred error timing. Built-in current-date/local-time and
  timezone/calendar defaults remain with G04. Real sequences/`nextval`, general
  catalog function identity, multi-catalog routing, non-schema dependencies and
  prepared invalidation remain with G10. DML-level explicit `DEFAULT`, CHECK
  constraints and generated columns remain with G11. Those are separate consumer
  scopes and are not claimed by this G09 exit.

**Functional exit achieved for this slice; performance exit remains open:**
the checked-in development fixture covers FUNCTION
checkpoint input, and the independent process gate passes FUNCTION/CASE/predicate
and interval checkpoint exchange in both directions plus Rust-origin WAL recovery. A
one-time evaluated literal or codec-only round trip cannot pass. Release checkpoint
exchange passes separately from its documented upstream FUNCTION-node WAL startup limit.
Sources: `src/catalog/{mod,expression}.rs`, `src/planner/binder/stored.rs`,
`src/storage/duckdb/{parsed,value}/`, [implementation notes](implementation-notes.md#retained-defaults).

## G10 — Catalog objects, settings and attachments

**Current:** the catalog exposes schemas/tables; TableName has schema/name only.
Runtime catalog/object IDs now separate stable object identity from observed catalog
version, legacy adapters fail closed at identity-aware boundaries, a bidirectional
checked dependency graph plans deterministic RESTRICT/CASCADE order, and a pure
search-path model matches pinned parsing and implicit lookup order. These contracts
are now adopted by snapshot schema/table DDL and transaction publication. Runtime
registries rebuild fresh non-wire identities on reopen, enforce table-to-schema
dependencies, preserve identity across rename, reject drop/recreate replacements and
reuse one prepared insertion identity across transaction-current/catalog-basis views.
SQL binding and logical/physical table plans now retain these identities. Query/DML
plans require their exact observed catalog version at logical validation and physical
scan open; stable ALTER/DROP follows rename, rejects replacements, and preserves
`IF EXISTS` without classifying arbitrary catalog failures as absence. Alternative
frontends can resolve the same handles through `Connection::resolve_table`. Prepared
API statements still retain syntax and rebind on each execution. The pure search-path
model is connected to a bounded, normalized session setting: unqualified table lookup
searches configured schemas before `main`, creation uses the current schema, and
prepared statements rebind on each execution. Catalog-qualified path entries fail
explicitly until attachment routing exists. Settings now include operational
`enable_verification`, `debug_force_external`, `enable_profiling`/`enable_profile`,
`profiling_mode` and `profiling_output`/`profile_output`, with bare Boolean PRAGMA
forms, result comparison and bounded JSON/text profile output. Verification is an
operational release-compatible superset of the development pin's deprecated no-op;
forced-external execution remains explicitly unavailable because no spill-capable
operator exists. Profiling mode/enable/disable/reset now share the pinned effective
state: renderer enablement exposes the default `standard` mode and disabling
suppresses output after a prior mode change. The profiling
`no_output` workload has **at-parity or better
performance: pass** at wall 0.370x, CPU 0.000x at timer resolution and RSS 0.615x
the faster pin, with equal block I/O. G10.4a remains functionally open because
verification is incomparable with that development no-op and forced-external
execution is absent. There is no general catalog object model. Catalog-named ENUM types now have
transactional CREATE/REPLACE/DROP and SQL binding, preserve the dictionaries of
already-bound table columns across replacement and name removal, and survive native
checkpoint and WAL handoffs with both pinned C++ revisions. Storage version 69 uses
the pinned qualified schema/type WAL fields; Rust still limits schema qualification
to its top-level schema model. Preparation is still syntax-only and does not yet own
the native prepare transaction/start timestamp or a retained plan.

- **G10.1 Establish identity and dependencies.** Add catalog/object IDs, search paths,
  dependency tracking, invalidation, temporary object scope and transaction visibility.
  Preserve prepared object lifetimes and reference rename/drop/cascade rules.
- **G10.2 Implement object families.** Add views, scalar/table macros, sequences,
  remaining user-defined types, aliases and applicable newer object families such
  as triggers from the pinned source. Extend the implemented named ENUM slice where
  those objects require additional DDL, binding or persistence lifecycle behavior.
- **G10.3 Add attachments and routing.** Implement ATTACH/DETACH/USE and qualified
  access to multiple catalogs, read-only modes, storage extensions and connection
  shutdown. Match cross-database write restrictions instead of promising atomicity
  merely because two stores can be attached.
- **G10.4 Complete metadata and configuration.** Implement duckdb_* tables/functions,
  information_schema/compatibility catalogs, SHOW/DESCRIBE, settings/PRAGMAs,
  SET/RESET/variables and scope/locking rules. Include comments and object metadata.

**Exit:** objects behave consistently under session scope, DDL rollback, dependencies,
prepared reuse and native reopen; catalog listings reflect the actual implementation.
Sources: `src/catalog/`, `src/main/settings/`, upstream `src/catalog/`,
`src/function/table/system/`, `src/main/settings/` and `test/sql/trigger/`.

## G11 — DML, constraints and schema evolution

**Current:** ordinary INSERT/UPDATE/DELETE, primary/unique/NOT NULL and selected
ALTER exist; conflict/returning/joined DML and many CREATE/ALTER options are rejected.

- **G11.1 Complete mutations.** Add RETURNING, INSERT conflict/replacement forms,
  BY NAME and DEFAULT expressions, UPDATE FROM/tuple assignments, DELETE USING,
  MERGE and other pinned mutation forms. Preserve changed-row and result metadata.
- **G11.2 Complete constraints.** Implement CHECK, foreign keys, generated columns,
  applicable constraint DDL and dependency rules; handle multirow/self-referential
  statements, NULLs and statement-atomic failure according to the source.
- **G11.3 Finish schema changes.** Add ALTER TYPE/USING, nested-field changes and
  remaining object/column modifiers; coordinate indexes, defaults, dependent objects,
  old snapshots and catalog conflict timing.
  - **G11.3a ADD COLUMN execution cost — functional and performance gates
    passed; delegated completion sweep pending.** Source
    mapping follows `DataTable`/`RowGroupCollection::AddColumn` and
    `RowGroup::AddColumn`: stable published rows retain their existing columns and a
    simple selected default can use one constant vector, while custom evaluators and
    non-simple defaults retain materialized physical-slot demand. Physical-slot state
    is copy-on-write, and scans retain the published ID vector when derived metadata
    proves physical and logical order match; holes and relocated order keep the
    ordered-selection path. The non-quiet development diagnostic reduced
    `alter_add_column` from 177.634x on the immediately preceding literal-only tree
    (258.8–268.6x initial baseline) to 0.179x. A separate focused dual-pin diagnostic
    passed both Rust campaigns at no more than 0.238x of the faster C++ median; these
    runs are diagnostic. The final quiet 21-sample focused latency Gate P passed at
    no more than 0.2401x of the faster C++ median. Final integrated evidence is
    recorded below and supersedes those earlier diagnostic/frozen results.

    Validation manifest: owned paths are `src/catalog/expression.rs`,
    `src/planner/stored.rs`, `src/storage/table{.rs,/alter.rs,/rows.rs,/recovery.rs}`,
    `test/component/alter.rs`, `benchmark/g11_add_column_workloads.json`,
    `test/performance/g11_add_column.test`,
    `benchmark/g11_add_column_sqllogic_workloads.json`,
    `scripts/{measure_sqllogic_performance.py,test_measure_sqllogic_performance.py,
    alter_reference.py,test_alter_reference.py}`,
    and this entry. Focused checks are `cargo test --offline --test alter`,
    `cargo test --offline --test contracts
    add_default_demand_distinguishes_materialized_and_simple_physical_paths --
    --exact`, `cargo test --offline --test sql sql_logic_corpus -- --exact`, and
    `python3 scripts/test_measure_sqllogic_performance.py`.
    Negative/boundary coverage includes empty and deleted-only tables, literal and
    one-cast simple defaults, non-simple/custom per-slot results and failures, NOT
    NULL, differing catalog-basis append/delete prefixes, relocation/holes, old
    readers, rollback, checkpoint reclamation/failure, WAL and native reopen.
    Unchanged native workload IDs are `scan`, `filter`, `aggregate`, `point`, `limit`,
    `correlated_exists`, `recursive_linear`, `recursive_cycle`,
    `recursive_correlated`, `alter_add_column`, `alter_drop_column`, and
    `alter_rename_table`; run `scripts/compare_native.py` with nine samples and
    `benchmark/native_workloads.json` against both `release` and `development`.
    Run `scripts/alter_reference.py --target release|development` after the final
    release/no-default-features build for independent ALTER checkpoint/WAL exchange.
    Its report separates the unchanged SQL corpus from the exchange result: both
    stay fail-closed, so a known pin divergence cannot hide an unexecuted or failed
    exchange. The exchange's final aggregate is explicitly cast to VARCHAR because
    the development CLI serializes BIGINT JSON values as strings while the release
    adapter returns numbers; wrong totals still fail exactly.
    Gate P's exact focused manifest is `benchmark/g11_add_column_workloads.json`:
    50,000 rows, one thread, three warmups and nine alternating paired samples, timed
    `ALTER TABLE t ADD COLUMN added BIGINT DEFAULT 7`, untimed reset and verified
    `sum(added)=350000`; run both pins then `scripts/fastest_reference.py` on their
    reports. Preserve wall, CPU, peak-memory and I/O evidence under `target/` and
    require every applicable metric to beat the faster pin independently.

    The process-resource manifest is
    `benchmark/g11_add_column_sqllogic_workloads.json`. Its shared SQLLogic file
    creates one 50,000-row table, repeats 128 transactional literal-default ADDs
    with rollback, then commits and verifies one ADD. This keeps table construction
    explicit while excluding repeated DROP work and making ADD dominate the narrow
    process-level observation. `measure_sqllogic_performance.py` records the shared
    file hash and bytes inside each pin's identity together with that pin's source,
    build and test directories, CMake cache, runner and CLI identities. After the
    quiet-host window, build `target/release/sqllogictest` with
    `cargo build --release --no-default-features --bin sqllogictest`, then run:
    `python3 scripts/measure_sqllogic_performance.py --release-cpp
    ../duckdb-v1.5.5/build/rewrite-reference/test/unittest --development-cpp
    ../duckdb/build/engine-walkthrough/test/unittest --rust
    target/release/sqllogictest --test-root . --workloads
    benchmark/g11_add_column_sqllogic_workloads.json --report
    target/g11_add_column_sqllogic_resources.json --samples 21 --warmups 3`.
    Acceptance requires wall latency, invocation throughput, CPU, peak RSS, block
    input and block output to pass independently against the faster pin.

    The resume audit passes all 65 ALTER component tests, the exact default-demand
    contract and the SQL corpus target. Independent release and development
    checkpoint/WAL exchanges all pass after transport normalization. Each complete
    command still exits nonzero because the unchanged corpus exposes one explicit
    reference divergence per pin: release accepts `DROP NOT NULL` where its corpus
    expects an error, and development accepts `ADD COLUMN ... NOT NULL` where its
    corpus expects an error. These are retained divergences, not exchange passes.
    Executable revision `ed7e62d` closes the shared consumer and resource gates. The
    focused ADD campaign in `target/final-g11-add-native-fastest-ed7e62d.json`
    passes at 0.237/0.238 of the faster pin. The final 41-sample full native pair
    and joint gate in `target/final-native-*-ed7e62d-41samples.json` pass all 12
    workloads; `recursive_correlated` is 0.977/0.974 of the faster pin after
    retaining generation cardinality without repeated batch recounting. The
    process report `target/final-g11-add-resources-ed7e62d.json` independently
    passes wall 0.327, CPU 0.167, peak RSS 0.366, block input/output 1.0 and
    throughput. Release and development checkpoint/WAL exchanges remain green;
    each aggregate reference command still exits nonzero solely for the one
    explicit unchanged corpus divergence per durability configuration described
    above.

    The default-demand split is pinned-source behavior, not a timing inference.
    Release `src/parser/transform/statement/transform_alter_table.cpp` keeps only a
    bare `ConstantExpression` on the direct ADD path; development
    `src/parser/peg/transformer/transform_alter.cpp::IsSimpleDefaultValue` additionally
    admits one non-TRY cast around a constant. Both pins' materialization rewrite is
    `ADD ... DEFAULT NULL`, visible-row `UPDATE`, then `SET DEFAULT`, so deleted slots
    are outside non-simple evaluation. Development's unchanged C API tests
    `ALTER with a non-constant DEFAULT expands and executes fully` and `an error
    inside an expanded group is sticky and rolls back` assert that expansion and
    atomic failure; the sequence ADD tests assert per-visible-row values.
- **G11.4 Verify durable and concurrent behavior.** Exercise prepared mutation,
  rollback, concurrent changes, native serialization and WAL replay for each new
  form. Do not treat in-memory DDL success as durable compatibility.

**Exit:** unchanged DML/constraint/ALTER cases preserve all rows and metadata on
failure, with matching error timing and visibility.
Sources: `src/planner/binder/{statement,alter}.rs`, `src/catalog/alter.rs`,
`src/storage/table/alter.rs`, upstream `src/planner/binder/statement/`.

## G12 — Native files and checkpoint compatibility

**Current:** substantial native reading and selected versioned writing exist,
including nested/VARIANT/temporal values and catalog-named ENUM checkpoints. Named
ENUM checkpoint and WAL handoffs pass in both producer directions against both pins,
with development's qualified schema WAL layout additionally covered. Thirteen decoder
registrations exist; the writer does not provide corresponding general compressed-write
selection.

- **G12.1 Complete format/version metadata.** Map supported historical/current
  versions, headers, catalog objects, types, constraints, defaults, indexes,
  statistics and layout changes. Track read, create, rewrite and upgrade separately.
- **G12.2 Finish compression and encoding.** Map every pinned codec/version/type
  combination; implement missing readers and required encoder/selection behavior,
  compressed updates and nested validity/string layouts. Compare actual values.
- **G12.3 Add selective and scalable storage.** Connect block/row-group access,
  partial scans, large values/files, compaction/vacuum and free-block reclamation to
  buffer/resource contracts. Remove artificial size ceilings only with bounded I/O.
- **G12.4 Prove interoperability by direction.** Read independently produced files,
  write files read and mutated by C++, then read those mutations in Rust. Include
  mixed types, deleted row IDs, endian/platform cases, corruption and version rejection.

**Exit:** required format populations pass bidirectionally with unchanged-file
guarantees on rejected reads/writes. A private snapshot round trip is not this gate.
Sources: `src/storage/duckdb/`, upstream `src/storage/`,
[storage specifications](../specs/components/storage.md).

## G13 — WAL, recovery and checkpoint lifecycle

**Current:** native WAL v2, selected DML/ALTER/nested/named-ENUM recovery, logged
commits, checkpoint policies and process interruption tests exist. Versioned schema
and named-type records cover the legacy fields and storage-69 qualified names within
Rust's top-level schema model. WAL v1 is explicitly rejected.

- **G13.1 Complete record and version coverage.** Map all required WAL record/object/
  type families, nested update paths and version transitions; preserve atomic FLUSH
  boundaries, index/default metadata and committed-versus-aborted changes.
- **G13.2 Complete maintenance lifecycle.** Match automatic/manual/forced/concurrent
  checkpoints, sidecar reconciliation, truncation, restart and repeated recovery.
  Extend the selected physical-order reclamation contract to remaining concurrent,
  compaction and vacuum histories without changing default/backfill demand.
- **G13.3 Verify publication failures.** Extend truncation/corruption/process-kill
  tests to new operations, short writes, sync/rename failures and compound faults.
  Distinguish definite failure from unknown commit and safe recovery-required states.
- **G13.4 Exercise reference handoffs.** Alternate readers/writers and checkpoint/
  recovery owners across both pins, including continued writes after recovery,
  encrypted forms once G21 lands and backward-compatibility obligations from G01.

**Exit:** all acknowledged commits survive required failure histories and incomplete
commits are invisible, with compatible files and errors after restart.
Sources: `src/storage/{logged,recovery,log}.rs`, `src/storage/duckdb/wal/`,
`test/component/{recovery,logging,checkpointing}/` and companion test files.

## G14 — Transactions and connection lifecycle

**Current:** optimistic snapshot transactions conflict with every intervening
writer, even disjoint writes. That is observably different from DuckDB concurrency.

- **G14.1 Match visibility and conflict domains.** Implement required row/catalog
  version visibility, concurrent disjoint updates, conflict timing and read/write
  transaction modes; retain old snapshots and safe version reclamation.
- **G14.2 Match failure and statement lifecycle.** Define bind/execution/commit error
  transitions, autocommit over multiple statements, implicit transactions, rollback,
  read-only mode, prepared reuse and cancellation exactly as in the reference.
- **G14.3 Match database-instance/connection ownership.** Handle repeated opens,
  shared instances, locks, active results, close/reopen and multiple catalogs.
  Coordinate with G22 foreign handles and G18 pending execution.
- **G14.4 Validate histories.** Run deterministic overlapping read/write/DDL/index/
  checkpoint histories, resource failure and process tests; check outcomes against
  the source rather than accepting any serializable outcome.

**Exit:** mapped transaction/API histories have the same allowed results and errors;
stronger blanket rejection is not full functional parity.
Sources: `src/transaction/`, `src/main/connection.rs`, `src/storage/table/`,
upstream `src/transaction/`, `test/sql/transactions/`.

## G15 — Index DDL, maintenance and access

**Current:** selectable hash/B-tree equality indexes and native ART output exist.
Mutations rebuild runtime indexes; SQL index DDL and range access remain open.

- **G15.1 Add index catalog/DDL.** Implement create/drop/names, unique/composite/
  expression definitions and persistence/dependency metadata through G10/G12.
- **G15.2 Implement incremental maintenance.** Update index state transactionally
  for insert/delete/update, schema changes and rollback; retain uniqueness semantics
  for NULL, NaN, collated and nested keys.
- **G15.3 Add selective access.** Define range/batched/gather access and planner
  eligibility, residual predicates and statistics. Distinguish selecting fewer rows
  from actually reading fewer file blocks.
- **G15.4 Finish native index compatibility.** Verify reference index visibility/use,
  rebuild/reopen, corruption and mutation behavior; add extension index contracts
  only for the extension scope selected in G01/G23.

**Exit:** SQL-defined indexes work across transactions/native files and eligible
access produces correct rows without changing errors or effects.
Sources: `src/execution/index/`, `src/optimizer/key_lookup.rs`,
`src/storage/duckdb/writer/index.rs`, upstream `src/execution/index/art/`.

## G16 — Statistics, optimization and physical planning

**Current:** the default pipeline has expression simplification, equality lookup
and conservative EXISTS decorrelation. There is no general cost model.

**G16.3a.1 frozen validation manifest — decimal total-cents aggregation.** Owned
paths are `src/common/vector.rs`, `src/function/aggregate{.rs,/exact.rs}` and, only where the selected
ungrouped driver requires it, `src/execution/operator/aggregate{.rs,/batched.rs}`;
the slice also owns its focused component/performance fixtures, performance
manifest and this backlog entry. Fast validation is
`cargo test -p duckdb-rust --test numeric batches::exact_sum_column_proofs_cover_block_width_transitions_and_signed_tails -- --exact`
and `cargo test -p duckdb-rust --test numeric batches::exact_sum_batches_match_scalar_prefixes_nulls_and_wide_fallbacks -- --exact`;
the affected SQL command is `cargo run --offline --bin sqllogictest -- test
sql/relational.test performance/g16_decimal_total_cents.test`. The unchanged upstream case ID is
`test/sql/types/decimal/decimal_aggregates.test`, selected exactly with
`scripts/run_upstream.py --target both --path-prefix
test/sql/types/decimal/decimal_aggregates.test`; it covers empty/NULL values,
DECIMAL physical-width boundaries and table SUM. Full functional acceptance adds
the complete `numeric_batches`, `numeric_contracts`, `grouping` and SQL
`relational.test` targets so scalar/batch, checked overflow/cancellation,
dictionary, alternate aggregation, grouping and downstream decimal arithmetic
remain consumers. Performance workloads are the shared immutable custom native
`decimal_total_cents` case (50,000 stored `DECIMAL(12,2)` rows) and the maintained
substantial SQLLogic total-cents case, serial/one thread, release/no tracing,
three warmups and 21 alternating samples against both pins. Each workload must
return the declared exact row/count checksum and pass independently for wall,
process CPU, peak RSS, block input/output and invocation throughput at Rust/faster
C++ `<=1.0` (throughput `>=` the faster reference); raw identities and failures
stay under `target/`. Final commands use fresh paths with `compare_native.py` plus
`fastest_reference.py` for the embedded latency case and
`measure_sqllogic_performance.py` for the complete resource gate.

The implementation maps both pins' `PhysicalUngroupedAggregate` and DECIMAL
`BindDecimalSum`/`GetSumAggregate` path: DECIMAL widths through 18 retain their
physical signed-64 coefficient lane while logical `Value` remains the fallback
oracle. NULL, constant, dictionary, sliced, DECIMAL(19..38), cancellation and
checked prefix-overflow paths stay on the existing general kernels. The shared
scan prerequisite `be47ada` removes only the diagnosed stable-order identity
rebuild and retains hole/relocation/snapshot/rollback/checkpoint coverage. On the
resource gate, construction of the one-million-row narrow DECIMAL table happens
inside every timed process and the cache remains resident through the query, so
its allocation CPU and peak RSS are measured even though SUM is the terminal
consumer; G16.3a.3 separately retains the same cache through a non-SUM filter.
Fallible cache allocation occurs only after complete logical validation and NULL
eligibility, preserving the former type-error precedence. Clone, empty/sliced,
contiguous/noncontiguous concatenation, width-18 extrema, metadata mismatch,
cached cancellation and WAL/checkpoint/reopen reconstruction have focused tests.
On the final integrated tree, the focused tests pass, the complete numeric target is
70/70,
grouping is 64/64, local relational/performance SQL is 14/14, and each pin's own
decimal aggregate upstream file is 1/1: release hash
`5a5367993d562fd9a5cd3fdd20b82171113588dcf92cb43db2d4bce0b7d79158` and
development hash
`f3d88b5044ca1f8dc2f6ff45c1c60f0ae71404037eec96817d1532ac020ff8e0`.
The files differ and remain separate pin-specific source inputs rather than one
claimed unchanged fixture. The frozen 21-sample native reports in
`target/g16-decimal-total-cents-final-{release,development,fastest}-21.json`
pass on their recorded source/binary identity: the joint fastest-reference gate
is 0.761x for the release-paired campaign and 0.895x for the development-paired
campaign. The native Gate P is therefore **passed** for that frozen source. The
Executable revision `ed7e62d` supersedes that frozen-source status. The native
joint gate in `target/final-g16-decimal-total-native-fastest-ed7e62d.json`
passes at 0.806/0.752 of the faster reference. The one-million-row SQLLogic report
`target/final-g16-decimal-total-resources-ed7e62d.json` also passes independently:
wall 0.742, CPU 0.5, peak RSS 0.943, block input/output 1.0 and throughput. Native
latency and the process resource scope are therefore both passed.

**G16.3a.2 frozen validation manifest — ordinary ungrouped aggregation.** This
bounded child owns `src/common/cast/mod.rs`, its focused cast regression,
`benchmark/g16_ordinary_aggregation{,_sqllogic}_workloads.json`, the two
`test/performance/g16_ordinary_aggregation*.test` fixtures and this entry. The
ordinary aggregate execution itself uses the shared stable physical-order scan
prerequisite; this child does not claim ordered aggregate arguments, grouping,
or a new aggregate operator. Fast checks are
`cargo test --offline --test casts primitive_integer_overflow_reports_physical_types -- --exact`,
`cargo test --offline --test execution batches::integer_sum_batches_preserve_wide_prefixes_and_vector_views -- --exact`,
and `cargo run --offline --bin sqllogictest -- test
performance/g16_ordinary_aggregation.test
performance/g16_ordinary_aggregation_overflow_error.test sql/relational.test`.
Affected functional acceptance includes the complete `casts`, `execution`,
`numeric` and `grouping` targets, the full unchanged upstream
`test/sql/aggregate/group/test_group_null.test` population, and the retained
reachable prefix of `test/sql/aggregate/aggregates/test_sum.test` for both pins.
The latter is deliberately partial: records 18–19 require G08.2 `ORDER BY`
inside aggregate calls and are outside this unmodified-aggregation child.

Performance has three independently declared scopes: prepared native latency
for 50,000 stored BIGINT rows with separate `sum(i)` and `count(*)` cases; a
one-million-row process-level SQLLogic SUM/count workload; and a process-level
SQLLogic workload that repeats the changed HUGEINT-to-BIGINT SUM overflow 128
times. The SQLLogic files are immutable custom-comparable inputs shared by Rust
and both exact pinned runners; they have no external fixture directives. Final
acceptance uses release/no tracing, one thread, three warmups and 21 alternating
samples. Every case must validate its exact result or error and independently
meet Rust/faster-C++ `<=1.0` for wall, CPU, peak RSS and block I/O, plus invocation
throughput at least the faster reference. The 50,000-row prepared scope and the
one-million-row/repeated-error process scopes are not interchangeable.

Both pins map ordinary SUM/count through
`src/execution/operator/aggregate/physical_ungrouped_aggregate.cpp` (release
`9631350d6f0632c609cea28d22f514b925e40e5ade2826ccea9a485f9ecadcfd`,
development
`3d6fbd7e62b1a49b630a17e0eea21535386c62ef896c48625c448e8fc09e81fa`)
and SUM through `extension/core_functions/aggregate/distributive/sum.cpp`
(release
`54948afb24869e7443cb1c6fcba07cc91089e56a421d9227c1df4a1c7fd9c1b6`,
development
`d1a31e607537af8543d7ae31472941f4d5c6a3eba29bc38c7befd083be0b95ae`).
The release uses its direct unary SUM path while development contains the
clustered SUM rewrite; the stable scan prerequisite is the Rust-side source of
the measured ungrouped improvement. Both pins format checked primitive narrowing
through `CastExceptionText<SRC,DST>` in
`src/include/duckdb/common/operator/cast_operators.hpp` (release
`7282bb476d972c67399c7f556c3973716caf0ed61da78b9995f8b9809820bbeb`,
development
`72aeac3d44657d87da757c57eb09d6b98ed79199293e088fc5b4651ccdc3c053`),
using physical names `INT128` and `INT64`. Rust now derives those names from the
retained bound cast specification while preserving `Error::Conversion` and all
nonnumeric/parsing paths.

The original unchanged `test_sum.test` run stopped identically on both pins
after 14/19 records at the pre-existing Rust diagnostic
`4611686018427388403500 overflows BIGINT`; that first-blocker evidence remains
under `target/`. After the bounded diagnostic correction both pin-specific files
(release hash
`752a36568e99b9bacd27a2ad0f5340ac562a46766ce046ec253f0d346c25aa53`,
development hash
`4cef15f7fb47d5984eb1b51eed59a9b7b2ca89035b19b7d45b3a11c0c4ada5a6`)
advance to 17/19, then stop identically at `sum(n ORDER BY ABS(n))`; this is
retained G08.2 blocker evidence, not an upstream pass. The separate complete
`test_group_null.test` population passes 4/4 on both pins. Focused validation
passes the exact overflow/category regression, all 14 cast tests, all 106
execution tests, and the local ordinary/repeated-error fixtures on Rust and both
pinned C++ runners (3 and 130 records respectively). Post-change nine-sample
diagnostics (not quiet-host acceptance) measured native SUM at 0.889x release
and 0.270x development, and COUNT at 0.682x release and 0.248x development.
The frozen 21-sample native reports in
`target/g16-ordinary-aggregation-final-{release,development,fastest}-21.json`
pass every prepared workload on their recorded source/binary identity: SUM is
0.893x release-paired/0.963x development-paired and COUNT is 0.680x/0.560x at
the joint fastest-reference gate. The native Gate P is therefore **passed** for
that frozen source. Final integrated evidence supersedes it: the joint native gate
in `target/final-g16-ordinary-native-fastest-ed7e62d.json` passes SUM at
0.764/0.825 and COUNT at 0.690/0.722 of the faster pin. The process report
`target/final-g16-ordinary-resources-ed7e62d.json` passes the million-row case at
wall 0.598, CPU 0.5, peak RSS 0.941 and block input/output 1.0, and the 128-error
case at wall 0.269, CPU 0.0, peak RSS 0.346 and block input/output 1.0; throughput
also passes for both. Native and both process scopes are passed.

**G16.3a.3 frozen validation manifest — narrow DECIMAL filtering.** Owned paths
are `src/common/type_registry/numeric.rs`, its `test/component/numeric_batches.rs`
coverage, `benchmark/g16_decimal_filter{,_sqllogic}_workloads.json`,
`test/performance/g16_decimal_filter.test`, and this backlog entry. Fast checks
are `cargo test --offline --test numeric
batches::narrow_decimal_selection_covers_physical_widths_orderings_and_fallbacks
-- --exact` and `cargo test --offline --test numeric
narrow_decimal_sum_cache_rebuilds_after_wal_checkpoint_and_reopen -- --exact`.
The latter is the latest reopen contract: WAL reconstruction must not retain a
stale coefficient cache. Functional acceptance is the complete `numeric`,
`execution`, `grouping`, and `casts` targets plus `cargo run --offline --bin
sqllogictest -- test/performance/g16_decimal_filter.test test/sql/relational.test`;
it retains NULL/selected/dictionary/sliced/width-18 fallback, ordering,
cancellation, bad metadata, and checkpoint/WAL/reopen consumers. The unchanged
upstream case IDs are exactly `test/sql/types/decimal/decimal_aggregates.test`,
`test/sql/aggregate/group/test_group_null.test`, and the reachable records 1--17
of `test/sql/aggregate/aggregates/test_sum.test`, selected against both pins with
`scripts/run_upstream.py --target both --path-prefix` for those three paths.
Records 18--19 remain the explicit G08.2 `ORDER BY`-in-aggregate blocker, not a
G16 failure or a claimed full-file pass.

Gate P has two independent workloads: the immutable 50,000-row prepared native
`decimal_filter` (`d >= 250.00`, exact count 25,000) and the one-million-row
process SQLLogic workload in `test/performance/g16_decimal_filter.test` (`d >=
5000.00`, exact count 500,000). Both require serial, release/no-tracing,
three warmups and 21 alternating samples against both pinned C++ references;
each independently requires Rust/faster-reference <=1.0 for wall, CPU, peak RSS
and block I/O, and throughput >= the faster reference. The frozen native reports
`target/g16-decimal-filter-final-{release,development,fastest}-21.json` pass
their recorded source/binary identity at 0.579x release-paired and 0.515x
development-paired at the joint fastest-reference gate. Final integrated evidence
supersedes it: `target/final-g16-decimal-filter-native-fastest-ed7e62d.json`
passes at 0.575/0.555 of the faster pin, and
`target/final-g16-decimal-filter-resources-ed7e62d.json` passes wall 0.736, CPU
0.5, peak RSS 0.949, block input/output 1.0 and throughput. Native and process
Gate P scopes are passed. Raw samples, identities and earlier failed runs remain
under `target/`.

- **G16.1 Establish statistics lifecycle.** Implement ANALYZE and required table/
  column statistics, propagation, invalidation and cardinality estimates.
- **G16.2 Add relational transformations.** Expand safe filter/projection/limit
  pushdown, decorrelation, join ordering, common expression/subplan reuse and set/
  aggregate/window rewrites, preserving error and effect semantics.
- **G16.3 Add cost-based physical selection.** Select scan/index, join, aggregation,
  sorting/Top-N and external algorithms using explicit capabilities, statistics and
  resources. Support relevant optimization controls and comparable plans.
- **G16.4 Complete EXPLAIN/profiling.** Implement required plan forms, EXPLAIN ANALYZE,
  estimates versus actuals, query metrics and settings with reference-facing output
  contracts. Development tracing is separate tooling.

**Exit:** optimizer-on/off results agree for supported cases, observable controls
work, and measured planning/execution regressions are addressed. C++ algorithm
identity is unnecessary; performance remains a separate acceptance dimension.
Sources: `src/optimizer/`, `src/execution/physical_plan.rs`, upstream
`src/optimizer/`, [optimizer spec](../specs/components/optimizer.md).

## G17 — Memory, buffers and spilling

**Current:** QueryContext accounts intermediate rows. There is no byte allocator,
buffer pool, spill policy or complete query memory ownership model.

- **G17.1 Account actual resources.** Add query/database byte budgets, ownership,
  reservations and fallible allocation across values, vectors, operator states,
  catalog work and external scans; wire memory/temp limits into settings.
- **G17.2 Add buffer management.** Implement block pin/unpin, eviction, dirty state,
  cache lifetime and I/O attribution, integrating file format and transaction needs.
- **G17.3 Implement external algorithms.** Spill/reload joins, grouping, sorting,
  windows and intermediates under bounded memory; preserve ordering/equality/type
  metadata and cleanup on error, early consumer stop and process restart.
- **G17.4 Exercise limits.** Test datasets exceeding memory, long/nested values,
  low-budget concurrency, allocation/disk-full failures and encrypted temporary
  files where required. Validate peak bytes and I/O, not only row counts.

**Exit:** reference-supported workloads succeed under comparable limits or fail
with matching resource semantics; raising arbitrary limits is not the solution.
Sources: `src/parallel/mod.rs`, `src/common/vector.rs`, `src/storage/`,
upstream `src/storage/buffer/`, `src/include/duckdb/storage/buffer_manager.hpp`.

## G18 — Parallel scheduling and pending execution

**Current:** InlineScheduler synchronously runs one task; stream adapters do not
establish a parallel/pending query engine.

- **G18.1 Define scheduling state.** Add task pools, pipeline dependencies, local/
  global operator state and exactly-once scheduling with thread settings.
- **G18.2 Parallelize operators.** Partition scans, joins, aggregation, sorting and
  format reads/writes; combine/finalize states while preserving observable ordering,
  mutation atomicity and transaction snapshots.
- **G18.3 Implement asynchronous progress.** Add pending result/readiness/backpressure,
  cancellation/timeouts, query close and safe cleanup. Connect public wait/step APIs
  through G22 rather than spinning synchronously behind an asynchronous name.
- **G18.4 Test scheduling histories.** Exercise one/many threads, blocked tasks,
  resource pressure, cancellation races, multiple connections and configuration
  changes; map upstream stress and platform cases.

**Exit:** concurrent/pending APIs and configured parallel work preserve reference
semantics, with no lost/replayed rows or leaked work after cancellation.
Sources: `src/parallel/mod.rs`, `src/execution/stream.rs`, upstream `src/parallel/`.

## G19 — Table functions and multi-file scan contracts

**Current:** table binding special-cases integer `range`/`generate_series`.
There is no general table-function/external-format lifecycle.

- **G19.1 Register and bind table functions.** Add signatures, named/default options,
  schema discovery, bind data, local/global state and ownership; integrate correlated
  arguments, replacement scans and registration through APIs/extensions.
- **G19.2 Define source capabilities.** Support projection/filter/limit pushdown,
  residual filters, statistics, virtual columns and streaming batches. Preserve
  logical column identities through physical projection and type conversion.
- **G19.3 Implement multi-file behavior.** Expand lists/globs, schema union/by-name,
  partition columns, filename/row identifiers, sampling and reader reopening.
  External file snapshots must not be inferred from native-table MVCC.
- **G19.4 Port core table functions.** Inventory metadata, range variants, UNNEST,
  test_all_types and other built-ins; connect dependencies to their actual owners.

**Exit:** a new source can bind/scan through ordinary contracts and public consumers
with correct schemas, pushdown residuals, lifetime and failure behavior.
Sources: `src/planner/binder/table.rs`, `src/storage/scan.rs`, upstream
`src/function/table/`, `src/common/multi_file/`.

## G20 — CSV, JSON, Parquet and COPY

**Current:** native DuckDB/snapshot formats are implemented; general SQL external
read/write support is absent. Using serde_json internally is not the JSON extension.

- **G20.1 CSV.** Port read/write, sniffing/schema options, quoted/buffer boundaries,
  encodings/compression, NULL/error/reject handling and parallel records.
- **G20.2 JSON.** Port JSON SQL functions plus readers/writers, paths/transforms,
  schema inference and strict/lax behavior. Preserve JSON null, SQL NULL, missing
  fields, numeric domains and mixed/nested records.
- **G20.3 Parquet.** Implement interoperable read/write metadata, pages/encodings,
  compression, decimals/temporal/nested mappings, statistics/pushdown and required
  encryption/logical annotations. Verify files with independent engines.
- **G20.4 COPY/import/export.** Implement options, partitioned/batched/multi-file
  writes, output finalization, cleanup and database import/export/copy lifecycle.
  Include text/blob readers and any additional inventoried core format entry points.

**Exit:** unchanged format suites and cross-engine files pass, including malformed
data, partial outputs, low memory, schema variation and exactly-once parallel scans.
Sources: upstream `src/execution/operator/csv_scanner/`, `extension/{json,parquet}/`,
[format spec](../specs/components/file-formats.md).

## G21 — Filesystems, secrets and encryption

**Current:** native publication has a local filesystem adapter and fault boundaries;
there is no general routed filesystem, secret manager or encrypted database engine.

- **G21.1 Filesystem capabilities.** Add routed filesystems, random/sequential/range
  reads, globs, compression, metadata/cache invalidation, lock/durability behavior,
  cancellation and remote retries without replaying non-idempotent writes.
- **G21.2 Secret and external-access policy.** Implement providers, scope selection,
  temporary/persistent/transaction lifetimes, redaction and configuration controls.
  Connect extension/file access to the selected policy.
- **G21.3 Encryption.** Match required database headers/keys/ciphers, blocks, WAL,
  checkpoint/recovery and temporary/output encryption. Distinguish wrong keys,
  unsupported versions and authenticated corruption.
- **G21.4 Remote integrations.** Implement required HTTP/object-store behavior or
  compatible extension adapters, then test credentials, redirects, ranges, network
  failure, stale objects and actual bytes transferred. External services are explicit
  test prerequisites, never silent omissions.

**Exit:** inventoried local/remote/encrypted workloads preserve data, policy and
failure behavior across the required operating systems.
Sources: `src/storage/filesystem/`, upstream `src/common/file_system.cpp`,
`src/main/secret/`, [security spec](../specs/components/security.md).

## G22 — Embedding APIs, Arrow and ADBC

**Current:** the Rust library is usable, but Cargo does not expose DuckDB's C ABI
or a complete DuckDB-compatible client/interchange surface.

- **G22.1 C API v1.** Implement exported symbols, configuration/open/connect/query,
  prepared values/results, logical types/vectors/chunks, appender and registration
  callbacks. Match ABI layouts, allocation/free rules, errors and ownership.
- **G22.2 C API v2 and wrapper contracts.** Implement the pinned environment/cache,
  handle/result state machines, wait/step/cancel and the consumer C++ wrapper-facing
  contract. Distinguish WAITING/CHUNK/FINISHED/CANCELLED from errors and internal states.
- **G22.3 Relations and bulk interfaces.** Add deferred relation composition,
  appender buffering/flush/close, replacement scans and user registration. Verify
  early destruction, transaction visibility and nested values through foreign callers.
- **G22.4 Arrow.** Implement import/export and stream schemas, buffers, release
  callbacks, nested/dictionary/extension types, offsets/NULLs and cancellation;
  connect Arrow scans through G19 and validate actual ownership lifetimes.
- **G22.5 ADBC.** Implement required driver database/connection/statement metadata,
  parameter binding, transactions, streaming and error/status contracts against the
  selected external ADBC tests and version.

**Exit:** mapped API/ABI/interchange tests run against the Rust-built artifact,
including failures and owner destruction; linking an installed C++ library is not a pass.
Sources: upstream `api_spec/{v1,v2}/`, `src/main/capi/`, `tools/cpp/`,
[API spec](../specs/components/apis.md), [Arrow/ADBC spec](../specs/components/arrow-adbc.md).

## G23 — Extensions and their capabilities

**Current:** Rust function/type registries are implementation seams, not a binary
extension loader or compatible extension distribution system. The pinned population
inventory records 22 development / 23 release configured external extensions, nine
in-tree extensions and two static registrations on each pin. `lance` is release-only
and 20 shared external tags differ. Configured refs, patch hashes and build/load flags
are retained; immutable ref syntax is distinguished from remote-object verification,
and the absent external checkouts are not claimed as tested. Stable and unstable C
tables, the C++ wrapper over C v2 and internal-C++ coupling are separate ABI classes.
G23.1 remains open: this is inventory-only evidence with no comparable Rust candidate,
so **at-parity or better performance: open**.

- **G23.1 Freeze the compatibility population early.** Inventory configured in-tree
  and external extension pins/builds/tests. Separate stable C tables, unstable C
  tables, the C++ wrapper over C v2 and extensions coupled to internal C++ classes.
- **G23.2 Implement loader lifecycle.** Add INSTALL/LOAD/update, repository metadata,
  version/platform/signature checks, disabled/autoload/autoinstall policies, repeat
  loading and retained callback/library ownership.
- **G23.3 Port built-in extension behavior.** Integrate core_functions, JSON, Parquet,
  ICU, autocomplete and TPC-H/TPC-DS generators through their owning groups; exercise
  static and loadable forms where the target requires them.
- **G23.4 Port configured external capabilities.** Assign concrete child goals for
  cloud filesystems, lake formats/catalogs, scanners, spatial/search/vector and other
  inventoried extensions, each with pinned tests and dependencies. Their full remote
  implementations were not audited here and remain explicitly unassessed obligations.

**Exit:** the selected extension population works through documented compatible
interfaces. A Rust trait cannot load an arbitrary internal-C++ binary. For those
extensions, either reimplement/port the capability or explicitly resolve a different
compatibility target; do not claim existing-binary compatibility by assumption.
Sources: upstream `src/main/extension/`, `.github/config/`, `api_spec/VERSIONING.md`,
[extension spec](../specs/components/extensions.md).

## G24 — Clients, shell, distribution and acceptance

**Current:** a small SQL CLI and local Rust tests exist. Full client/tooling,
platform and performance parity have not been demonstrated.

- **G24.1 Client behavior.** Port/adapt pinned Python, Swift, C/C++ and inventoried
  external clients (for example JDBC/R/Node/Wasm integrations where selected).
  Exercise conversions, relation/dataframe APIs, transactions, threading, errors
  and artifact provenance; account for sources outside this checkout explicitly.
- **G24.2 Shell and user tooling.** Match required arguments/dot commands, interactive
  editing/history/completion, renderers/output modes, import/export, metadata,
  progress/profiling, exit codes and interruption. Treat any separately selected UI
  package as its own pinned population.
- **G24.3 Build and release matrix.** Produce shared/static libraries and clients,
  packaging/install paths and required OS/architecture/configuration combinations;
  verify ABI symbols and that every test loads the newly built Rust implementation.
- **G24.4 Cross-cutting correctness.** Continuously run full mapped upstream/feature/
  configuration/slow/client suites, fuzz/malformed input, allocation and persistence
  faults, concurrency histories, coverage and mutation checks. Explain all ignored
  cases and zero-test runs. Missing dependencies remain incomplete evidence.
- **G24.5 Separate performance acceptance.** Measure planning, execution, cold/warm
  storage, durable commits/recovery, concurrency, APIs, clients, CPU, memory and I/O.
  Use equivalent correct workloads and the faster of the two pinned references;
  the existing <=1.0 ratio rule remains binding. The current 34-case baseline passes
  only nine cases under that gate and does not establish general performance parity.

**Exit:** every required population is accounted for and passing under the acceptance
policy. Publish functional, file/API/extension compatibility and performance results
separately. A full Rust test sweep or six passing Kani proofs cannot replace them.
Sources: `tools/shell/main.rs`, upstream `tools/`, `.github/workflows/`,
[client testing](../specs/testing/clients.md), [acceptance](../specs/testing/parity.md).

## Handoff template

```text
Goal: Gxx — <goal title from this backlog>
Baseline: <integrated Rust commit>; pinned development and release from reference-builds.md
Owned chunks: Gxx.1 ... Gxx.n
Dependencies: <specific capabilities; integration owner for shared files>
Validation manifest:
  Fast checks: <affected package/target and test filters; reject zero-test runs>
  Upstream cases: <exact unchanged IDs, negative/boundary cases, configurations>
  Functional acceptance: <complete assigned population and interop/API commands>
  Performance: at-parity or better performance (gate P)
    Workloads: <IDs/manifest, changed operation and affected consumers>
    Metrics/configuration: <latency/throughput/CPU/peak-memory/I/O as applicable>
    Baselines: <both pinned C++ identities; faster reference per workload>
    Evidence: <final source/binary hashes, commands, samples, pass/fail/open>
Deliver: implemented behavior, unchanged upstream cases/source mappings,
         relevant cross-consumer regressions, full-sweep/Kani result,
         at-parity or better performance evidence, remaining gaps.
Constraints: selected subsystem interfaces; development wins semantic disagreements;
             no skips relabeled as passes; raw evidence only in ignored target/.
Readiness: agent says ready -> freeze integrated tree -> independent full pass.
Completion: functional acceptance + full sweep/Kani report + performance gate P;
            satisfy this group's exit criteria; any edit invalidates completion.
```

## Documentation maintenance

This is the only active implementation plan. The superseded
`specs/value-expression-milestone.md` was removed in this measurement chunk; its
prior contents remain recoverable at `c2fdae7`. The stale opening-wave assignments
and historical measurement baseline in this file have been replaced in place.
README files and normative component requirements have not been removed.

Earlier historical parity/progress/checkpoint summaries were already removed.
Their last pre-cleanup tracked version is recoverable at
`20c8214`, including `docs/value-expression-progress.md`, `docs/testing-parity.md`
and family reports. Use `git show 20c8214:docs/<old-name>.md` for a specific historical
question; do not automatically reload that history into every agent context.

Keep [architecture](architecture.md) as the concise implementation map,
[implementation notes](implementation-notes.md) for difficult contracts,
[reference builds](reference-builds.md), [tracing](dev-tracing.md), and
[adversarial testing](sqlite-testing-review.md) as runbooks. Normative requirements
and detailed source specifications belong in `specs/`. A source-system specification
is not an assertion that its Rust counterpart is implemented. Update this work map
in place; avoid copying milestone status into multiple documents.
