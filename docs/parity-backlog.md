# DuckDB parity: current status and implementation plan

Status reconciled 2026-09-18 against integrated engine revision `8f2ef3f`.
This is the single maintained status and work plan. The measurements below
retain their own source revisions; a historical pass is never presented as a
fresh measurement of HEAD. Source and tests take precedence over prose.

## Current status

The Rust engine has substantial SQL, typed/batched execution and selected native
persistence. Full DuckDB replacement remains open across all 24 goal groups.
There is no supported overall completion percentage. The September 18 A1 census
at pre-batch revision `44538fb` refreshes the SQL file population, not the full
native/configuration/platform population or acceptance of subsequent changes.

| Scope | Current disposition | Evidence and remaining boundary |
| --- | --- | --- |
| Latest G06.1g/G06.1h/G06.2c batch | Accepted at `8f2ef3f` | Escaped literals, accent/NFC normalization and LIST regex extraction; both pins' four assigned files pass; seven native and five process workloads pass. See G06. |
| Common integrated regression gate | Passed at `8f2ef3f` | Format, all-target check/Clippy/tests, 1,632 recovery boundaries, Kani 6/6; 2,306.4 seconds. This is not whole-upstream acceptance. |
| Earlier core SQL/type/function slices | Implemented; scoped evidence at revisions in their entries | Core joins/windows/nested values, retained defaults, named ENUM, temporal/math, text, FROM-first and aggregate modifiers exist. Remaining contracts are assigned below; do not port these foundations again. |
| G01.4a fast upstream feedback | Implemented; functional and performance acceptance recorded at `ed7e62d` | Debug worker, selected cache, provenance and watcher are available. Fixture-dependent files need the ordinary runner. No new implementation assignment for an already delivered fast path. |
| G11.3a / G16.3a cost repairs | Functional and native/resource gates passed at `ed7e62d` | ADD COLUMN, ordinary SUM/COUNT, decimal SUM/filter, and the 12-case native manifest were repaired. These are revision-scoped results, not fresh HEAD benchmarks. |
| Full SQL population | A1 refresh complete at `44538fb`, before batch 1 implementation | Release 553/4,834 and development 587/5,637 whole-file passes after exact timeout retries. Seven old-pass pin/file losses remain explicitly recorded. This is not a feature percentage; do not add later scoped passes to it. |
| Original performance population | A1 refresh complete at `44538fb`: 25/34 pass, 9 fail | Both pins, 21 samples, faster-reference gate. Broader configurations and resource coverage remain open; see active batch checkpoint. This is not acceptance of new F1/C2.1 code. |
| External formats, resource/parallel engine, ABI and ecosystem | Major capability gaps | Tracks D–H implement these incrementally; passing local tests does not close them. |

**Delivery priority (2026-09-18): engine first; foreign compatibility last.**
Track G (C ABI, foreign APIs, Arrow/ADBC and clients), ABI-dependent binary
extension loading and foreign-library/client packaging are **deferred — blocked
on core-engine completion**. They are not candidates for current agent batches,
including preparatory design, inventory or benchmark-adapter work. Follow the
durable [delivery-order rule](../AGENTS.md#delivery-order--engine-first-foreign-compatibility-last).
Alphabetical tracks and Gxx reference sections are inventories, not execution order.

**Core-engine exit gate:** accepted in-scope SQL/type/function/query behavior,
catalog/mutations, transactions/native persistence/recovery/indexes, optimizer,
memory/buffers/spill/parallel execution, table sources/formats and required
filesystem/security/built-in capabilities. Refresh applicable engine population
accounting and correctness/performance evidence through A1/A5; open required
engine gaps or missing acceptance evidence keep this gate open. Existing safe
Rust interfaces and engine/shell validation stay active. Foreign compatibility
requirements remain recorded, but neither block this gate nor count as passed.
Only after this gate is met does the final foreign-compatibility phase become
eligible; overall DuckDB replacement still requires that later phase.

Use four independent fields in every active chunk: **implementation**, **functional
acceptance**, **performance**, and **sweep/Kani**. Implementation states are queued,
active, ready, accepted, or blocked (with an exact dependency). Acceptance fields
are pass/fail/open plus tested revision and report; strictly documentation-only
performance may be not applicable with a reviewed rationale. Kani is reported
separately as successful/unsuccessful/incomplete or not applicable with a scope
rationale under its exploratory policy. Validation scope is governed by
[AGENTS.md's durable policy](../AGENTS.md#validation-scope--durable-policy), not
this changing status: partial sweeps by default, full sweeps only with explicit
justification, and no engine validation for documentation/instructions-only edits.
“Accepted” requires the chunk's final-tree gates; retain its accepted revision
when later unrelated work lands. New edits to its contracts reopen affected
acceptance. Never leave an old “pending” sentence as the current status beneath
a newer result.

**Planning maintenance chunk (this change).** Own only
`docs/parity-backlog.md`, `docs/architecture.md`,
`specs/testing/parity.md`, `specs/testing/kani.md`, `AGENTS.md`, and instruction-only
`.codex/agents/verifier.toml`. Validation: reviewed diff,
`git diff --check`, local-link/anchor and command/target checks, plus independent
status and dispatch review. No SQL/source/build/configuration/fixture/workload
changes; unchanged upstream IDs have no new behavioral obligation. Completion
uses only the relevant documentation checks and TOML syntax validation. Engine
sweep, recovery, Kani and performance: **not applicable: documentation/instructions-only**.
The mistakenly dispatched sweep at `d32f08c` was cancelled during its test stage;
it is incomplete and is not acceptance evidence. It must not be restarted.
This updates the plan; it does not execute or close its queued engine chunks.

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
audited regex/output/fixture repairs. None of those earlier attempts is acceptance
evidence for the accepted Wave-A tree; none is a fresh HEAD campaign.
Wave B evidence is under `target/g01-wave-b-20260915/`:
`registry-integrated-2/inventory.json`,
`api-map-integrated-1/api-contract-inventory.json`,
`extension-pins-integrated-1/extension-pin-inventory.json`,
`api-lifecycle-acceptance-3/report.json`, and
`performance/g01-2c-acceptance-4.json` are the retained results for the measured
Wave-B tree, whose exact source identity is in those reports.
Source/binary/harness hashes and exact commands belong in those reports. Historical
pass counts and the superseded milestone baseline have been removed from this plan;
they must not be added to fresh scoped results.

### Historical SQL baseline — September 15

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

### Historical performance baseline — September 15

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
ordinary aggregation **16.3–17.9×**, decimal filtering **13.7–14.7×**. These were pre-optimization failures at the measured baseline revision.
The ADD COLUMN and three G16 numeric/aggregation scopes now have later accepted
native/resource measurements in G11/G16; those supersede the corresponding
numbers here. The remaining baseline needs a current campaign. Preserve default
evaluation demand and checked numeric semantics during further optimization.

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
concurrency and client/API timings remain unmapped/unmeasured. **Overall performance parity remains open.** The 9/34 result describes the
historical baseline only; later G11/G16 and feature-specific passes cannot be
summed into an updated score. A1/A5 must refresh the full manifest and report
unmeasured configurations separately.

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

0. Classify the change. Documentation/planning/status/agent-instruction-only work
   gets relevant artifact checks only; steps 1–7 below govern implementation.
   Never dispatch an engine sweep or benchmark for unrelated prose changes.
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
   the relevant integrated inputs and delegate the manifest's impact-scoped
   commands to the configured low-cost verifier. Follow the durable
   [scope policy](../AGENTS.md#validation-scope--durable-policy). Applicable Kani findings
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

The earlier A–F waves described work that is now partly implemented. They are
replaced by the remaining-work tracks below. Track IDs A1–H5 are dispatch IDs,
mapped to stable Gxx requirements; they do not rename or erase those requirements.

### Why the previous cadence was slow

Recent batches spent most worker capacity on adjacent function families. Shared
execution/performance fixes were often discovered at integration, followed by
long serial acceptance. The latest full sweep took 38.4 minutes; the 21-sample
process campaign additionally exercised five workloads against three engines.
These are measured/declared costs, not estimates of future chunk duration.
Repeated historical narratives and a stale “fast feedback not implemented”
instruction made task selection and validation slower and less clear.

The operating change is to keep a capability-unblocking lane active, validate
shared interfaces before leaf implementation, diagnose representative performance
early, and use the delivered fast-feedback tooling. Retain the required final
gates and sample populations. There is no automatic acceptance scheduler today;
the integration owner maintains the queue below.

### Roles, concurrency and model selection

One integration lead plus at most three workers can run in this environment.
Use separate implementation worktrees with local build caches. The lead owns
`DataType`/`Value`, shared registries, logical/physical plan interfaces,
catalog identities, `DatabaseBuilder`, workspace metadata and wire-format
integration. Workers own their leaf modules and tests. For an executable first
slice, the lead either lands the minimal shared seams before the worktree forks
or explicitly grants provisional edits to named shared files in that worker's
worktree. The first-round cards below grant such proposals to F1 and C2.1; only
the lead integrates them into the shared branch. Workers must preserve others'
changes. A frozen manifest without compiling integration seams is not readiness.

Model choices are starting assignments based on contract risk, not a measured
claim of an optimal model for every task. The available session models and the
configured verifier are authoritative. Current official guidance supports using
smaller models for bounded work and higher reasoning for complex contracts;
see [subagent guidance](https://learn.chatgpt.com/docs/agent-configuration/subagents),
[Terra](https://developers.openai.com/api/docs/models/gpt-5.6-terra),
[Sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol) and
[Astra](https://developers.openai.com/api/docs/models/gpt-6-astra).
No token-price forecast is needed to dispatch this plan.

| Code | Exact model / effort | Responsibility |
| --- | --- | --- |
| L | `gpt-5.6-luna` / medium | Mechanical case mapping, report reconciliation and packaging after the contract is fixed; semantic conclusions require owner review. |
| T | `gpt-5.6-terra` / medium | Default bounded implementation and independent review with strong reference cases. |
| S | `gpt-5.6-sol` / high | Binder/catalog integration, codecs, ownership and cross-module behavior. |
| A | `gpt-6-astra` / high | Transaction conflicts, publication failures, scheduler and foreign-handle state machines; integration decisions with broad consequences. |
| V | configured `verifier`: `gpt-5.6-terra` / low | Impact-scoped completion checks; full runner only with explicit justification. Never silently substitute the implementation model. |

Use explicit model/effort on dispatch (and a bounded task brief), because the
repository default is Terra/low and full-history forks inherit the parent.
Start with the row's choice; escalate T→S or S→A only for a concrete unresolved
contract or two failed correction cycles. Hand over the failing case and relevant
source, not the entire conversation. Downgrade mechanical follow-up to T/L after
the contract is settled. Record elapsed implementation/validation time, review
rework and model usage per accepted chunk under `target/`; use those observations
to revise assignments after the first two rounds. High effort is not needed for
running a long test command.

### Dispatch order and acceptance queue

#### Active run and next three-batch queue — 2026-09-19

User authorization: execute batches 1, 2 and 3 sequentially, with parallel
tracks inside each batch; checkpoint here between batches and continue without
asking for another dispatch. Do not start batch 2 until batch 1 is accepted,
or batch 3 until batch 2 is accepted. Record genuine blockers instead of silently
dropping a gate or marking unfinished work complete. Foreign compatibility stays
deferred under the engine-first rule.

September 19 queue refresh at planning baseline `ca57c3c`: resume the open
Batch 2 acceptance boundary, then take **Batches 3, 4 and 5** as the next three
substantial batches. Batch 3 carries forward the existing assignment; Batches 4
and 5 promote the dependency-ordered rounds below into the concrete queue.
User follow-up authorizes execution of all three batches with parallel tracks,
continuing through their acceptance without another dispatch request. Parallel
read-only scope preparation is active while Batch 2's acceptance is repaired;
implementation of Batch 3 still follows Batch 2 acceptance.
Batch 1 stays accepted. Batch 2's native WAL continuation and atomic first-
transaction publication pass scoped verification and both-pin native oracles at
`d5d19ac`; empty-WAL read elision passes114 affected tests at `36a0293`.
Compiled A2 passes all five workload gates at `9e5418f`, including lifecycle
at0.953 of the faster pin. CSV and all six affected compiled process families
also pass at their recorded inputs. At `d608b5a`, the independent Python proxy
passes both large workloads and all memory gates; three short workloads still
fail latency. The startup repair and tiny entry have passed configured partial
checks and release replay. Independent B1/COUNT measurements expose additional
native query/DDL, checkpoint publication and CTAS process gaps. Batch2 repairs
now cover immutable view payload sharing, packed checked integer arithmetic,
optional projection-chain evaluation and BIGINT-to-text CASE totality, with
retained custom-adapter validation. Their integrated partial manifest is
`target/batch2/native-repairs-integrated-manifest.md`;365 functional tests,
scoped Clippy, coverage (no missing instrumentation) and tracing now pass.
Release/upstream and affected performance acceptance remain pending. Unchanged evidence is retained only under
the impact-scope policy, including the still-failed Python proxy gates.

September 19 carryover: source digest still matches candidate6 preparation.
A busy-host diagnostic (not acceptance) measured Rust narrow14.11ms/wide70.37ms
against release10.61ms/61.30ms, recorded in
`target/batch2/candidate6-diagnostic-20260919.json`. Candidate7 is assigned to
remove per-column CSV string copying by retaining the already-supported shared
UTF-8 arena. ROOT owns only `src/function/table/csv.rs`; partial validation,
existing retention/custom-cast cases and fixed performance obligations are in
`target/batch2/csv-shared-arena-manifest.md`. Formal performance remains open;
user paused activity and quiet-host status will be checked after builds.
Candidate7 partial verification passes: table-functions16, vector1, formatting,
Clippy and tracing; the original misspelled vector filter selected zero tests
and is preserved as invalid evidence. Only that corrected selection was rerun.
Evidence: `target/batch2/shared-arena-verifier/`. Production preparation passed all8 stages.
Candidate7 native performance fails: narrow12.255ms vs release10.307ms;
wide61.321ms vs58.848ms. Development is slower than both, so its passing pair
does not close the joint gate. Preserve `f2-performance/native-*-candidate7.json`
and host preflight. Narrow diagnostic sampling validates383 query oracles and
points to scanner work; candidate8 owns byte-boundary searches/quoted spans with
unchanged parser semantics and explicit boundary tests under
`target/batch2/csv-byte-search-manifest.md`. No CSV process timing followed the failure.
Candidate8 parser18/18 feedback passed after correcting an initially malformed
test fixture (honest failure note under `byte-search-worker/`). ROOT reviewed
state/UTF8/error/cancellation preservation; direct memchr2.8.1 uses the existing
locked version/features. Seven-command scoped verifier passes against hashes
in `byte-search-frozen-inputs.json`: parser18, table_functions16, coverage missing=[],
format/Clippy/tracing. Logs/times/hashes: `target/batch2/byte-search-verifier/`.
Performance/source acceptance remains open; next command is
`python3 target/batch2/prepare_byte_search_candidate8.py`, then quiet-host
`python3 target/batch2/run_csv_native_candidate8.py --authorized` and serial
stages from `target/batch2/remaining-performance-candidate8.json`.
Candidate8 production8/8 passed at `e94507d`. Native wide53.337ms versus
release58.668ms passes; narrow10.724ms versus10.240ms fails (1.0473×).
No CSV process timing followed the failure. Candidate9 adds per-open scratch
reuse guarded by exclusive Arc ownership; new streaming retained-output and
stop/late-error/reopen regressions pass in the CSV10/10 routine selection.
Six-command partial verifier passes under `csv-recycle-manifest.md`; lexer
and parser tests are byte-identical to `e94507d`, so parser18 evidence is reused.
Frozen inputs: `recycle-frozen-inputs.json`; final logs/times/hashes are in
`recycle-verifier-corrected/` (table_functions18, coverage missing=[], trace pass).
The first sweep's test-only Clippy useless_vec failure remains in `recycle-verifier/`;
ROOT corrected the expected-value container and reran the affected selection.
Candidate9 production8/8 passed at `08f81f8`. Native narrow10.411ms versus
release10.324ms fails (both retained Rust medians1.0084x/1.0144x); wide52.786ms
versus58.743ms passes. No process timings followed the failed native gate.
Candidate10 removes the preliminary delimiter-count traversal, retaining fallible
metadata growth and unchanged record semantics; ROOT owns production and the CSV
worker owns a metadata-growth regression. Manifest:
`target/batch2/csv-single-delimiter-manifest.md`; fixed acceptance obligations remain.
Candidate10 seven-stage independent partial verifier passes: parser19/19,
table_functions18/18, formatting/Clippy, coverage missing=[] and tracing. Frozen
inputs match before/after; evidence `target/batch2/single-delimiter-verifier/`
and `single-delimiter-frozen-inputs.json`. Production/source preparation10 passed8/8 at `3fb8cfc`. Candidate10 native latency
passes both fixed workloads against the faster pin: narrow9.851ms/9.917ms versus
release10.223ms (worst0.9701x), wide50.823ms/50.843ms versus58.827ms (0.8643x).
Both paired21-sample reports and fastest-reference gate are retained under
`target/batch2/f2-performance/native-*-candidate10.json`. Host indexing settled
before timing; final preflight87% idle/no swap. CSV process resource gate also passes: narrow wall0.9017x/CPU0.8824x/RSS0.35x;
wide wall0.8661x/CPU0.8632x/RSS0.3437x; I/O equal0, throughput passes.
Evidence `target/batch2/f2-performance/process-candidate10.json`,21samples/3warmups.
Remaining F1/A2/B1/durable/COUNT gates execute serially via
`target/batch2/run_rest_candidate10.py`; Batch2 remains open until all pass.
F1 native/process remaining5 passes. A2 process preflight fails before timing:
fresh `load {TEST_DIR}/g01_2c_loop_sessions.duckdb` is misclassified as an external
fixture. Both pinned LoadCommand implementations delete/create nonreadonly loads;
Rust binds TEST_DIR to owned scratch and removes fresh DB/WAL files. Bounded
Python-only correction/negative tests assigned under
`target/batch2/a2-generated-database-manifest.md`; no workload/source/assertion,
threshold, Rust or pinned input changes. Preserve failed remaining5 A2 stage.
CSV/F1 measured execution paths and tested inputs remain unchanged; their accepted
evidence is retained at candidate10 rather than rerun for unrelated tooling branch.
Generated-output adapter correction passes independent Python-only verifier:
38/38 tests, py_compile and diff checks; hashes stable. Evidence:
`target/batch2/a2-generated-database-verifier/`; actual A2 both-pin gates remain open.
At `72ba095`, corrected preflight reaches C++ but original A2 tuple workload
`left,right`/`first,` fails on bothpins before acceptance samples. Preserve original
bytes/hashes under `target/batch2/a2-tuple-correction-original/` and harness1 failed
report plus direct development probe. Actual string-delimiter Split removes all
empty components, falling back to original input only if none remain; Python/Rust
currently preserve empty tuple components incorrectly. Initial source review chose
the wrong char overload; its failed leading-empty probes are preserved. Corrected
v2 five-case probes pass bothpins, including `first,,last` mapping to two fields.
Parallel Rust/Python contract repair and equivalent shared workload correction are
assigned under `target/batch2/a2-tuple-correction-manifest.md`; preserve5/5000/16
successful operation counts and all performance thresholds. No parity claim for
the incomparable original workloads. CSV/F1 accepted unrelated evidence retained.
Tuple repair routine passes Python12/Rust71; final configured seven-stage partial
verifier passes Python50/Rust71, scoped Clippy/format, coverage missing=[] and trace.
Frozen hashes match before/after; evidence `target/batch2/a2-tuple-verifier/`.
Next `python3 target/batch2/prepare_a2_tuple.py` rebuilds only affected runner bins,
replays exact corrected workloads and unchanged6pin/file selections, then
`run_rest_a2_tuple_candidate1.py` resumes both-pin acceptance with fresh outputs.
At `c2a171e`, preparation18/18 passes: release runner/worker attestation; all
three corrected workloads pass bothpins, compiledRust and Pythonproxy at5/5000/16;
v2sourceprobe5 passes Rust/Python; unchanged upstream release1211/47 and
development1211/47/51/195 all pass with no skips/stale inputs. Evidence:
`target/batch2/a2-tuple-preparation/run1/`. A2 Rust process candidate1 measured:
small andlarge pass allmetrics (wall0.302x/0.149x), lifecycle wall48.229ms versus
38.577ms fails1.2502x (CPU/RSS/I/O pass). Preserve
`target/batch2/a2-tuple-rust-process-candidate1.json`; no proxy orlatergates followed.
Diagnostic200x exactlifecycle validates3200records; samples in
`target/batch2/a2-lifecycle-profile1/` point to fullcheckpoint publication file and
parent syncs during commit. Read-only source investigation in progress; no flush
or durability contract has been weakened. Further repair needs a reviewed impact
manifest including affected native/recovery consumers before implementation.
Reviewed `target/batch2/a2-logged-runner-manifest.md`: route writable SQLLogic
session/worker files through existing `Database::open_logged`, preserving all
per-commit sync/transaction boundaries and read-only behavior. Only runner call
sites/tests change; existing WAL codecs/publication stay unchanged. Scoped runner
and logging crash/recovery consumer checks precede fresh lifecycle/proxy timing.
A2small/large compiled process passes retained individually; lifecycle stays open.
Routine logged-runner checks exposed an error classification gap: read-only DML
and catalog writes returned `Unsupported`, so runners called valid rejections
missing functionality. The two transaction guards now return `Transaction` with
identical checks/messages. Direct worker tests verify `unsupported=false`, and
runner/worker feedback passes72/5. Existing logging and affected ALTER recovery
consumers are included in the configured partial sweep. Native WAL codecs,
publication and flush boundaries are unchanged; exhaustive byte-tail recovery and
Kani are not applicable to this routing/error-tag scope. New comparable workloads
exercise both rejection guards (4/2002 records); the harness permits a read-only
load only of the same safe runtime path generated earlier in that workload.
The strict proxy population expands3→5 with all old cases and thresholds intact.
Logged-runner partial verification passes: worker5, runner72, logging8 and exact
ALTER recovery1; formatting/Clippy/trace pass and coverage has no missing entries.
Rust/Cargo hashes are stable under `target/batch2/a2-logged-rust-verifier/`.
Separate Python verification passes51 tests and four-file syntax compilation;
frozen harness/workload hashes are under `a2-readonly-python-verifier/`.
Performance remains open. Production/source preparation uses
`target/batch2/prepare_a2_logged.py` before fresh serial measurements.
Logged production/source preparation passes17/17: four release binaries, fresh
worker and durable attestations, five workloads through Rust/proxy at
5/5000/16/4/2002 records, and both new cases through both C++ pins. Evidence:
`target/batch2/a2-logged-preparation/run1/report.json`. Timed gates remain open.
At `373381e`, quiet-host logged candidate1 fails compiled lifecycle latency:
66.526ms versus38.180ms (1.7424x); small read-only36.225ms versus35.602ms
(1.0175x). Large read-only passes131.853ms versus215.231ms (0.6126x);
CPU/RSS/block-I/O pass all three. Preserve
`target/batch2/a2-logged-lifecycle-process-candidate1.json`; no proxy/B1/COUNT
stage followed. Runtime sampling and file-publication source review investigate
the remaining lifecycle cost; accepted evidence is not waived or retried unchanged.
Native repair is scoped under `target/batch2/a2-wal-continuation-manifest.md`:
optional clean committed WAL continuation retains physical IDs, byte/entry/commit
counts and compatibility; legacy adapters, checkpoint-marker logs and incomplete
tails retain prior recovery publication. Local storage can atomically stage the
header plus first transaction with unchanged sync strength. An installed initial
transaction with uncertain directory sync now correctly reports CommitUnknown.
The scope includes native logging/checkpoint/recovery/fault and affected catalog,
nested/identity consumers, plus explicit exhaustive recovery and both-pin native
logging oracles. Routine15/16 tests pass; generated-prefix boundaries caught an
incomplete-frame header misclassified as clean EOF, now being repaired. No
performance acceptance or Batch3 dispatch is claimed.
The clean-EOF bug is fixed by tracking incomplete header/payload termination
explicitly; its exact generated-prefix regression passes1/1. Integrated source
is frozen for configured partial verification under
`target/batch2/a2-wal-continuation-verifier/`; the legacy storage fallback keeps
its original header-only failure assertions. Fresh production/oracle preparation
is `target/batch2/prepare_a2_wal_continuation.py`, then serial fixed measurements
via `run_rest_a2_wal_candidate1.py`. Prior failed outputs remain preserved.
Native partial verifier passes149 selected Rust tests, including logging16,
checkpointing20, runner72/worker5, recovery13 plus the explicit exhaustive1,
and affected identity/fault/nested/ALTER/view consumers. Python oracle harness18
passes; formatting/Clippy/trace pass and coverage has no missing entries. Frozen
Rust/Cargo/helper/test/fixture identities match before/after under
`target/batch2/a2-wal-continuation-verifier/`. Release preparation and both-pin
native WAL oracles are next; performance remains open.
Production/source preparation passes its first17 stages, and release native WAL
oracle passes4 data cases plus10 interruption boundaries. Development stops at
primitive-case CLI decoding: its shell emits bare nan/inf/-inf, rejected by the
existing JSON decoder before row comparison. Candidate1 failures are preserved.
A narrow ignored transport decoder retains SQL, fixtures, expected values and
both pins; four negative/representation tests receive Python-only verification.
Then `verify_a2_wal_references_candidate2.py` reruns development only and reuses
release evidence at unchanged source/binary identities. No engine revalidation or
performance waiver follows this untimed oracle-input correction.
The first decoder passes4 tests; development then exposes quoted HUGEINT
rendering versus Rust/release numeric JSON. Candidate3 adds typed decoding from
a separate read-only DESCRIBE: only canonical signed128-bit HUGEINT values are
normalized, with range/shape checks and numeric VARCHAR text retained. Six
adapter tests receive independent Python review; original SQL and logical values
remain unchanged. Candidate1/2 failed oracle artifacts are retained.
Typed adapter6/6 and syntax checks pass. Development native oracle candidate3
passes4 cases/10 boundaries; release14 is reused from candidate1 at identical
engine inputs. Corrected preparation receipt:
`target/batch2/a2-wal-continuation-preparation/final1.json` links successful
commands1–17 and both native oracles without relabeling the failed aggregate.
All production/source obligations for this repair pass; timed acceptance is next.
At `d5d19ac`, quiet-host WAL candidate1 improves lifecycle to37.698ms but still
fails the strict faster-reference37.638ms gate (1.001613x). Both read-only
workloads pass all metrics: small0.9110x and large0.5965x latency. Preserve
`target/batch2/a2-wal-lifecycle-process-candidate1.json`; proxy had not run.
The empty-WAL path redundantly read the checkpoint before ordinary loading read
it again. `a2-empty-wal-manifest.md` scopes an early empty-log decline plus a
counted-read regression (1/1 routine pass). Configured partial verification passes
114 tests: worker5, checkpointing20, logging17 and runner72, plus formatting,
Clippy, coverage (no missing entries) and trace compatibility. Frozen inputs
match under `a2-empty-wal-verifier/`. Parser/recovery/encoding/initial-publication code
is unchanged, so its prior exhaustive, native-oracle and other scoped evidence
is retained at `d5d19ac`. Next `prepare_a2_empty_wal.py` and
`run_rest_a2_empty_wal_candidate1.py`. The latter collects compiled3 and proxy5
measurements as independent serial stages on the same frozen tree, even if one
fails, to identify both bottlenecks; both must pass before B1/COUNT or Batch3.
No threshold, sample, fixture or acceptance obligation changes.
Empty-WAL production preparation passes all13 stages with unchanged source
digests, fresh worker/durable provenance and exact five-case Rust/proxy counts
5/5000/16/4/2002. Receipt: `a2-empty-wal-preparation/run1/report.json`.
At `36a0293`, empty-WAL candidate1 compiled lifecycle still fails:38.856ms versus
38.378ms (1.01246x), while read-only small/large pass0.9074x/0.5957x with all
resource gates. Independent proxy measurement now exposes failures across all5
cases: small loop70.897ms versus25.507ms, CPU5x and RSS1.3585x; large loop and
large read-only latency pass but memory does not. Lifecycle/read-only small also
fail wall/CPU/RSS. Both raw reports and failed stage receipts are retained under
`target/batch2/a2-empty-wal-*-process-candidate1.json` and
`remaining-a2-empty-wal-candidate1-stage-logs/`; B1/COUNT did not run.
Actual attested proxy import/runtime diagnostics validate5 records and identify
unused campaign/upstream orchestration imports in worker startup. A bounded
Python import/protocol separation and an independent native lifecycle cost
investigation are next; preserve parser/oracle/provenance semantics and every
performance obligation. Diagnostics are not acceptance evidence.
Proxy startup separation is implemented: `worker_protocol.py` retains the exact
JSON-line transport and canonical sidecar name; campaign/report imports load on
demand. The new helper is included in both campaign and upstream identities.
Configured Python-only verification passes53 tests and syntax/diff checks, with
stable inputs under `a2-proxy-startup-verifier/`. Actual preparation passes12
stages: each fixed workload in attested and standalone modes with exact counts,
then release2/development4 unchanged upstream files (1211/47 and1211/47/51/195).
Receipt: `a2-lean-import-preparation/run1/report.json`. No Rust rebuild was needed.
The five-case proxy gate is next under
`remaining-performance-a2-lean-import-candidate1.json`; compiled lifecycle is
unchanged and is not retried. Current native sampling validates3200 records and
shows repeated cast construction; a separate cast-only optimization is being
scoped. Function sharing is excluded because regex caches contain mutable state.
At `b49d0d4`, lean-import proxy candidate1 improves small-loop latency to58.718ms
from70.897ms, but fails against26.012ms (2.2574x), CPU4x and RSS1.2452x.
Large-loop and large-read-only latency pass; memory remains1.1389x/1.0177x.
Lifecycle90.356ms and small-read-only83.225ms still fail latency/CPU; lifecycle
memory now passes. Preserve `a2-lean-import-proxy-process-candidate1.json` and
its failed stage receipt. The next actual import diagnostic confirms remaining
Record/dataclasses, Decimal and tempfile startup costs; it is not acceptance.
Cast-only sharing is authorized under `a2-registry-sharing-manifest.md`, with
atomic type registration and custom/stored-expression isolation tests. Its scope
includes fresh affected CSV/F1, lifecycle and proxy evidence; no durability or
function-cache sharing changes. Further Python startup work is separately scoped.
Cast COW implementation adds custom exact/family isolation, late overlapping
type-registration rollback, invalid metadata and future stored-default tests.
The worker's claimed final two-filter pass is withdrawn: it inferred success
from process disappearance without capturing the exit status. Verifier3 passes
casts17/17 and catches a wrong expected row shape in the new contracts test;
ROOT corrects two one-column rows and verifier4 resumes affected checks.
The failed contracts hash and audit are retained in
`a2-cast-focused-handoff-correction.json`. Two earlier Clippy failures (type
complexity and redundant test closures) remain preserved, with both corrected. The bounded immutable
Record/lazy Decimal change passes75 Python consumer tests. Both final partial
sweeps are delegated to the configured verifier under
`a2-cast-registry-verifier/` and `a2-proxy-record-verifier/`; no performance pass
is claimed. Release preparation will build all4 affected runners once, refresh
provenance, replay current selected cases, then measure affected workloads.
Configured verification now passes: Python Record75/75 at its frozen inputs;
cast repair reuses verifier3 casts17/17 and production Clippy, then verifier4
passes contracts108, operator1, table-functions18, worker5 and runner72, plus
formatting, affected Clippy, coverage (no missing entries) and trace checks.
Frozen identities match; the withdrawn worker feedback is not used. Release
preparation has started via `prepare_a2_cast_record.py` (29 stages).
Concrete timer review refines acceptance before timing: seven stable prepared
native query families do not execute changed registry construction/selection in
their timed interval, so prior native evidence is retained. Every process family
includes startup and needs fresh evidence. Exact scope and commands are in
`a2-cast-record-acceptance-scope.md` and
`remaining-performance-cast-record-candidate1.json`: compiled5, proxy5, CSV2
plus all six prior process families, then pending B1/COUNT if independent gates
pass. This broadens affected process coverage; no fixture/gate change or failure
is waived. Old provisional native CSV/F1 rerun clauses are superseded by the
explicit timer/consumer analysis, not by a new measured pass.
Cast/Record release preparation passes stages1–18, then release upstream
`integer_try_cast.test` stops after20 records at FLOAT output near i32 limits:
expected integer-form values versus worker shortest-FLOAT rendering. Preserve
`a2-cast-record-preparation/run1/report.json` and the failed upstream report.
Focused diagnosis confirms both pinned C++ cases pass (83/82 assertions), and
current compiled Rust passes the same39/38 records. This isolates missing Python
numeric-oracle behavior; production cast results and221 Rust checks remain valid.
The typed FLOAT/DOUBLE Python comparison now matches both pinned references,
including f32 rounding, actual-based epsilon, NaN and finite-text overflow to
infinity. Configured verification passes76 Python tests with frozen source
identities (`a2-proxy-float-oracle-verifier/`); native boundary and overflow
witnesses are retained under `a2-float-oracle-boundaries/evidence/`.
Continuation1 passes all ten actual proxy invocations and reaches the next
upstream representation gap at record23: BOOLEAN `True`/`False` versus `1`/`0`.
Its failed report remains at `a2-cast-record-preparation/continue1/`. Typed
BOOLEAN matching now follows the pinned helper; continuation2 passes all eight
selected upstream cases, ten proxy invocations and nine compiled fixtures.
Review then catches a mixed-type `valuesort` ownership bypass in the new Boolean
fallback. The final guard and negative regression pass78 Python feedback tests
(`a2-proxy-bool-oracle-feedback2/`) and are delegated for final scoped verification.
Continuation2 remains evidence only for its recorded inputs; changed Python
cases replay in continuation3 while unchanged compiled fixture evidence is
retained. Configured BOOLEAN/FLOAT verification now passes78/78 with stable
identities (`a2-proxy-bool-oracle-verifier/`), and continuation3 passes all twelve
affected Python/upstream stages. Combined preparation receipt is
`a2-cast-record-preparation/final1.json`; the current engine source digest is
`6df7a5907cc8af009aeb4229b39040fede402bdcb1791e4deb42fd7dd5e09ea7`.
The cast/Record candidate1 campaign at `9e5418f` passes compiled A2 five of five
and all seven affected compiled process commands, including both CSV workloads
and regex. Compiled A2 latency ratios are0.298/0.148/0.953/0.871/0.583; CPU,
RSS, I/O and throughput gates pass independently. The actual Python proxy fails
four of five aggregate gates: small/lifecycle/readonly latency and small/large
RSS remain over the faster reference. Preserve all samples and final exit1 in
`remaining-cast-record-candidate1-stage-logs/`; pending B1/COUNT did not run.

The bounded startup repair at baseline `9e5418f` owns
`scripts/{measure_sqllogic_proxy,proxy_once_core,secure_scratch,worker_protocol,
run_upstream}.py`, their four affected test files and `test/runner/worker.rs`.
It adds a strict attested once-job entry, descriptor-relative scratch storage,
public POSIX spawn with a Popen fallback, bounded request writes, and a flagged
worker cwd/ready contract. Existing unflagged worker behavior remains covered.
The reviewed partial manifest is `target/batch2/a2-startup-integration-manifest.md`;
exact commands, upstream IDs, source/helper hashes, exclusions and unchanged
five-workload performance obligations are recorded there and in
`a2-startup-frozen-inputs.json`. Configured `a2-startup-verifier1/` passes148/148
Python,6/6 worker,1/1 temporal tests, production Clippy, formatting, coverage with
no missing instrumentation, and trace checks with stable frozen inputs. Full
engine, recovery and Kani suites are not applicable: native durability and
maintained proof invariants are unchanged. Release preparation builds the worker successfully, then actual standalone
replay catches a string-to-Path provenance boundary error. Preserve that failure
in `a2-startup-preparation/run1/`. The correction and a descriptor double-close
regression pass150/150 configured Python checks with stable inputs in
`a2-startup-verifier2/`; unchanged Rust checks/build remain valid. Exact delta
manifest and identities are `a2-startup-correction-manifest.md` and
`a2-startup-correction-frozen-inputs.json`. Run2 passes all41 stages:30 actual
proxy invocations (five workloads across three entry modes and two transports),
both pinned upstream selections (eight files, zero skips), two stale-attestation
negatives, five invalid worker-argument cases, flagged zero-query readiness and
unflagged query42. It retains the validated release build from run1. Source,
helper, driver and fixture hashes stay stable; performance remains open pending
the fresh startup proxy campaign. The compiled runner, shell and native engine are unchanged, so their
recorded acceptance evidence is retained rather than rerun for an aggregate
worker source hash change. No workload, assertion or resource gate is reduced.
Startup candidate1 at `28df69a` passes both large workloads and every peak-memory
gate, but short scalar/lifecycle/read-only latency ratios remain1.343/1.664/1.640.
Small scalar is34.048ms versus25.344ms; lifecycle64.244ms versus38.600ms;
read-only58.346ms versus35.570ms. Small and lifecycle CPU also fail. Preserve
`a2-startup-proxy-candidate1.json` and its stage exit1; B1/COUNT remain pending.
Focused exact-job import/request profiles (`a2-startup-profile1/`,
`a2-operation-profile1/`) identify interpreter/import overhead plus durable
load/commit latency. Alternate installed pyenv/Homebrew/uv runtimes do not
materially improve diagnostics. Native source review rejects lazy empty-open,
weaker staged-file flushes and combining the two implicit transactions in
`CREATE; INSERT`: both pins require the relevant independent acknowledgments.
These reviews are not new correctness or performance passes.

A tiny strict-job executable avoids compiling the full campaign source in each
child. It delegates the unchanged core/Runner, preserves the public entry, and
adds the new helper to identity snapshots. Manifest `a2-tiny-entry-manifest.md`
at baseline `28df69a` owns only `scripts/measure_sqllogic_proxy.py`,
`scripts/sqllogic_proxy_once.py` and `scripts/test_measure_sqllogic_proxy.py`.
Configured `a2-tiny-entry-verifier1/` passes64/64 affected Python tests, syntax
and diff checks with stable `a2-tiny-entry-inputs.json`. Actual replay passes
all16 stages in `a2-tiny-entry-preparation1/report.json`: ten fixed workload
invocations across spawn/Popen plus six malformed/argument/stale-attestation
negatives. Existing public/upstream and Rust evidence remains at its unchanged
inputs. Diagnostic entry savings are roughly3–4ms, not acceptance. A target-only
standard static CPython3.11.4 build is being investigated as a separate runtime
configuration; no installed interpreter is replaced and no prior failed runtime
gate is converted to a pass. Final tiny-entry campaign at `d608b5a` passes both large workloads and all
memory/I/O gates; small CPU now passes. Short scalar/lifecycle/read-only latency
still fails at31.399/60.090/54.914ms versus25.862/38.352/35.566ms; lifecycle CPU
is30ms versus20ms. Preserve `a2-tiny-entry-proxy-candidate1.json` and stage exit1.
Static CPython and removal of its unused direct CoreFoundation link pass bounded
runtime smoke but do not show sufficient diagnostic improvement; neither is
adopted as the project interpreter. Default-runtime proxy parity remains open.
Batch ordering is unchanged pending the user's response to whether the next
implementation batches may proceed while these acceptance gates remain open.
Within Batch2, ROOT now collects the already-authorized independent B1 and COUNT
acceptance stages while A2 remains failed. This scheduling change supersedes the
prior stop-before-B1/COUNT clause only; each stage is still fail-fast and no pass
can compensate for open A2. Reviewed exact commands/settings/outputs are copied
unchanged into `independent-b1-count-performance-manifest.json`, with frozen
input checks and receipts in `independent-b1-count-stage-logs/`. The fixed10k
seed, references and all resource thresholds are unchanged. No new engine
implementation or full sweep is introduced by that scheduling change.
The independent campaign at `d608b5a` completes with stable inputs and exit1:
all five release B1 native cases fail (ratios2.31/2.47/22.33/11.34/3.33), so
development/joint/process stages remain unexecuted. B1 durable WAL view/direct
cases pass (0.855/0.965); checkpoint view/direct fail (1.111/1.197), with
CPU/RSS/I/O passing. COUNT native query passes both pins (0.101/0.087), but
nullable-VARCHAR CTAS process fails at31.016ms versus27.382ms and CPU20ms versus
10ms; DISTINCT-star rejection passes. Preserve the three independent stage
receipts, all failed samples, and unchanged input hashes under `target/batch2/`.
Follow-on Batch2 repairs investigate actual measured execution: BIGINT-to-text
totality for CASE column evaluation, immutable view payload sharing, and stacked
physical projections. The proposed expression-only add-spine optimization was
withdrawn because separate view projections never reach it; conditional removal
of non-journal catalog basis was rejected because it changes committed-row
constraint checks. Their target-only designs are not passing implementations.
The integrated follow-on repairs now pass configured partial verification:
casts20, numeric contracts9, execution118, views11, contracts108, operators10,
grouping84, regex2 and three exact ALTER consumers (365total). Clippy first
caught one unused test import; its removal refreshed casts and passed lint,
coverage with no missing instrumentation, and tracing. Preserve verifier1's
failed lint result and compose unchanged functional evidence with verifier2's
correction; source inputs are stable. Release build, the new14-record Rust/27-assertion-per-pin witness, and all eight
native view exchange cases pass. The existing whole string-cast file still fails
at its first hexadecimal-to-INTEGER case; the attested `add001f` worker reproduces
the identical error. Its unchanged applicable formatting prefix and the four
other selected files pass. Preserve the full-file failure and unreached cases;
`native-repairs-hex-scope-resolution.md` records this pre-existing G04 gap.
COUNT now passes both native references and every process/resource gate:
nullable CTAS19.760ms versus27.077ms (ratio0.730), CPU10ms versus10ms, and lower
peak memory. B1 stacked views improve to212.375us versus271.292us on release,
but filtered view/direct, direct arithmetic and CREATE VIEW still fail at
ratios2.307/2.503/3.852/2.811. Development/joint/process B1 stages did not run
after that failure. Durable and string/regex consumer acceptance remains open.
Evidence: `native-repairs-preparation2/report.json`,
`native-repairs-performance-stage-logs1/`, and their exact source/input hashes. Pre-execution upstream scope review restored the exact previously
accepted B1 view-if-not-exists/alias/stacked cases, alongside unchanged cast cases
and a new14-record witness; unrelated introspection/deferred-binding settings
remain unimplemented, not passed. Frozen commands, source/input hashes and
scope rationale remain under `target/batch2/native-repairs-*`.
At baseline `207a221`, the next repair specializes checked BIGINT operations once
per column, accesses selected/chunked physical values directly, and derives one
CREATE VIEW catalog successor for the proven-equal transaction/basis pair. The
new non-journal UPDATE/DELETE → CREATE VIEW → ALTER NOT NULL case checks that
committed-row constraint semantics survive. Configured partial verification passes317 tests, including the whole numeric
 target's existing batch consumers. An initial unit incorrectly demanded known
 ordering from BIGINT chunks; it now verifies the preserved unknown metadata.
 Scoped Clippy then rejected four needless closure borrows. The correction
 passes10 refreshed arithmetic tests plus scoped Clippy, coverage with no missing
 instrumentation, and trace checking with no incomplete/error returns. Compose
 `native-followup2-verifier2/` with `native-followup2-verifier3/` at their recorded
 inputs; both earlier failures remain preserved. Release/performance is pending.
A separate Python-only repair preserves every SHA-256 input using the standard
CPython constructor with public fallback, and calculates SQLLogic MD5 only for
hash/label oracles. Diagnostic fresh-process import savings are about3ms;
`a2-proxy-sha-implementation-manifest.md` requires actual strict-entry replay and
 the unchanged five-workload performance gate. Its configured151-test Python
 sweep first passed150 and exposed an outdated source-identity fixture omitting
 eight existing utf8proc inputs. The corrected fixture passes its exact test,
 checks the full dependency set and fingerprint sensitivity to every file;
 production input selection stays unchanged. Compose `a2-sha-verifier1/` and
 `a2-sha-verifier2/` at their recorded inputs. The follow-up is committed as `8c0c7a3` (native) and `a93563f` (Python).
 Release preparation passes all10 stages, including both-pin selected replay,
 the14-record witness, native view exchange and B1/C2/regex process witnesses.
 All16 strict proxy-entry replays pass with fresh worker provenance.
 B1 release now measures filtered view/direct772.167/542.917us versus421.291/
 269.041us; stacked direct270.167us versus73.167us; CREATE VIEW75.416us versus
 42.291us. Those four fail; stacked views208.708us versus266.167us passes release
 only. B1 development/joint/process did not run after the release failure.
 Python SHA candidate passes both large cases and every CPU/RSS/I/O gate;
 short scalar/lifecycle/read-only latency still fails at27.851/57.574/50.976ms
 versus25.180/37.706/34.980ms. Preserve `b1-native-release-native-followup2.json`,
 `a2-sha-proxy-candidate1.json` and their failed stage receipts.

 The next profile-supported repairs at `a93563f` specialize endpoint-proven
 ordered arithmetic, validate a dependency graph once per catalog validation,
 and remove eligible BIGINT checkpoint row/value copies. They own disjoint
 arithmetic, catalog and checkpoint paths and remain incomplete pending their
 new partial manifests. The catalog shared-validation change broadens affected
 process acceptance to COUNT, compiled A2, prior seven process families, B1
 durable/native/process and A2 proxy; do not retain now-affected earlier passes.
 Native query evidence with only unchanged timed consumers remains at its prior
 source identity. New native profiles are `b1-native-followup2-profiles1/` and
 `b1-checkpoint-profile-followup2/`. The latter's diagnostic wrapper compared a
 validating function's None return to a dict; preserve its false status and the
 separate correction verifying actual exit0/sampler0 and unchanged seed/checksum.
 These profiles are not performance acceptance, and sync durability is unchanged.
 The integrated partial sweep passes560 functional cases: arithmetic/catalog,
 packed checkpoint exact bytes and guards, native versions/services, numeric,
 execution/operators/grouping/regex, views/contracts/ALTER, checkpointing,
 recovery13 and logging17. Initial compilation exposed a missing import/type
 annotation; later scoped Clippy exposed two equivalent writer cleanups.
 Preserve `native-followup3-verifier1/` failures, `verifier2/` functional560,
 and `verifier3/` refreshed13 writer/version/service tests plus passing Clippy.
 Coverage then identified seven missing cfg(test) instrumentation attributes;
 their attribute-only correction retains no-default functional evidence and
 passes coverage (missing[]) and trace (completed1, no errors/incomplete/panics)
 through `native-followup3-verifier4/`. The configured partial sweep is passed.
 Source539, baseline-matching fixture155 and vendored dependency8 identities
 are recorded in the corrected freeze and its input addenda. Committed as
 `241e68a`; all10 release-preparation stages and16 strict proxy replays pass.
 Final21 performance remains failed: B1 release filtered view/direct406.167/
 256.542us, stacked views213.750us and CREATE VIEW35us pass release only;
 stacked direct88.250us versus72.792us fails1.212359. Development/joint/process
 did not run after that failure. Fixed10000-row checkpoint view/direct44.562/
 50.560ms fail1.040919/1.151845 against the faster pin; WAL view/direct35.869/
 41.476ms pass0.850804/0.975163. All durable CPU/RSS/I/O gates pass.
 A2 proxy short scalar/lifecycle/read-only27.950/56.656/51.235ms fail1.117378/
 1.504314/1.460568; both large cases and all resource gates pass. Preserve
 `native-followup3-performance-stage-logs1/` and linked raw samples. Other
 now-affected process/native acceptance remains open, not inherited as passing.

 Followup4 at `241e68a` changes six files: `src/function/aggregate.rs`,
 `aggregate/exact.rs`, `src/common/vector.rs`, `src/storage/table.rs`,
 `table/rows.rs` and `src/storage/duckdb/writer.rs`. The ordered BIGINT SUM
 proof bounds every partial sum before choosing narrow reduction; unknown
 order/full-width cases retain i128 fallback. Checkpoint encoding now borrows
 eligible sliced BIGINT Chunks, using bounded scratch only across child
 boundaries, with exact native byte/successor/row witnesses at both versions.
 Scoped verifier passes493 selected cases plus lint/coverage/trace with stable
 source539/fixture155/dependency8 identities before and after. Coverage reports
 missing[]; trace completed1 with no errors, incomplete spans or panics.
 Evidence: `target/batch2/native-followup4-verifier1/`. Release acceptance
 remains pending; this ordinary pass does not close any performance gap. Reviewed ownership,
 commands/counts, unchanged upstream cases, consumers, performance obligations,
 exclusions and input identities are in `target/batch2/native-followup4-integrated-manifest.md`,
 `native-followup4-ordinary-commands.json`, `native-followup4-frozen-inputs.json`
 and `native-followup4-performance-manifest.json`. Follow AGENTS.md validation
 scope; full-engine/exhaustive framing/Kani/Python suites are not applicable
 for the bounded unchanged contracts described in that manifest.
 Additional unchanged BIGINT SUM upstream records are selected byte-for-byte
 from both pins; full upstream SUM remains blocked on unrelated DOUBLE ordered
 SUM. Expanded native/process SUM consumer gates are owed on the final tree.
 Diagnostic profiles identify signed SUM reduction and generic Chunks copying;
 the no-site Python experiment saved too little and was not adopted. Both C++
 pins commit each statement separately, as Rust does; no transaction merging
 or weaker sync is supported. Batch2 remains incomplete.


| Batch | Tracks | State / restart point |
| --- | --- | --- |
| 1 | A1 census; F1 table-function lifecycle; C2.1 STRING_AGG | Complete: A1 evaluation accepted at `44538fb`; F1 accepted through `324878c`; C2.1 accepted through `6d221b8`. Independent partial verification passed, affected dense-offset proof passed, and all affected native/process gates pass (C2 candidate10 plus unchanged five-family final21 evidence). Earlier failed samples remain preserved. Restart at Batch 2, not another Batch 1 sweep. Worker branches/worktrees remain `codex/batch1-{a1,f1,c2-1}` / sibling `../duckdb-rust-batch1-{a1,f1,c2-1}`; integrated root contains accepted follow-ups. |
| 2 | A2.1 single-iterator comma-value regression; F2.1 explicit-schema CSV read; B1 persistent views | Active: `241e68a` passes560 scoped functional cases, release replay and16 strict proxy entries. B1 release now passes four of five native cases; stacked direct and both checkpoint latency cases fail. WAL cases, both large A2 proxy cases and their resource gates pass; three short proxy latency cases fail. Followup4 SUM/Chunks repair passes493 scoped tests with expanded consumer acceptance still pending. Affected startup/process refreshes remain owed; only unchanged evidence retains its original identity. The hexadecimal cast and unrelated DOUBLE ordered SUM gaps stay explicit. |
| 3 | E1 byte reservations; A4 incremental regression accounting; C1.1 STRUCT regex extraction | Queued after batch 2 acceptance; if A4 was accepted as batch 2's fallback, reuse that evidence and select its next measured engine-accounting leaf rather than repeat it. |
| 4 | D1 row/catalog conflict semantics; H1.1 local filesystem contracts; F2.2 COPY CSV writer | Queued after batch 3 acceptance. One transaction/publication owner; filesystem and writer proposals integrate serially at shared I/O boundaries. |
| 5 | E2 buffer ownership/native scan integration; F3.1 scan pushdown/residuals; B2.1 persistent scalar macros | Queued after batch 4 acceptance. E2 consumes E1/H1.1; F3.1 consumes accepted F2.1; B2.1 consumes B1/D1. |

**Immediate carryover, before Batch 3:** finish the frozen `241e68a` followup4
SUM and chunked BIGINT checkpoint repair using its reviewed partial manifest.
Ordinary verification passed; build release artifacts, replay scoped upstream and
native/proxy witnesses, then run quiet serial short performance gates before
long consumer campaigns. Catalog startup changes still require all affected
process refreshes; the SUM repair additionally requires its named consumers.
Retain only unaffected evidence at its original inputs. Short A2 proxy and B1
native/checkpoint failures remain open. User paused activity; leave apps running.
Batch 3 implementation follows Batch 2 acceptance pending the user's response
on changing that order.

**Batch 3 — memory budgets, regression accounting and structured regex results.**

- E1: introduce fallible byte reservations, ownership/release and memory-setting
  propagation through vectors and one existing operator. Exercise allocation
  failure, overflow, cancellation and results retained after connection closure.
  Proposed ownership: `src/parallel/` and resource tests; lead integrates
  `src/common/vector.rs`, settings and the chosen operator. This is the first
  resource-accounting slice, not complete allocator/spill parity.
- A4: extend report reconciliation into a case/pin/configuration regression view
  with lost passes, new failures, stale evidence and elapsed stages. Own
  `scripts/summarize_upstream.py` and related Python tests/adapters. Preserve
  population identities, reject duplicate/omitted selections and support a pin
  with no timeout retries. Existing `scripts/upstream_regression.py` is a paired
  deadline diagnostic, not this incremental accounting feature.
- C1.1: implement named-group STRUCT regex extraction, including field names and
  result types, optional/unmatched groups, NULL/error behavior, prepared use and
  encoded vectors. Include the related named `regexp_extract_all` LIST(STRUCT)
  overload present in both accepted pins. Named patterns and name lists must be
  constant; dynamic named arguments are bind-error cases. Own
  `src/function/scalar/text/regex.rs` and focused regex
  tests; lead integrates return-type binding. Preserve extract-all/replacement
  and nested-value consumers; scalar/table splitting remains later work.
- Acceptance workloads: reservation overhead on existing scan/group/CSV paths
  and failure cleanup; small/large report reconciliation and selected-feedback
  consumers; constant-pattern STRUCT/LIST(STRUCT) extraction plus existing
  dynamic scalar-group regex and nested paths.
  E1 and C1.1 have separate shared-vector proposals integrated by the lead.

**Batch 4 — concurrent writes, local I/O and CSV export.**

- D1: replace blanket intervening-writer conflicts with the pinned row/catalog
  conflict and visibility contracts. Own `src/transaction/` and deterministic
  transaction histories; lead integrates catalog/storage publication seams.
  Cover disjoint and overlapping writers, DDL races, old snapshots, rollback and
  durable publication. Only this track owns transaction state-machine changes.
- H1.1: generalize local sequential/range I/O, cancellation and failure contracts
  through an existing native-storage consumer. Own `src/storage/filesystem/`
  and its module/tests; lead integrates native callers and publication overlap
  with D1. Cover short reads/writes, locking and failed-publication cleanup.
  Remote storage, globbing and compression remain later leaves.
- F2.2: implement single-file local `COPY ... TO` CSV with quoting, NULL/newline
  semantics, bounded output and cleanup after failure/cancellation. Own the CSV
  writer and its tests; lead integrates parser/binder/execution dispatch. Use
  the existing local adapter until H1.1 integration, with independent C++ reads
  of Rust output and Rust reads of reference output.
- Acceptance workloads: disjoint/contended writes and retained readers; native
  sequential/random I/O and publication; narrow/wide, quoted/NULL CSV output
  plus unchanged CSV reads. Validate conflict outcomes and output before timing.

**Batch 5 — native buffers, selective scans and reusable SQL expressions.**

- E2: add buffer pin/unpin, eviction, dirty-state lifetime and I/O attribution
  through native block scans. Own a dedicated storage-buffer module and tests;
  lead integrates native readers, E1 reservations and H1.1 I/O. Cover pinned
  blocks, read/write/eviction failure and transaction/result ownership.
- F3.1: add projection/filter pushdown with correct residual predicates to the
  accepted CSV/table-source path. Own table-function scan negotiation and CSV
  consumers; lead integrates binder/physical-plan seams. Preserve custom
  adapters, NULL/error behavior, prepared reopen and cancellation. Measure actual
  parsing/materialization and I/O savings; do not infer skipped bytes from a
  pushed predicate. Multi-file discovery and sniffing remain separate leaves.
- B2.1: implement persistent scalar macros with capture, qualification,
  default/named arguments, dependency/transaction lifecycle and native reopen.
  Own macro catalog/binder modules and focused tests; lead integrates shared
  catalog and native codecs. Cover recursion, shadowing, lazy errors, prepared
  invalidation, rollback and bidirectional checkpoint/WAL exchange. Table macros
  remain B2.2.
- Acceptance workloads: below/above-cache sequential/random native scans;
  selective/wide CSV scans with residuals and unchanged full reads; repeated
  scalar-macro expansion/query, DDL and durable reopen. Preserve view/default
  consumers when changing catalog binding or native representations.

**Queue validation and readiness:** all nine leaves are queued, with functional,
performance and scoped-sweep acceptance open; Kani applicability is determined
from each frozen impact manifest. Proposed ownership above is a planning boundary,
not an executable handoff. Before each dispatch, resolve the actual integrated
baseline, exact owned paths, commands/targets/filters, both-pin unchanged case IDs,
negative/boundary cases, existing consumers and workload/adapters in the
[handoff template](#handoff-template). Missing coverage is an obligation to fill,
not a pass. Default to partial verification under the
[durable policy](../AGENTS.md#validation-scope--durable-policy); expand recovery
and maintained proofs only for affected contracts. A4 uses Python checks, without
an engine build for Python-only edits. Each implementation leaf needs its own
applicable functional, delegated verification and faster-of-both-pins performance
acceptance, including CPU/memory/I/O costs. Serialize final timing across all
worktrees, and stop adding implementation when two chunks await acceptance.

This queue edit owns only `docs/parity-backlog.md`. Its validation is reviewed
diff, `git diff --check`, local-link/anchor and queue/dependency consistency checks.
Engine tests, verifier, recovery, Kani and performance are **not applicable:
documentation-only**; existing engine performance failures remain open.

Checkpoint contract: update this table and each affected entry in place with
implementation/functional/performance/scoped-sweep outcomes; integrated commit,
worker branch/worktree, exact commands/case IDs and reports under `target/`;
accepted work, open failures, next command and prerequisites. Commit each batch
checkpoint so interruption or exhausted credits leaves an unambiguous restart.
Never use a status-only commit as a trigger to rerun engine validation.

Initial classification: this dispatch/policy edit is documentation-only; validate
diff/links/consistency only. Worker manifests must be recorded before executable
changes. Each implementation track uses scoped checks and both-pin affected
functional/performance acceptance. A1 is read-only evaluation, including its
explicitly assigned broad census, not a full-engine regression gate on F1/C2.1.
Reserve quiet host windows for all final performance measurements.

Batch 1 evidence/restart details:

- A1 owns `../duckdb-rust-batch1-a1/target/a1-20260918/`: both-pin full
  `run_upstream.py --timeout 10 --jobs 4`, exact timeout retries at 60 seconds,
  then accounted summaries. Original 34 workloads are the existing native (12),
  numeric (8), relational (10), grouping (3) and ordering (1) manifests; paired
  21-sample paired measurements completed serially in a coordinated quiet-host
  window. Evaluation failures remain visible, not a claim of engine acceptance.
  Exact commands/restart details: worker `target/a1-20260918/manifest.md`.
  Functional evaluation is complete at `44538fb`: after exact retries of 13
  timeout IDs per pin, development passes 587/5,637 executable files and release
  passes 553/4,834; 7/6 timeouts remain respectively. The binary fixture
  `data/parquet-testing/orders_small_parquet.test` is accounted separately,
  explains raw denominators 5,638 / 4,835. Compared with historical effective
  counts, development gains 64 and loses 5 prior passes; release gains 334 and
  loses 2. See worker `target/a1-20260918/census-report.md` and `summary.json`
  for exact populations/provenance. This is the pre-batch engine, not acceptance
  of F1/C2.1. Original34 latency evaluation completed: 25 pass / 9 fail.
  Failed faster-reference ratios: decimal grouped cents 2.060x, decimal equality
  join 3.072x, USING inner join 1.830x, NATURAL semi join 1.899x, partition SUM
  window 1.054x, QUALIFY window 1.012x, grouped SUM 1.706x, ROLLUP SUM 1.098x,
  CUBE SUM 1.885x. Native 12-case and ordering manifests pass. Exact workload
  IDs, both reference identities and samples are in worker
  `target/a1-20260918/performance-report.md` and the `perf-*-{release,development,fastest}.json`
  files. Resource/configuration coverage beyond those manifests remains open.
  A1 implementation=N/A (evaluation only), functional evaluation=complete,
  performance evaluation=complete (25 pass/9 fail, not whole-engine parity),
  implementation sweep/Kani=N/A. No extra engine sweep is required for evaluation.
  Six lost pin/file passes reach the same comma-substitution runner bug:
  `list/aggregates/incorrect.test` and `numeric/test_trunc.test` on both pins,
  plus development `numeric/test_trunc_precision.test` and `float/nan_cast.test`.
  `scripts/sqllogic.py::bind_loop` and `test/runner/schedule.rs::replace_loops`
  split values unconditionally, unlike development
  `test/sqlite/sqllogic_test_runner.cpp::ReplaceLoopIterator`, which splits only
  tuple iterator names. Queue A2.1, preserving actual source paths from the
  report. The remaining development loss `test/issues/rigger/rowid_conjunction.test`
  exposes unsupported `NOT SIMILAR TO`; it remains an engine follow-up, not a
  silently waived passing case.
- F1 owns table-function modules/tests and provisional shared table-plan seams
  on its branch. Reviewed scope includes registry/bind/schema/per-open scan and
  exactly-once cleanup, integer range extremes/NULLs, prepared reuse and affected
  existing consumers. Keep legacy public `PlanNode::Range` compatible. Temporal
  and correlated range cases remain separate obligations; preserve source IDs.
  Partial acceptance uses the new `table_functions` target, affected execution/
  FROM-first/registration contracts, selected upstream range/error files, scoped
  lint/tracing, and new native/process range workloads. Recovery/Kani: N/A to
  these contracts, not passed. Worker handoff `261100a` integrated as `449d92c`;
  follow-up commits must not amend that handoff. Detailed case/command records
  are in worker `target/f1/validation-manifest.md`. The independent scoped
  verifier `batch1_f1_scoped_verifier` owns integrated checks/logs under root
  `target/batch1/f1-verifier/`; no full-engine/recovery/Kani run was dispatched.
  Ordinary selected upstream acceptance now matches debug feedback: 36 integer
  records pass per pin; the final correlated record at line 226 remains the
  declared C6.1 gap, not a whole-file pass. Exact remaining source/configuration
  blockers are in worker `target/f1/ordinary-mapped-open.json`; integer evidence
  is `target/f1/ordinary-integer-selected.json`. All four native workloads have
  correct rows/checksums on Rust and both pins; both process fixtures pass
  64/128 records respectively on all three engines. These are untimed correctness
  checks, not performance acceptance. The independent partial sweep passed
  against `449d92c`: table-functions 6/6, execution 108/108, FROM-first 2/2 and
  two contracts 1/1 each; formatting, scoped Clippy, instrumentation coverage
  (no missing items) and trace compatibility passed. Later commits through
  `2069859` touched documentation only and did not invalidate those checks.
  Quiet-host performance diagnostic (9 samples, not final acceptance) is complete:
  three native workloads pass, but `range_aggregate_1m` fails at 4.73–4.75x
  (Rust 4.91–4.94 ms versus faster-reference 1.039 ms). Both process workloads
  initially passed the numerical diagnostic gates, but later review found
  unmatched thread settings; those process results are not acceptance evidence.
  Raw reports are worker
  `target/f1/performance/*-diagnostic-9.json`. S agent
  `batch1_f1_performance` owns a measured correction on the F1 branch; preserve
  `261100a` and add follow-up commits. Candidate 1 removed redundant checked
  vector construction but still failed at 2.80–2.85x. Candidate 2 computes the
  batch cardinality once, fills a proved BIGINT progression with cancellation
  checks at most 1,024 rows apart, and preserves the authoritative i128 state.
  Its fresh 9-sample diagnostic passes all four native workloads: aggregate
  0.851–0.877 ms versus faster-reference 1.024 ms (0.832–0.857x). No shared SUM
  kernel change was needed. Reports are `*-candidate{1,2}-9.json` in the same
  directory; original failed samples remain preserved. Candidate 2 is committed
  as worker `ccb0100` and integrated as `324878c`. Its correction manifest is the
  top section of worker `target/f1/validation-manifest.md`, also available at
  `target/f1/performance/range-correction-validation-manifest.md`. Refreshed independent partial
  verification passed at `324878c`: table-functions 7/7, two affected execution
  filters 1/1 each, scoped Clippy/format/coverage/trace; root evidence is
  `target/batch1/f1-range-verifier/`. Fresh ordinary source selection retains 36
  integer passes per pin and the declared correlated failure. Development's
  initial two 10-second timeouts are preserved as incomplete; an isolated exact
  60-second retry with the same source/worker established the 36 passes. Reports
  and identities are linked from the correction manifest. Final integrated
  performance at `90d63ab` now passes: both pins, 3 warmups/21 samples, all four
  native cases and both matched-serial process cases. Native ratios versus the
  faster pin are scan 0.174–0.184x, million-row aggregate 0.851–0.855x, early
  LIMIT 0.236–0.313x and reverse prepared reopen 0.266–0.272x. Process wall
  ratios are 0.339x/0.316x; CPU/RSS/I/O/throughput gates also pass (CPU recorded
  at the platform timer's coarse resolution, not zero actual CPU). All engines
  validate 64/128 records respectively. Root evidence and exact identities:
  `target/batch1/f1-final/{manifest.md,native-release.json,native-development.json,native-fastest.json,process-serial.json}`.
  **F1's declared lifecycle/integer-range slice is accepted.** Reuse unaffected
  scoped correctness and upstream evidence; temporal/correlated sources and
  whole-engine parity remain open. Do not remove/redefine failing workloads.
- C2.1 is accepted through root `6d221b8`, with implementation, independent
  partial functional verification and affected performance gates passed. Functional chain:
  worker `1413b21` → root `6258294`; candidate9 worker `94dc0f2` → root
  `8e27760`; dense-index boundary correction `b72ea46`; lint/format/tracing
  attribution corrections through `5c10878`. Worker branch/worktree remains
  `codex/batch1-c2-1` / `../duckdb-rust-batch1-c2-1`; use integrated root for
  final measurements because the worker retains the older F1 range implementation.

  Scope: STRING_AGG/group_concat/listagg with bound constant separators,
  grouped/ungrouped and window consumers, ORDER/DISTINCT/FILTER and retained
  selected adapters. NULL separators preserve the pinned no-input-evaluation
  behavior; mutable separators are rejected. Development is authoritative for
  the documented unary ORDER-expression pin disagreement. Optimizations include
  capability-selected VARCHAR DISTINCT/owned LIST completion, bounded integer
  group lookup, validated LIST(VARCHAR) payloads and additive aggregate column
  transport preserving legacy row callbacks. No unchecked constructors or
  function-name routing. Foreign compatibility stays deferred.

  Original unchanged `test/sql/window/test_window_string_agg.test` passed
  development 4/release 5 at `e94a022`, with no unreached records; unchanged
  binder/window/scalar inputs reuse
  `target/batch1/c2-window-upstream/{original-window.json,validation-manifest.md}`.
  Default aggregate/window source replays are covered by the grouping target.
  Do not claim original aggregate/distinct files pass: forced external/parallel
  configurations, positive settings and correlation/list-position prerequisites
  remain E3/E5/B4/C6 obligations. Exact pin-specific IDs/oracles and blockers
  remain worker `target/c2-1/source-case-map.json` and `validation-manifest.md`.

  Independent partial verification is green: grouping 81, window consumers 10,
  index 14, STRING_AGG leaf 7, owned LIST 3, typed key 7, custom transport 3 (inside
  grouping), and the exact single-case publication/vector/nested/regex filters
  listed in `target/batch1/c2-final/scoped-verification.md`. Scoped Clippy,
  formatting, coverage (`missing: []`) and trace compatibility passed.
  Cumulative evidence combines unchanged ordinary inputs at `cee3281` with
  the test-only tracing annotation at `5c10878`; logs/identities/timings are
  `target/batch1/c2-final/verifier*`. Initial lint, formatting, zero-test and
  fixture failures remain recorded, not counted as passes. No full-engine or
  recovery sweep ran.

  Boundary review reproduced HUGEINT dictionary growth whose spare geometric
  capacity overflowed the dense interval. Correction `b72ea46` preserves
  `minimum + width - 1` within i128; all 14 index tests pass. The one affected
  dense-offset Kani harness passed at that revision with pinned 0.67.0:
  232 properties, zero failures, two unreachable. Unsupported-construct warnings
  remain recorded. This proves the selected offset contract under its stated
  interval assumption, not all grouping or construction behavior. Later
  unrelated changes reuse the proof; unrelated harnesses are not applicable.

  Final quiet-host performance campaign completed on integrated root `5c10878`: six
  affected native/process families—C2 STRING_AGG, grouping SUM/ROLLUP/CUBE,
  G08 filter, G08 ordered aggregate, F1 range publication, and regex extract-all
  LIST(VARCHAR). Regex selects exactly the two maintained constant/dynamic native
  cases and one process entry from `g06_1g_1h_2c`; structurally checked subset
  JSON is `target/batch1/c2-final/regex-{native,process}-workloads.json`.
  Untimed expected record counts are C2 257, grouping/G08 filter/G08 ordered 385
  each, F1 64+128, regex 129. Preserve all SQL/checksums/settings and input hashes.
  Exact build, both-pin 21-sample native/joint and matched-serial process commands
  are `target/batch1/c2-final/performance-manifest.md`; reports stay beside it.
  All latency/throughput and CPU/RSS/I/O gates must pass independently.
  Final21 results: 22/23 native cases pass. Only `string_agg_few_groups` fails:
  Rust release/development campaigns are 819,708/856,958 ns versus the common
  faster C++ release median 854,917 ns (0.958816x/1.002387x). The development
  C++ median is 1,258,167 ns; it is not the governing baseline. The other four
  C2 cases and all five other native families pass. All six process families
  pass wall/CPU/RSS/I/O gates, including completed regex timing. Full results:
  `target/batch1/c2-final/performance-results.md`. Preserve the failed q2.
  The focused q2 profile supports append/copy work as the next experiment,
  without proving separator copying dominates. Candidate10 `6d221b8` changes
  only grouped Heap append: safe `String::push` for one-byte ASCII separators,
  existing `push_str` otherwise. Scalar/inline/promotion/capacity paths stay
  unchanged. Independent partial delta passed at `6d221b8`: leaf 8/8,
  grouping 81/81, formatting, scoped Clippy and tracing compatibility. Exact
  commands and provenance: `target/batch1/c2-final/candidate10-verification.md`
  and `verifier-candidate10/`. C2-only final measurement passed under
  `candidate10-performance-manifest.md`: all five native cases against both
  pins, untimed 257/257 records with zero skips, and every matched-serial
  process wall/CPU/RSS/I/O gate. Three warmups/21 samples; fresh release build
  with no default features or tracing. Native ratios versus the faster pin:
  ungrouped 0.663–0.669x, few groups 0.860–0.868x, many groups 0.937–0.942x,
  ordered DISTINCT 0.731–0.754x, mixed SUM/LIST 0.913–0.920x. Reports are
  `native-c2-{release,development,fastest}-candidate10-final21.json` and
  `process-c2-candidate10-final21.json` beside that manifest. Combined with
  unchanged final21 evidence, all 23 affected native cases and six process
  families pass. Retain unchanged five-family results and proof;
  do not rerun them because the implementation or documentation commit changed.

  Historical candidates/profiles/failures remain in worker
  `target/c2-1/*candidate*.json`, `profile-candidate*.txt`, manifests and Git
  history. Candidate8 last measured two native C2 failures (many groups
  1.084–1.096x; mixed 1.002–1.025x); its passing diagnostics are not final
  acceptance. Unmatched-thread process timings are invalid comparisons and
  remain historical only. Exact-command matched-serial tooling at `1d07c45`
  was independently verified with 16 Python tests; no engine sweep applied.

- Batch 1 is complete at integrated implementation `6d221b8`. A1 evaluation,
  F1's declared slice and C2.1 are accepted; this is not whole-engine parity.
  Changed F1 publication passed the final21 refresh. Next: execute Batch 2's
  three scoped tracks, then checkpoint acceptance and proceed to Batch 3.
  Never shrink workloads, retry unchanged failures to green, or restart
  completed checks merely for documentation/new commit hashes.

Batch 2 evidence/restart details:

- A2.1: worker `codex/batch2-a2` owns Python/Rust loop substitution and scoped
  regressions/workloads. Manifest: worker `target/a2/manifest.md`; unchanged
  pin/file selections: `target/a2/{release,development}-paths.txt`. Scalar
  comma tokens must remain whole; tuple names retain arity checks. Python-proxy
  performance needs a comparable measurement adapter; this remains explicit
  acceptance work, not an exemption for tooling. Core worker `17d81f9` is
  integrated as `0857865`: Python 12/12 and Rust runner 70/70 pass; all six
  previously lost pin/file passes are restored (release records 1211/47;
  development 1211/47/51/195). Development uses the unchanged root-private
  pinned source via a recorded entry wrapper, not the mismatched shared checkout.
  Adapter hardening is integrated through `f4d3a5a`: whole-tree worker
  attestation stays outside per-file timing, as in the actual upstream workflow;
  exact sidecar/source/binary identities remain checked before and after the
  campaign and during replay. Independent partial verification passed all seven
  stages: Python 12/20/16 tests, Rust runner70, scoped Clippy and instrumentation
  compatibility. Formatting/coverage evidence is reused on unchanged Rust inputs.
  Root manifest/evidence: `target/batch2/a2-scoped-verification.md` and
  `a2-verifier/`. Untimed worker workloads pass 5/5000/16 records. Final ROOT
  release/source replay passes as recorded below; both Rust/Python performance campaigns remain open;
  no measured parity claim. Exact proxy commands remain worker
  `target/a2/manifest.md`; integrated acceptance uses
  `target/batch2/final-acceptance-manifest.md`.
- F2.1: worker `codex/batch2-f2` owns single-file explicit-schema CSV. Manifest:
  worker `target/f2/validation-manifest.md`. Reviewed CSV source is integrated
  through `5384532`: bounded retained records, strict post-quote handling,
  pinned custom escapes, UTF-8/read-error/NULL/options and selected-cast tests,
  owned-row output. Seven parser and twelve table-function tests pass in the
  worker; root independent partial sweep passed all nine stages under
  `target/batch2/f2-scoped-verification.md` / `f2-verifier/` (parser7,
  table-functions12, owned-row contract1, generator2, scoped Clippy,
  formatting/diff, coverage and instrumentation compatibility).
  Final review subsequently found quoted newlines escaping the record-size
  accounting and custom-escape closing-quote handling; correction `42b6e04`
  passes eight CSV tests and twelve table-function tests on the integrated B1
  tree. Exact-pin probes cover embedded LF, LF/CRLF limits, lone/trailing CR
  and custom escaping. Commands/logs: `target/batch2/csv-correction-delta.md`
  and `csv-correction-{leaf,table-functions}-test.log`. Test-list artifacts
  are inventories only. Independent integrated partial verification now passes
  all 14 commands below; the earlier sweep remains evidence for its actual inputs.
  Explicit mode requires `auto_detect=false`; omitted/true inference remains F3.4. Exact
  source replay of `test_quote_default.test` passes release3/dev2 with a
  recorded DATA_DIR environment. Provenance review found the worker's development
  checkout label was wrong: the consumed SQL and both CSV hashes do match the
  exact private pin, but the shared checkout itself does not. Preserve the
  original report and reconciliation in
  `target/batch2/f2-source-provenance-correction.md`; final ROOT replay must
  check the actual private checkout and fresh worker. Ordinary-runner fixture and
  COPY prerequisites remain preserved failures, not passing whole-file claims.
  Generator correction `951821f` validates fixed100k narrow/wide all-column
  inputs, 16-record process fixtures, source/data/config hashes and before/after
  verification; two Python tests pass. Final native measurement on unchanged
  engine `51b690a` and corrected generated candidate2 FAILS: narrow Rust
  41.42/41.60ms against faster-pin10.43ms; wide266.56/266.67ms against59.25ms.
  Quiet-host21/3 paired samples and joint gate are preserved under
  `target/batch2/f2-performance/native-{release,development,fastest}-candidate2.json`.
  No CSV process timing ran after this failure, and no unchanged retry is
  authorized. Optimize the scanner/conversion hot path, retain all fixed data
  and acceptance obligations, then refresh only affected evidence. Root
  seam `a817ede` supplies selected casts and binder-owned type-name resolution
  to table adapters; 8/8 `table_functions` tests pass, including nested STRUCT
  arguments, custom cast, DECIMAL and catalog/search-path ENUM. No alternate
  constant evaluator was needed. Root manifest:
  `target/batch2/f2-bind-services-manifest.md`; refresh affected F1 lifecycle
  and range performance alongside CSV, not unrelated engine families.
  Optimization is frozen for verification, not accepted: baseline stack sample
  `target/batch2/f2-performance/profile-baseline/stacks.txt` identifies parsing,
  field/row allocations, identity-cast cloning and nullable COUNT cloning.
  CSV `7239914` adds64KiB/bulk unquoted parsing, observed-width row allocation
  and direct column construction; root `b10bedb` adds selected opt-in owned
  identity casts without bypassing custom callbacks or logical validation;
  Sol/high `c0536f1` adds borrowed nullable flat COUNT with overflow/cancellation
  checks. Manifests: `csv-optimization-manifest.md`,
  `csv-owned-conversion-manifest.md`, `count-flat-values-manifest.md` under
  `target/batch2/`. Configured Terra/low completed the16-command integrated
  manifest `csv-count-final-partial.md` plus its scoped resume on `72a53ed`:
  CSV9, table-functions13, casts16, four COUNT tests1 each, durable Python32,
  formatting, scoped Clippy, coverage missing=[] and tracing compatibility pass.
  Original sweep stopped at Clippy's nonminimal boolean warning; the equivalent
  guard normalization `72a53ed` refreshed CSV9 and remaining stages, retaining
  unaffected cast/COUNT test evidence rather than repeating it. Reports:
  `csv-count-verifier/` and `csv-count-verifier-resume/`. An earlier routine attempt lost
  final output and is explicitly unverified, not reused as a passing result.
  Release preparation `prep-csv-count-candidate3/report.json` built four binaries,
  checked provenance/inputs and passed CSV source3/2, then correctly stopped on
  the original COUNT file's `COUNT(DISTINCT *)` error-text mismatch. Narrow
  diagnostic fix `7fe9087` matches both pins; configured Terra/low passed all
  five stages in `count-star-verifier/` (grouping regression1, scoped Clippy,
  format/diff, coverage missing=[]). Unaffected checks are retained.
  Release preparation `python3 target/batch2/prepare_optimized_csv.py --after-count-diagnostic`
  passes all seven stages in `prep-csv-count-candidate4/`: four fresh binaries,
  matching provenance/inputs, COUNT source release7/dev6 and CSV fixtures16/16,
  reusing unchanged CSV source3/2 evidence. Worker source digest is
  `f10e030268e97017361efb0a903d2573f7ffe0e8bf65d184dfa9a56556c3c0ef`.
  Additional COUNT acceptance covers the fixed100k nullable VARCHAR population
  and64 explicit rejected-star records under `count-flat-values-performance/`;
  untimed three-engine replay now passes2/64 each in
  `count-durable-untimed-final1/`, with unchanged source/binary/input identities.
  Separate performance gates remain open.
  Optimized native candidate4 still FAILS against both exact pins: release
  narrow23.762ms versus10.334ms (2.30×), wide120.811ms versus58.949ms (2.05×).
  Both21/3 paired campaigns and joint gate are preserved in
  `f2-performance/native-{release,development,fastest}-candidate4.json`.
  No CSV process timing ran after failure. Diagnostic-only
  `f2-performance/profile-candidate4/stacks.txt` passed38 query oracles and
  identifies per-field allocation/free as the remaining dominant cost.
  Frozen response: one reader batch byte arena with per-field UTF-8
  validation, compact per-output-column arenas, checked packed UTF-8 vectors
  and borrowed COUNT/length consumption. Generic materialization, selected
  custom cast row order and logical validators retain their fallback contracts.
  Per-column compaction avoids retaining unrelated CSV columns in projected
  CTAS storage. Manifests: `csv-arena-manifest.md`, `packed-utf8-manifest.md`,
  `csv-packed-cast-manifest.md`; original candidate2 inputs are unchanged.
  Commits `aa716e5`, `dd67c99`, `ebd23db` integrate the three owners' changes.
  Initial partial stopped at its first compilation on an ambiguous closure
  Result error type; explicit annotation `165fa53` fixes that setup failure.
  No tests passed before that failure. Configured Terra/low's scoped sweep
  now passes, composing `packed-csv-verifier-resume1/` with `resume2/` on
  `dd167be`: cast1, vector16, CSV12, scalar2, aggregate5, table_functions15,
  casts16, three execution consumers1 each, scoped Clippy, coverage missing=[],
  and tracing compatibility with0 errors/panics/open spans. Resume1 stopped at
  Clippy conditional-expression style; equivalent if/else `dd167be` refreshed
  only scanner table_functions15 and remaining stages. Preserve both failed
  reports; no unrelated engine/Python/recovery/Kani checks ran.
  Production preparation passes all7 stages in `prep-csv-count-candidate5/`,
  including fresh four-binary build/provenance, original CSV source3/2, generated
  Rust fixtures16/16 and unchanged input checks. Original COUNT7/6 is reused.
  Candidate5 native performance still FAILS: narrow18.917/19.018ms against
  faster-pin10.445ms; wide85.194/85.124ms against59.323ms. Wide beats development
  but not release, so no joint acceptance. Preserve both21/3 campaigns and
  `f2-performance/native-fastest-candidate5.json`; no CSV process timing ran.
  Diagnostic-only profiles `profile-candidate5-{wide,narrow}/stacks.txt` pass
  55/246 query oracles and identify remaining parser/row allocation and integer
  input materialization costs. Frozen implementation `21ab348`: flattened field metadata with
  row ends, conservative complete-unquoted-record fast path, and selected-adapter
  borrowed VARCHAR integer conversion. Complex records and custom casts retain
  ordinary fallbacks; no parser/cast validation may be bypassed. Root integration
  manifest: `csv-record-borrowed-integration-manifest.md`; integrated nine-stage
  `record-borrowed-final-partial.md` passes all9 stages with the configured Terra/low
  verifier, output `record-borrowed-verifier/`: borrowed cast3/3, CSV17/17,
  table_functions16/16, casts16/16, scoped Clippy, coverage missing=[] and tracing
  with0 errors/panics/open spans. Relevant inputs match frozen `21ab348` despite
  documentation checkpoint `15ecc16`. Packed vector/COUNT/length inputs are
  unchanged, so retain that verified evidence.
  `prepare_optimized_csv.py --borrowed-csv` passes all8 production/source stages
  in `prep-csv-count-candidate6/`: fresh four binaries/provenance, original CSV3/2,
  fixed Rust CSV fixtures16/16, unchanged input checks and14 original integer
  records per pin against both Rust and C++. Engine input digest before/after is
  `b839721b707fada8c0d8b76efe1736393936605fcd2c0bc380d7bbfef2fd86e7`.
  Selected integer source IDs/hashes are in `borrowed-varchar-cast-manifest.md`;
  broader decimal/exponent grammar remains a G05 gap, not delivered by this
  allocation optimization. Once the host is quiet, next exact command is
  `python3 target/batch2/run_csv_native_candidate4.py --authorized --borrowed-csv`.
  It runs both fixed-pin native gates; candidate6 reports do not yet exist.
  Do not rerun preparation or the partial sweep merely for a new documentation HEAD.
  Following the CSV native gate, reviewed command arrays for the remaining
  CSV process/F1/A2/B1/COUNT gates are now in `remaining-performance-candidate6.json`;
  serial per-stage executor is `run_remaining_stage.py STAGE --authorized
  --manifest target/batch2/remaining-performance-candidate6.json
  --log-root target/batch2/remaining-candidate6-stage-logs`.
  Its untimed `durable_preparation_candidate6` now passes with current provenance
  `b1-durable-rust-provenance-candidate6.json` and report in
  `remaining-candidate6-stage-logs/durable_preparation_candidate6/`. Do not repeat it
  unless relevant executable inputs change. No candidate6 timed stage has run.
  All workloads/21samples/3warmups remain fixed. Quiet-host preflight after the
  release build still showed BambuStudio processes using about38% of one CPU core
  combined, plus other active desktop/indexing processes. User was asked to pause
  activity; no authorization to stop their apps was inferred. Resume timings when
  quiet, preserve any failures, and proceed to Batch3 only after Batch2 acceptance.
  Then refresh the fixed quiet-host workloads; previous view/native evidence is
  unchanged, not an excuse to rerun it.
- B1: worker `codex/batch2-b1` owns persistent views/catalog/binding/native
  lifecycle. Manifest: worker `target/b1/validation-manifest.md`. Shared
  `binder/table.rs` ownership is split by method: root's table-function context
  versus B1 relation resolution. Stored SQL must preserve pinned qualification,
  dependencies, old snapshots, replacement and native reopen semantics; an
  in-memory view is not completion. Worker feedback passes views10, catalog13,
  stored-expression15, defaults6, logging1, publication8 and process331 records.
  Exact selected upstream cases pass21 records per pin. Bounded native
  projection/filter/alias/stacked checkpoint and WAL exchanges pass in both
  directions on both pins; these are worker feedback, not final integrated
  acceptance. Engine source is integrated as `fc905a8`, including corrected
  schema-qualified recursion, duplicate output names, required DROP errors and
  wide numeric literals. Native delta `51b690a` adds focused torn/corrupt
  view-WAL atomicity and rejects legacy/new cross-catalog sources instead of
  silently rebinding locally; both focused worker tests pass. Both exact pin
  cross-catalog files are rejected, and standardized private-development
  selected feedback now passes21 records. The durable
  performance adapter is assigned independently to Terra/medium in
  `../duckdb-rust-batch2-b1-durable` / `codex/batch2-b1-durable`, owning only
  the new durable measurement script/tests/manifest. Fixed 10k-row view and
  direct-table controls cover checkpoint and WAL create/reopen/query/drop;
  no timing before review and quiet-host authorization.

- Resumed checkpoint: the configured Terra/low verifier passed all 14 commands
  in `target/batch2/integrated-verifier/`: views11, catalog13, stored-expression15,
  defaults6, logging1, publication8, CSV8, table-functions12 and native encoding1;
  formatting, scoped Clippy, coverage and tracing compatibility also pass.
  No substitute model, full-engine sweep or repeated Batch 1 checks ran.
  Frozen engine is `51b690a`; reviewed partial command manifest:
  `target/batch2/integrated-b1-f2-partial.md`. Completed Terra/medium preparation
  evidence is recorded below; it does not substitute for the verifier. The B1
  Sol/high corrected the durable adapter after two failed tooling review cycles;
  worker hardening `8658e6e`/`22d31eb` is integrated through `2854080`.
  Independent Python-only verification passes all six stages (durable29,
  generator2, syntax, scoped formatting/lint and diff), with evidence under
  `target/batch2/python-adapters-verifier/`. No engine checks were repeated.
  Batch 3 remains queued until Batch 2 is actually accepted.
  No timing is authorized during concurrent verification or implementation. The durable
  worktree resumed from `c0413c0`; its audited remaining defects and next
  Python-only checks are in worker `target/b1-durable/validation-manifest.md`
  (SHA-256 `590b5d26ad7266a09ded8a685b367ccc0bc36271d5ff4002497fa52970e5cdb0`).
  Seed hashes, exact absence/positive seed proof, partial-evidence retention,
  canonical release-shell attestation, conditional validation flags, late input
  drift and nonempty WAL evidence are now corrected. Historical Git HEAD is
  metadata, not a docs-only invalidation trigger. Durable real-CLI feasibility
  and final timing remain open.
  Subsequent real-CLI feasibility preserved three reports under
  `target/batch2/b1-durable-feasibility-final{1,2,3}/`: final3 passes all eight
  C++ configurations, then stops because Rust does not implement SQL
  `SET threads=1`. This is corrected at the measurement configuration boundary,
  not by adding an engine setting: C++ explicitly sets one thread; Rust uses
  its existing InlineScheduler, whose selected implementation paths are hashed.
  Adapter fixes `bf5cbad`, `eaa3d03`, `b13cd82` also accept the pins' exact
  HUGEINT decimal-string JSON and empty setup-result arrays; strict negative
  tests pass32/32, including the independent Python delta check in the scoped
  optimization sweep. Fresh full12 feasibility now passes in
  `target/batch2/count-durable-untimed-final1/b1-durable-full12/`: all three
  engines × view/direct × checkpoint/WAL, with unchanged relevant identities.
  No durable performance measurement has run.

- Fresh release preparation on engine `51b690a` passes: four binary builds;
  canonical worker provenance (source hash
  `49f8e9666c51285e5a7a4b50489adcc6fbe46142e18794f0017b06330c78916c`);
  unchanged generated CSV input verification; original CSV source release3/dev2
  with actual checkout/hash checks; A2 original release1211/47 and
  development1211/47/51/195; Rust CSV16/16, F1 range64/128, A2 5/5000/16,
  B1 process331. Evidence: `target/batch2/prep-51b690a-*`. An initial shell
  status-variable error during provenance logging is preserved separately;
  corrected wrapper/provenance execution exits0, not a performance retry.
  Subsequent CSV C++ replay correctly failed closed on zero selected tests:
  `target/batch2/csv-reference-fixtures-final1.json`. Both C++ runners discover
  fixtures under `test/`, but the generator placed them at the input root.
  Layout correction `8a91e96` preserves candidate1 and generates candidate2
  with identical data hashes/populations. All four C++ fixture cases pass
  (96/544 Catch assertions per pin) in `csv-reference-fixtures-final2.json`;
  its overall report remains incomplete because wrapper serialization failed
  before Rust ran. Only the affected Rust portion was resumed:
  `csv-reference-fixtures-rust-final3.json` passes16/16 and verifies unchanged
  inputs/data. These compose the six fixture checks, not a passing claim for
  the incomplete whole report. Original zero-test output remains preserved.
  Final B1 source replay `b1-source-integrated-final1.json` passes3/12/6 records
  per pin using the current canonical worker. CSV native performance subsequently
  failed as recorded above; other performance gates remain open.
  Final native view exchange acceptance composes six positive cases from
  `target/batch2/b1-exchange-final2/report.json` (three producers × checkpoint/WAL,
  three consumers × three assertions each, 54 assertions) and both cross-catalog
  rejections from `b1-exchange-cross-final3/report.json`. Relevant input/binary
  hashes match across reports. Common format is storage64 (`v1.0.0`);
  storage69 is only covered by the focused encoding rejection unit test.
  Preserve setup failures: final1 caught an incorrect version-to-format mapping;
  final2 passed positives but its cross-case output parser rejected empty
  CHECKPOINT JSON arrays. Only those two affected negative cases were rerun.
  Native exchange manifest/wrapper: `target/batch2/b1-exchange-manifest.md` and
  `b1_exchange.py`. Finish remaining source/performance acceptance, checkpoint
  Batch 2, then dispatch Batch 3.
  Both pins allow DROP TABLE/VIEW despite dependent views, so do not invent
  blocking dependencies; root observations are
  `target/batch2/b1-reference-observations.md`. Functional/performance acceptance
  remains open.

The first round starts from the latest accepted integrated revision, after the
lead freezes the F1 and C2.1 scope/consumer manifests. Planned modules/targets below are
deliverables, not claims that those files or commands already exist.

| Round | Slot 1 | Slot 2 | Slot 3 | Integration condition |
| --- | --- | --- | --- | --- |
| 1 | A1: current full census, T | F1: table-function lifecycle with existing range as consumer, S | C2.1: STRING_AGG, T | Separate table-source and aggregate ownership; lead integrates shared registry/build seams. A1 reads its own frozen checkout and does not gate these bounded chunks. |
| 2 | A2.1: highest-impact unblocked runner gap from A1, T/S | F2.1: explicit-schema local CSV reader, T with S review | B1: views and catalog dependency lifecycle, S | F2.1 needs F1; B1 and F2.1 binder changes integrate serially. If A2 has no unblocked high-impact gap, choose A3/A4 or an A1-ranked leaf instead. |
| 3 | E1: byte reservations and failure contracts, S | A4: incremental regression accounting, T | C1.1: named-group STRUCT extraction, T | A4 needs A1. Table-valued splitting remains a later leaf after its engine prerequisites. |
| 4 | D1: row/catalog conflict semantics, A | H1.1: filesystem contract on an existing native consumer, S | F2.2: local COPY CSV writer, S | D1 is the only transaction/publication owner; H1.1 proposes I/O seams for lead integration. F2.2 uses the existing local adapter until H1 integration. |
| 5 | E2: buffer ownership, S | F3.1: scan pushdown/residuals, S | B2.1: persistent scalar macros, S | E2 needs E1; F3.1 needs F2.1; B2.1 follows D1/B1. Multi-file/glob work waits for H1.2. |
| Remaining engine work | Highest unlocked B/C/D step | Highest unlocked E/F step | Highest unlocked engine-only H/A step | A1 results can reorder independent engine work; do not pull deferred foreign compatibility into an idle slot. |
| Absolute final phase — blocked until core-engine exit gate | G1, then dependent G2/G3 leaves | G4/G5 after their foreign-API prerequisites | ABI-dependent H4/H5 and client acceptance | No ABI/client preparation or implementation before engine completion. This is last, not a concurrent capability lane. |

Do not wait for all three workers before integrating a ready chunk. Finish and
review one coherent contract at a time, then queue its acceptance. Stop adding
implementation work when two chunks await acceptance; free a slot for the
verifier and clear the queue. A chunk remains ready, not accepted, until its own
functional, performance and scoped-regression obligations pass. Overlapping
checks on identical relevant inputs can share evidence; retain each chunk's
coverage/accounting and do not omit affected workloads.

On one host, quiet performance windows pause builds, test campaigns and other
CPU/I/O-heavy agent work across all worktrees. Separate worktrees do not isolate
host performance. Read-only planning may continue without a competing workload.
The scoped sweep also owns its integration worktree exclusively. A separate
measurement host is an optional future improvement, not assumed available.
Build all required candidates/references before timing. Never switch reference
checkouts during another campaign.

### Minimal validation contract

Every step in the track tables follows this loop. The table's acceptance column
is additive to these common gates; no step is exempt because it is tooling or an
adapter. Split any row covering several independent families into leaf IDs
(e.g. C5.1/C5.2) before dispatch. Freeze each leaf's exact case IDs, ownership and
workloads; the parent stays open until every inventoried family has a disposition.

| Checkpoint | Required action and evidence |
| --- | --- |
| Assignment | Record baseline/input identities; owned/shared paths; exact model/effort; dependency; one failing reference case; affected transitive consumers; partial/full scope and reason; exact commands/case IDs; excluded suites/rationale; complete functional commands; performance workloads/configurations/adapters. Missing coverage becomes a named task. |
| First executable slice | Run one end-to-end success, one negative/boundary case and the nearest existing consumer. Confirm the selected tests actually run. Resolve shared-interface problems here. |
| Each logical behavior batch | Run affected Cargo target/filter or Python tests, then the small unchanged upstream selection on both pins. Reuse caches; record source hash, selection/counts, result and newly exposed blocker. Do not stack edits on an unexplained regression. |
| Shared-contract edit / integration | Broaden to all listed consumers and compare debug/ordinary release outcomes. Include prepared reuse, custom adapters, vector encodings, rollback/reopen or foreign destruction as relevant. Re-run on the integrated tree; worker results are not integration results. |
| Early performance diagnosis | After correct representative execution exists, time the predeclared hot path on a quiet host. Diagnose a slowdown before implementing the whole family. This is not acceptance or permission to reduce semantics/workloads. |
| Ready | Independent reviewer checks pinned semantics, missing cases, ownership, performance comparability and manifest completeness. Freeze source and manifest before final gates. |
| Complete implementation | Complete assigned unchanged upstream/API/native/configuration population; every affected gate-P workload; delegated impact-scoped sweep and applicable Kani report (or reasoned N/A). Same tested relevant implementation and validation inputs, no stale binaries or unexplained ordinary failures. |
| Complete documentation/instructions | Relevant diff, links, examples, syntax and consistency checks only. Never dispatch engine validation; retain existing engine evidence with its actual tested revision. |
| Handoff | Update the existing status row in place with four gate fields, exact revisions/reports, remaining IDs and next dependency. Raw artifacts stay under `target/`. State whether full census is fresh or historical. |

During edits, a targeted `cargo test -p duckdb-rust --test <target> <filter>`
already compiles its dependencies; do not precede it with a redundant check.
Use `cargo check -p duckdb-rust --lib` for a library-only structural batch that
does not yet change behavior. Python-only changes use their affected unit tests.
One validation process per worktree; a watcher must be stopped before another
runner/test starts. Its lock coordinates upstream runs, not arbitrary Cargo
commands. No full Clippy, recovery sweep, tracing or Kani in the routine loop.

The delivered fast path is available for exact self-contained SQLLogic files:

```sh
python3 scripts/run_upstream.py --target both --debug-worker --path-list target/<chunk>-paths.txt --report target/<chunk>-debug-<state>.json
```

The `--target both` example requires every path to exist on both pins. Otherwise
use separate per-pin path lists, target flags and reports; preserve each pin's
unchanged bytes. Use fresh report names. The optional `--watch --debounce-seconds 0.25` mode
repeats on settled source changes and invalidates in-flight stale results.
Selected-cache mode does not stage external fixtures/includes: use the ordinary
runner for those files. Before final acceptance, compare debug and fresh release
outcomes with `--debug-worker --compare-release`, then run the ordinary
`--target both --path-list ...` campaign without debug/prebuilt flags.
Debug results and selected file prefixes never count as a full population.
Final functional acceptance additionally covers every native/API/configuration/
platform case declared in the manifest, not only this small SQL selection.

A prebuilt worker additionally requires the exact current source/binary/profile
sidecar from `--write-worker-provenance`; a prior binary is not assumed valid.
Fast-cache and source-tamper negatives must continue to fail closed. New Rust
interfaces/files require `cargo dev coverage` and
an affected-package/target `cargo dev trace check` at integration. Workspace/
all-target tracing checks require the same broad-impact justification as other
full checks.

The configured verifier follows the durable
[scope policy](../AGENTS.md#validation-scope--durable-policy): reject documentation-
only dispatches and unjustified full sweeps, require a reviewed scope manifest,
then run its exact affected checks progressively and fail-fast. The existing
`python3 scripts/verify_chunk.py` is full-only, not an impact selector; use it only
for justified broad checkpoints. The primary does not run or babysit the sweep.
Every applicable ordinary stage must pass; report applicable Kani findings/limits
or a scope-based N/A. Edits invalidate only evidence whose inputs or contracts
they affect. Reuse unchanged evidence with its actual tested identities; update
status without rerunning checks merely because a commit hash changed.

### At-parity or better performance

Every implementation leaf requires **at-parity or better performance** for its
operation and affected existing consumers. Declare workloads before editing.
Use correct equivalent semantics/settings, exact development and release pins,
release/no-tracing binaries and quiet-host runs. Default final measurement is
three warmups and 21 samples per declared configuration, preserving all samples,
failed runs, input/source/binary hashes and reference identities under `target/`.
Different sample counts need an explicit manifest rationale; do not stop sampling
when a favorable result appears.

For every case, require
`Rust median / min(release median, development median) <= 1.0`;
throughput must meet the larger reference throughput. Gate CPU, peak memory and
I/O independently wherever applicable. Faster cases cannot offset a failure.
Use `compare_native.py` for each pin and `fastest_reference.py` for the joint
latency gate; use the process or interface-specific adapter for other metrics.
A missing or incomparable reference/workload keeps performance open. An observed
slowdown keeps it failed. A microbenchmark or existing Rust baseline cannot
replace the declared consumer/resource scope.

The track tables name representative workloads, not already implemented
benchmark files. Creating a comparable adapter/fixture is part of the assigned
step when absent. Infrastructure changes must expose a real existing consumer
(e.g. range scans for F1), not time an empty abstraction.
Strictly documentation-only work may record **not applicable:
documentation-only** after diff review; this never closes an engine gap.

### Remaining tracks and concrete steps

All rows start **queued** unless their dependency is missing, in which case
they are **blocked on that dependency**. The Gxx sections below retain detailed
requirements and accepted-slice evidence. Existing Cargo targets named here are
fast feedback targets; new targets must be added to Cargo before they can count.
Deferred foreign-compatibility packages remain blocked regardless of whether
their narrower technical dependencies are already available.
Each implementation leaf requires the common loop, impact-scoped delegated sweep and
gate P above. A row cannot be dispatched for implementation or declared ready
until it has a concrete leaf ID, owned paths, actual named targets/filters,
pin-specific case lists and per-leaf workload/adapters. Terms such as “new CSV
target” and “scheduler target” mean add/register the target, then run it;
“native tests” and other family labels must resolve to real commands at assignment.
Read-only evaluation does not claim new implementation acceptance.

#### Track A — evidence, harness and regression accounting

Owner area: `scripts/`, `test/runner/`, source-case mapping; no engine edits.
A1 is an evaluation campaign using existing tools. It is complete when exact
population/provenance/retry accounting and the assigned measurements are reported,
even when those results fail. Such failures keep their owning engine goals open;
they are not an A1 implementation claim. A2–A5 are separate tooling or coverage
chunks when changes are required. Only the lead edits this backlog; workers
deliver reports and status recommendations.

| Step / goals | Action and dependency | Model | Continuous and final functional evidence | Performance workload / remaining measurement |
| --- | --- | --- | --- | --- |
| A1 / G01.4 | Run both complete SQL populations at one frozen current revision; reconcile retries and old-pass losses; rank first blockers by exact IDs and affected capability. Refresh all 34 original workloads separately. No predecessor. | T | Existing summarizer/runner unit tests when touched; full commands below, every candidate accounted, fixture exclusion explicit, unchanged assertions. | Serial original native/numeric/relational/grouping/ordering manifests against both pins; report resource coverage separately. |
| A2 / G01.2 | Close remaining runner gaps one at a time: max-thread routing, load-version selection, variable-dependent loops and proxy sibling-stop semantics. Select first from A1. Engine variable functions require B4. | T; S for concurrent/session semantics | `sqllogic_runner`, affected Python tests; exact directive/concurrency files, wrong-result and skipped/unknown controls. | Matched runner loops, sessions, restart and failed-assertion consumers; warm/cold cache costs explicit. |
| A3 / G01.1/G01.3 | Map native engine/safe Rust assertions and runtime-generated/configuration instances to executed Rust contracts. Foreign symbol/client mapping is deferred with G. | L mapping; T review | Unique source assertion IDs, unmapped counts, actual Rust artifact identity and engine ownership negative controls. | New engine adapters need paired launch/mapping workloads; inventory alone does not prove engine parity. |
| A4 / G01.4/G24.4 | Add an incremental regression view keyed by case + pin + configuration; include fresh failures, lost passes, stale results and elapsed stages. Depends on A1. | T | Runner/summarizer mutation tests; zero/duplicate/omitted selections fail. Validate snapshot refresh without summing overlapping campaigns. | Large/small report inputs and selected-feedback consumers; gate tool runtime/resources. |
| A5 / G24.4/G24.5 | At each capability round, refresh whole SQL accounting and performance coverage; expand faults/fuzz/slow/platform populations as dependencies arrive. | T | Both pins, current revision, all source IDs and dispositions; prior-pass regression diff, reproducible failures. | Add cold/warm storage, durable commit/recovery, API and concurrent workloads as those capabilities land. |

A1 commands (fresh report destinations; run campaigns serially in its frozen
worktree, outside performance windows):

```sh
python3 scripts/run_upstream.py --target development --timeout 10 --jobs 4 --report target/a1-development.json
python3 scripts/run_upstream.py --target release --timeout 10 --jobs 4 --report target/a1-release.json
python3 scripts/run_upstream.py --target development --retry-timeouts-from target/a1-development.json --timeout 60 --jobs 4 --report target/a1-development-retries.json
python3 scripts/run_upstream.py --target release --retry-timeouts-from target/a1-release.json --timeout 60 --jobs 4 --report target/a1-release-retries.json
python3 scripts/summarize_upstream.py --development target/a1-development.json --development-retry target/a1-development-retries.json --release target/a1-release.json --release-retry target/a1-release-retries.json --output target/a1-summary.json
```

Run each retry only when its first report contains timeouts. The final command
applies when both retry reports exist and contain exactly their eligible IDs.
If a pin has zero timeouts, keep its first report as its final observation and
derive its totals directly from those results; do not fabricate a retry report.
The current summarizer requires both retry inputs. Record this adapter limitation
and use explicitly reviewed per-pin accounting for that campaign; optional-retry
automation belongs to A4. Deadlines and unexecuted tails remain visible.

#### Track B — SQL, catalog objects and mutation semantics

Owner area: parser/binder, catalog and DML; shared identities/plans stay with lead.
Views/macros require explicit native representation or an explicit open persistence
obligation; an in-memory-only implementation cannot close their assigned lifecycle.

| Step / goals | Action and dependency | Model | Continuous and final functional evidence | Performance workload |
| --- | --- | --- | --- | --- |
| B1 / G10.1/G10.2 | Add persistent views: CREATE/REPLACE/DROP, aliases, dependencies, transactional binding and native reopen. Existing identity/default contracts are prerequisites already present. | S | `types`, `contracts`, `compatibility` plus new view target; pinned view/dependency cases, recursion rejection, replacement and old snapshots. | Repeated view bind/query, stacked views, DDL/rollback and reopen. |
| B2 / G10.2 | Add scalar macros, then table macros as separate leaves, with capture/qualification/default/named arguments and persistence. Needs B1 catalog lifecycle; table macros need F1. | S | New macro cases plus the `scalar_overload` filter in `contracts` and the `sql` target; expansion recursion, shadowing, lazy errors and prepared reuse. | Scalar expansion and repeated parameterized table macro queries. |
| B3 / G10.2/G09 consumers | Add sequences/nextval with transactional object lifecycle, effect demand, defaults and native exchange. | S | `contracts` with `stored_expression` filter, `alter`, `logging`; zero/negative/infinite bounds, rollback gaps, concurrent nextval contracts, bidirectional files. | Defaulted insert batches, nextval consumers, sequence checkpoint/WAL. |
| B4 / G10.3/G10.4 | Add variables and metadata/settings families; then ATTACH/DETACH/USE, temporary scope and multi-catalog routing as separate leaves. Routing needs D1 conflict model. | S | `settings`, SQL and catalog contracts; scope/RESET, forbidden cross-database writes, read-only mode, detach with live users. | Setting/metadata reads, cross-catalog lookup and attach/reopen. |
| B5 / G02 | Close binder/parser families by measured A1 IDs: star/COLUMNS and alias rules, named/default calls, SQL preparation, diagnostics and invalid bytes. | T syntax/diagnostics; S scope/preparation | `types`, `sql`, `settings`; exact names/types/spans, ambiguous scopes, rebinding and invalidation. | Parse/bind latency, repeated prepared queries and affected expression consumers. |
| B6 / G11.1 | Add RETURNING, explicit DEFAULT/BY NAME, conflict forms, UPDATE FROM/DELETE USING, then MERGE as separate leaves. Conflict forms need D1/D3 contracts. | S | `execution`, `contracts`, `logging`; multirow failures, changed-row metadata, rollback, prepared and native exchange. | Insert/update/delete batch sizes, indexed conflicts and returned-result transport. |
| B7 / G11.2/G11.3 | Add CHECK/generated columns, then foreign keys and remaining ALTER TYPE/USING/nested-field changes. Needs B1 dependency contracts and the existing G09 effect contracts; sequence-dependent cases need B3 and foreign keys need D3. | S | `alter`, `contracts` with `stored_expression` filter, `indexes`, native tests; self-reference, NULL, deferred errors, old snapshots and failure atomicity. | Constraint-heavy writes, computed backfill and schema changes with dependent indexes. |

#### Track C — scalar, temporal, nested and analytical SQL coverage

Owner area: leaf function/type modules; binder/operator changes proposed to lead.
Use A1 to order source families within each row; retain existing passing text,
numeric, nested and aggregate work.

| Step / goals | Action and dependency | Model | Continuous and final functional evidence | Performance workload |
| --- | --- | --- | --- | --- |
| C1 / G06.2 | Add C1.1 named-group STRUCT extraction, C1.2 scalar regex splitting, then C1.3 table-valued splitting after F1 and C6.1 correlated/lateral source support; retain extract-all/replacement contracts. | T; S binding review | `text_regex_value`, `text_regex`, `nested`; pinned extraction/split files, names/types, optional groups, zero-width/NULL/NUL and vector encodings. | Constant/dynamic pattern extraction/split; existing extract-all, replacement and nested consumers. |
| C2 / G08.1 | Add STRING_AGG first, then histogram/mode, statistical/regression, quantile and sketch families as separate leaves. | T simple aggregates; S distribution/state families | `grouping`; empty/all-NULL, overflow, ORDER/DISTINCT/FILTER, partition equivalence and exact result types. | Few/many groups, ordered/distinct inputs, skew, state sizes and current SUM/LIST consumers. |
| C3 / G05.2 | Add lambda binding/capture, then transform/filter/reduce, each with mixed child types and selected adapters. | S | `nested`, `types`, execution contracts; shadowing, nested captures, errors/effects, empty and NULL lists. | Flat/nested list lengths, captured values and existing list batch consumers. |
| C4 / G04/G06.2 | Finish core date/time boundaries and calendar catalog; provision pinned ICU, then named zones/DST and collations as separate leaves. | T core functions; S ICU/collation | `temporal`, `settings`, `grouping`, `indexes`; gaps/folds, infinities, prepared setting changes, sort/join/key consistency and native exchange. | Temporal casts/calendar calls, timezone conversion, collated sorting/grouping/index lookup. |
| C5 / G03/G05.1/G06.1/G06.3/G06.4 | Close the source-enumerated cast, scalar/utility, nested and text tails one family at a time; publish callable aliases/metadata via B4. | T bounded functions; S coercion/effects | Relevant `casts`, `numeric`, `nested`, text and `types` targets; every overload, NULL/error order, custom adapters and persistence where changed. | Both new function/cast and affected join/group/index/default consumers across physical encodings. |
| C6 / G07 | Add lateral/UNNEST (F1/C3 as needed), then ASOF/positional joins, richer recursive/materialized CTEs, BY NAME/GROUP BY ALL, PIVOT/UNPIVOT and sampling as separate leaves. | S | `execution`, `subqueries`, `recursive`, `grouping`; empty/NULL/cardinality, correlated scope, recurrence, schema discovery and seeded behavior. | Join distributions, recursive growth, reshaping and existing relation consumers. |
| C7 / G08.2/G08.3/G08.4 | Finish aggregate signatures/grouping masks and window RANGE/dynamic/exclusion/order semantics; connect combination/spill only after E3/E5. | S | `grouping`, `execution`; peers/ties, invalid bounds, effects, scalar/batch/partition agreement. | Many/few partitions, peer-heavy frames, ordered aggregates and eventual spill. |
| C8 / G05.4/G05.5 | Add core GEOMETRY/WKB/CRS and remaining logical type constructors, with native metadata/codecs. Needs D4 format contract. | S | New geometry tests plus `types`/`compatibility`; malformed payloads, exact metadata, both producer directions. | Construction/casts, mixed nested values, native scan/write; opaque bytes alone cannot pass. |

#### Track D — transactions, indexes and durable storage

Owner area: transaction manager, storage table/index/native modules. Only one
worker may change transaction/publication state machines at a time.

| Step / goals | Action and dependency | Model | Continuous and final functional evidence | Performance workload |
| --- | --- | --- | --- | --- |
| D1 / G14.1 | Replace blanket intervening-writer conflicts with pinned row/catalog visibility and conflict domains; preserve old snapshots and version reclamation. | A | `contracts`, `execution`, `logging`; deterministic disjoint/overlapping writer histories, DDL conflicts and wrong-history negatives. | One/many readers, disjoint/contended writes, retained snapshots; conflict outcomes validated before timing. |
| D2 / G14.2/G14.3 | Close statement error, cancellation, autocommit/preparation, repeated-open and connection/result ownership lifecycles. Depends on D1 where visibility changes; no G1/G2 prerequisite. | S; A for ambiguous state transitions | `contracts`, `settings`, relevant recovery tests and safe Rust callers; destroy owners with live results, failed bind/commit, reopen/locks. Foreign-caller validation is deferred with G. | Connection/preparation churn, failure cleanup and transaction start/commit. |
| D3 / G15 | Add SQL index DDL/dependencies, incremental maintenance, then range/gather access as separate leaves. Needs D1/B1 identity contracts. | S | `indexes`, `optimizer`, `logging`; NULL/NaN/nested uniqueness, rollback, old readers, C++ ART read/use/mutate. | Write amplification, point/range selectivity, actual read blocks and existing equality lookups. |
| D4 / G12.1/G12.2 | Inventory missing codec/type/version edges; implement one reader/encoder/metadata family at a time, including catalog objects from B. | S metadata; T isolated codec | `compression`, `compatibility`, nested/native targets; independent compressed fixtures, corrupt boundaries, rejected writes unchanged. | Cold/warm scan and compressed write for that codec/type; size, CPU, RSS and bytes read/written. |
| D5 / G12.3 | Add partial block/row-group reads, large-value/file handling, reclamation/vacuum and incremental publication. Needs E2 buffer ownership. | S | `compatibility`, `checkpointing`; selected rows vs actual I/O, holes, low budgets and old snapshots. | Selective/wide scans, checkpoint/reclamation cost, large files beyond memory. |
| D6 / G13 | Close required WAL records/versions, checkpoint/recovery concurrency and publication faults as separate leaves. Needs D1/D4; encrypted forms need H3. | S records; A publication failures | `logging`, `checkpointing`, `recovery`; FLUSH/abort, torn writes, sync/rename/process kill, bidirectional handoffs. | Durable commit, replay, repeated checkpoint and recovery; acknowledged data validated. |

#### Track E — resources, optimizer and parallel execution

Owner area: resource/scheduler contracts and physical operators. Resource and
pipeline designs must exercise an existing consumer before acceptance.

| Step / goals | Action and dependency | Model | Continuous and final functional evidence | Performance workload |
| --- | --- | --- | --- | --- |
| E1 / G17.1 | Add byte ownership/reservations and fallible query budgets through vectors and one existing operator; propagate memory settings. | S | `contracts`, `execution`, `adversarial`; reserve/release/overflow/cancel failures and retained-result ownership. | Existing scan/group/format paths with accounting enabled, peak allocations and failure cleanup. |
| E2 / G17.2 | Add buffer pin/evict/dirty lifecycle and I/O attribution with native block scan integration. Needs E1. | S | Storage/compatibility/fault targets; read/write/eviction failures, pinned blocks and transaction lifetime. | Working sets below/above cache, sequential/random blocks, CPU/RSS/I/O. |
| E3 / G17.3/G17.4 | Implement external sort first, then hash join/group/window spilling as separate leaves. Needs E1/E2 and local temp-file contract. | S | `execution`, `grouping`, `adversarial`; exact in-memory/spill equivalence, disk-full, early stop, restart cleanup. | Datasets larger than budget, spill partitions/skew, temp bytes and current in-memory consumers. |
| E4 / G16 | Add ANALYZE/statistics lifecycle; then measured transformations, join ordering and cost-based physical selection as separate leaves. Algorithms need relevant C/D/E capabilities. | T statistics/output; S transformations/cost | `optimizer`, SQL/execution; optimizer on/off differential cases, volatile/lazy-error counterexamples and invalidation. | Selectivity/join distributions, planning latency, existing 34-case baseline and actual plan metrics. |
| E5 / G18.1/G18.2 | Add task/pipeline state machine, then parallel scans/joins/aggregation/sort as separate leaves. Needs E1 and D1 ownership/visibility contracts. | A scheduler; S operators | Deterministic scheduler target plus execution; exactly-once/barriers, 1 vs many threads, cancellation and mutation atomicity. | Scaling at fixed thread counts, contention, skew and memory pressure; single-thread consumers must also pass. |
| E6 / G18.3/G18.4 | Add pending readiness/backpressure and real wait/step/cancel progress through safe Rust engine interfaces. Needs E5; G3 foreign-handle integration is deferred, not a prerequisite. | A state contract; S adapters | Blocked source, WAITING/CHUNK/FINISHED/CANCELLED, concurrent close and owner destruction. | Time to first chunk, throughput, blocked CPU and cancellation latency. |

#### Track F — table sources and external formats

Owner area: table-function/scan contracts and format modules; lead integrates
binder/plan/registry changes. Start local serial correctness before parallel
consumers, while preserving future resource ownership.

| Step / goals | Action and dependency | Model | Continuous and final functional evidence | Performance workload |
| --- | --- | --- | --- | --- |
| F1 / G19.1/G19.4 integer-range slice | Introduce registered table-function bind/schema/state/scan/cleanup lifecycle and migrate integer range/generate_series through it. Declare correlated arguments unsupported until C6.1; this does not close G19.4. | S | Add table-function target plus `execution`, `from_first`, `contracts`; named options, empty/error/cancel/early-drop, custom adapter and pin-specific range/non-foldable/prepared/error selections. | Range scan/aggregate, early LIMIT and bind/rebind; existing range-based consumers with matching C++ semantics. |
| F2 / G20.1/G20.4 | Implement explicit-schema single-file local CSV read, then COPY CSV write as separate leaves with correct quote/NULL/newline and failure cleanup. Needs F1; use existing local I/O with an explicit bounded buffer and propose its seam to H1. General memory-limit parity waits for E1. | T; S lifecycle review | New CSV target; unchanged selected CSV/COPY cases, chunk boundaries, malformed data, independent files both ways. | Narrow/wide CSV scans and writes with quoted/NULL fields; validation, CPU/RSS/I/O included. |
| F3 / G19.2/G19.3/G20.1 | F3.1 pushdown and F3.4 sniffing follow F2.1; F3.2/.3 multi-file/schema union/globs/partitions/filename additionally need H1.2; F3.5 rejects follows F2.1, while compression needs H1.2. | S | Format/scan contracts; schema drift, missing files, correct residuals and reopen, bytes actually read. | Many small/few large files, selective projections, inference and rejected-row workloads. |
| F4 / G20.2 | Add JSON value/path/transform functions; then streaming readers/writers and schema inference. Reader needs F1; functions can start independently after A1 ranking. | S | New JSON target plus nested/casts; SQL NULL vs JSON null vs missing, numeric bounds, malformed/nested inputs and reference build with JSON. | Scalar paths and large mixed-record reads/writes, retained nested consumers. |
| F5 / G20.3 | Add Parquet primitive page read/write first, then encodings/compression/nested/decimal/time/statistics and encryption as separate leaves. Needs F1/H1; encryption H3; buffer E2 for scale. | S | New Parquet target; independent files both directions, malformed pages/metadata, logical types and pushdown residuals. | Cold/warm column scans, selective row groups, write size and resource costs. |
| F6 / G20.4 | Finish COPY options, partitioned/multi-file finalization, database import/export and failed-output cleanup. Depends on relevant F2–F5/B catalog capabilities. | S | Process-level COPY/import/export plus durability; exactly-once files, failure cleanup, reopen and unsupported-option errors. | Partitioned writes/import/export, startup/finalization cost and output bytes. |

#### Track G — public ABI, interchange and clients

**Deferred to the absolute final phase; blocked on core-engine completion.**
The following rows preserve future obligations only, not dispatchable tasks.
Do not start ABI design, symbol inventories, scaffolding or measurement adapters
while engine work remains. Technical prerequisites alone do not unblock this track.

Owner area: new foreign-interface adapters and client tests. Keep unsafe foreign
pointer handling in a reviewed separate FFI crate; the existing safe engine's
`unsafe_code = "forbid"` remains in force. The lead owns workspace registration.

| Step / goals | Action and dependency | Model | Continuous and final functional evidence | Performance workload |
| --- | --- | --- | --- | --- |
| G1 / G22.1 | Export a bounded C v1 open/connect/query/materialized-result/error/free surface in a Rust-built library. Freeze exact symbols and the complete public layouts/ownership of exposed structs on both pins before code. | S; A ownership review | Add C ABI target and unchanged/mapped pinned C assertions; load actual Rust library, exact NULL/string/numeric results, early destruction and source-specified NULL/failed-handle behavior. Do not promise arbitrary forged-pointer/double-free detection. | C open/query/result access/destruction at equal types and row counts; new foreign-process resource adapter required. |
| G2 / G22.1/G22.3 | Add G2.1 configuration/prepared scalar handles after G1; G2.2 values/chunks/vectors and G2.3 appender after E1; G2.4 registration after F1 for table callbacks. | S | C prepared/appender/vector/type tests; borrowed buffers, NULL/nested values, failed flush/rollback and callback lifetimes. | Prepared loops, bulk append and chunk access; compare identical materialization/ownership semantics. |
| G3 / G22.2 | Implement C v2 environment/cache/handle/result contracts and required C++ wrapper-facing API. Pending states require E6; synchronous families can follow G1. | A | Exact state transitions, wait/step/cancel, wrong owner and destruction tests from pinned C/C++ consumers. | Handle/result lifecycle and pending progress with blocked CPU/resource checks. |
| G4 / G22.4 | Add Arrow C data/stream export, then import/scans and nested/dictionary types. Needs G2; scans F1. | S | Independent Arrow consumer, offsets/validity/schema, exactly-once release and early cancellation. | Zero-copy-eligible vs converting batches, nested transfer and peak retained memory. |
| G5 / G22.5/G24.1 | Implement ADBC database/connection/statement metadata and streams; port clients one pinned binding at a time. Needs G2/G4 and applicable G3 contract. | S | Pinned ADBC/client suites actually loading Rust; transactions, parameters, dataframe conversions, errors/threading and destruction. | Client round trips, bulk/dataframe transfer and streaming backpressure. |

#### Track H — filesystem, extensions and distribution

Owner area: I/O/security/loader/client packaging. Configured extension coverage
is finite and pinned; unsupported ABI classes remain explicit.

H1–H3 and engine-required built-in capabilities remain engine work. H4 foreign
ABI inventory/binary loading and H5 foreign-library/client packaging are deferred
with G until the absolute final phase. Implement required built-in behavior
through native engine interfaces without making C ABI scaffolding a prerequisite.
Engine CLI/platform work can proceed independently.

| Step / goals | Action and dependency | Model | Continuous and final functional evidence | Performance workload |
| --- | --- | --- | --- | --- |
| H1 / G21.1 | H1.1 generalizes local range/sequential/cancel contracts with an existing native consumer; H1.2 migrates F2.1 reads and adds glob/compression adapters with actual file-expansion/read consumers. H1.1 does not depend on F2. | S | Filesystem/fault contracts; short reads/writes, locks, metadata changes, failed publication and cleanup. | Native plus external sequential/random I/O, cache behavior and actual bytes. |
| H2 / G21.2/G21.4 | Add scoped secret/access policy, then HTTP/object-store providers one at a time. Needs H1/B4 configuration. | S policy; T bounded provider | Controlled server and credential fixtures; redaction, ranges, retries, forbidden access, stale objects and non-idempotent failures. | Local and remote provider overhead, request counts/bytes, retry costs and resources. |
| H3 / G21.3 | Add pinned database/WAL/temp encryption using audited primitives and exact format/key lifecycle. Needs H1/D4/D6; coordinate E3 temp ownership. | S; A failure review | Independent encrypted exchange, wrong keys, authenticated corruption, torn writes and recovery. | Encrypted scan/commit/checkpoint/spill, CPU/RSS/I/O per configuration. |
| H4 / G23 | Resolve extension refs/ABI classes, add loader/install/load policy, then one required built-in/external capability per leaf. Needs G2/G3/H1/H2 as applicable. | L inventory; S loader/ports | Independent compatible extension, signature/version/platform failures, repeat load, callback ownership; exact pinned extension tests. | Load/startup and actual extension queries/scans, including resource/failure paths. |
| H5 / G24.2/G24.3 | Finish shell families and package shared/static/client artifacts across the required OS/architecture matrix. Shell can begin independently; libraries need G1/G2. | T behavior/package; L mechanical manifests | CLI process/terminal tests, interrupt/output/exit semantics, clean install/exported symbols and actual target provenance. | CLI startup/rendering/import, library load and package smoke workloads on each supported platform. |

### Ordered leaf boundaries for compound work packages

The 48 rows above are work packages, not 48 promises of single-agent completion.
Use these numbered leaves when dispatching compound rows. Each leaf inherits its
row's model, consumers, negative cases and gate P, and freezes its exact cases
before code. A reviewer must reject a parent labeled accepted while a listed
leaf remains open. Further source-discovered families get additional IDs.

| Package | Ordered independently accepted leaves |
| --- | --- |
| A2 | .1 first measured unblocked runner gap; .2–.n remaining gaps, each named from A1 before assignment |
| A3/A5 | One exact native/API/configuration/platform population per leaf; keep unmapped/untested counts |
| B2 | .1 scalar macros; .2 table macros after F1; both include their durable lifecycle |
| B4 | .1 variables; .2 metadata/settings family; .3 attachment/routing; .4 temporary scope; remaining families separately |
| B5 | .1 star/COLUMNS; .2 alias/correlation scope; .3 named/default arguments; .4 SQL preparation; .5 diagnostic/invalid-byte family |
| B6 | .1 RETURNING; .2 explicit DEFAULT/BY NAME; .3 conflict/replacement; .4 UPDATE FROM; .5 DELETE USING; .6 MERGE |
| B7 | .1 CHECK; .2 generated columns; .3 foreign keys; .4 ALTER TYPE/USING; .5 nested-field ALTER; each with dependencies and reopen |
| C1 | .1 STRUCT extraction; .2 scalar split; .3 table split after F1/C6.1 |
| C2 | .1 STRING_AGG; .2 histogram; .3 mode; .4 statistical/regression family; .5 exact quantile; .6 approximate/sketch family |
| C3 | .1 lambda capture + transform consumer; .2 filter; .3 reduce; additional higher-order families separately |
| C4 | .1 core temporal boundary family; .2 calendar/current family; .3 named-zone/ICU conversion; .4 production collation/key consumers |
| C5 | One enumerated cast/scalar/nested/text/utility family per leaf, selected by A1; no “remaining catalog” blanket assignment |
| C6 | .1 correlated/lateral table sources + UNNEST; .2 ASOF; .3 positional joins; .4 recursive/materialized CTE family; .5 BY NAME/GROUP BY ALL; .6 PIVOT; .7 UNPIVOT; .8 sampling |
| C7 | .1 remaining aggregate signatures/grouping masks; .2 RANGE offsets; .3 dynamic/exclusion frames; .4 window argument ordering; .5 parallel/spill adapters after E3/E5 |
| C8 | .1 GEOMETRY value/cast/native lifecycle; remaining source-exposed type-constructor families separately |
| D3 | .1 index DDL/dependencies/native lifecycle; .2 incremental maintenance; .3 range/gather access |
| D4 | One missing codec/type/version/object family per leaf; split reader and encoder where independently useful |
| D5 | .1 selective block/row-group scan; .2 large-value/file limits; .3 reclamation/vacuum; .4 incremental publication |
| D6 | .1 missing WAL record/version family; .2 concurrent maintenance; .3 publication fault histories; later encryption after H3 |
| E3 | .1 external sort; .2 hash join spill; .3 grouping spill; .4 window/intermediate spill |
| E4 | .1 statistics/ANALYZE; .2 one safe transformation; .3 cost/algorithm selection; .4 EXPLAIN/profiling contracts; repeat .2 by source family |
| E5 | .1 scheduler + existing scan consumer; .2 parallel scan; .3 join; .4 aggregation; .5 sorting/format adapters |
| F2/F3 | F2.1 explicit-schema CSV read; F2.2 COPY CSV write (S); F3.1 pushdown/residuals; F3.2 multi-file/schema union; F3.3 partitions/virtual columns; F3.4 sniffing; F3.5 rejects/compression options |
| F4/F5 | F4.1 JSON value/path; F4.2 transform; F4.3 reader/schema inference; F4.4 writer. F5.1 primitive Parquet read/write; F5.2 encoding/compression family; F5.3 nested/decimal/time; F5.4 statistics/pushdown; F5.5 encryption |
| F6 | .1 COPY option family; .2 partition/multi-file finalization; .3 database import/export with failure cleanup |
| G2/G3/G4/G5 | G2 leaves defined above. G3.1 synchronous v2 handles; G3.2 pending after E6. G4.1 Arrow export; G4.2 import; G4.3 nested/dictionary. G5.1 ADBC; then one pinned client per leaf |
| H1/H2/H3/H4/H5 | H1 leaves defined above. H2.1 secret/access policy; then one remote provider. H3.1 encrypted native/WAL; H3.2 temporary/output. H4.1 pin/ABI inventory; H4.2 loader; then one extension capability. H5 one shell behavior or platform package per leaf |

### First-round dispatch manifests

These are the concrete starting assignments. Before any implementation, the owner
must expand the named pinned source files into exact unchanged test-case IDs and
freeze the list in its Gxx entry/handoff. Enumeration and an end-to-end failing
case are the first tasks, not optional future validation.

- **A1 (T):** own only campaign outputs with fresh revision-qualified names
  under `target/` and a status recommendation for the lead. Substitute those
  names consistently into the command templates above. Use both pinned
  inventories and `scripts/upstream_regression.py`/`summarize_upstream.py`
  after inspecting their CLI. No engine/tool edits in this evaluation step.
  Preserve timeouts/skips/unreached cases and report current counts, lost passes,
  top blockers, run times and the next exact selections. Evaluate the original
  five benchmark manifests serially; list missing resource adapters explicitly.
- **F1 (S):** own proposed `src/function/table/`, its new focused tests and
  adapters beside `src/storage/scan.rs`. Lead owns
  `src/planner/binder/table.rs`, shared plan/registry/`DatabaseBuilder`
  edits and Cargo target registration. F1 may implement provisional proposals
  to these named shared seams in its own worktree so its first slice compiles;
  the lead reviews and integrates them. First establish bind→scan→cleanup for
  existing integer range; then custom-source failure/cancellation and prepared
  reuse. Initial unchanged file is
  `test/sql/table_function/test_range_function.test` on each pin, with each
  pin's real bytes and later unrelated blockers retained. Also source-map
  `range_non_foldable.test`, `range_function_different_iterators.test`,
  prepared table-function and table-function-as-scalar errors with pin-specific
  paths; add local custom-adapter cleanup/cancellation/early-LIMIT cases.
  Fast targets:
  new table-function target, `execution`, `from_first`, `contracts`.
  Final: full assigned range/table-function cases, mixed query consumers,
  affected coverage/trace check, native and process range workloads plus scoped sweep.
- **C2.1 (T):** own STRING_AGG implementation and focused tests in
  `src/function/aggregate.rs`, its leaf modules and `test/component/grouping.rs`;
  lead integrates any required registry/binder/operator seams. Provisional edits
  to those named seams may be proposed in the worker's own worktree, with lead
  review before integration. First inspect both pinned STRING_AGG populations,
  freeze exact case IDs and establish an end-to-end failing case. Implement
  grouped/ungrouped aggregation and pinned separator, NULL, empty-input and
  result-type behavior; include ORDER BY/DISTINCT/FILTER and relevant window
  consumers. Start with focused `grouping` filters, then all affected aggregate
  consumer targets and the selected unchanged upstream cases. Preserve existing
  SUM/LIST behavior. Final performance covers few/many groups, ordered/distinct
  inputs, variable string lengths and affected existing aggregate consumers,
  including memory costs, against both pins. Add missing comparable workloads
  before readiness. Completion uses an impact-scoped sweep, not a full engine run.

After A1, choose later leaves by (1) enabling another blocked capability,
(2) measured source cases unblocked, (3) correctness risk and shared-interface
readiness, then (4) expected implementation plus validation time. No feature-count
or percentage target may override the exact acceptance obligations.

## G01 — Reliable parity inventory and harnesses

**Recorded measurement slice (September 15; full refresh queued as A1):**
both source populations and both compiled Catch
registries are inventoried; source generator/section sites, test configurations,
CI config invocations and platform declarations are separately enumerated; full SQL
first-blocker campaigns, bounded timeout retries, 18 existing compatibility-probe
invocations and all 34 original latency workloads have revision-scoped evidence above. The
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

### Delivered feedback tooling and remaining runner work

**G01.4a status:** implemented; selected debug/release comparison, ordinary
selected campaigns and the quiet compiled-feedback performance gate passed at
`ed7e62d`. The 45-test Python harness suite passed at that revision. The common
integrated sweep passed at `8f2ef3f`; performance is not claimed remeasured there.
This supersedes the former open/resume/correction rows.

The selected cache obtains exact pinned Git bytes and validates them against the
retained development manifest/per-file hashes; it does not reread the full archive
on each warm invocation. Ordinary campaigns retain full-suite verification.
Debug/prebuilt selection rejects unsafe/duplicate/missing IDs, modified cache
bytes/metadata and stale source/binary provenance. The watcher debounces and
marks changed-source runs stale. External fixture/include dependencies are outside
the narrow cache, so route those cases through the ordinary runner.

Fast tooling tests:
`PYTHONPATH=scripts python3 -m unittest scripts/test_run_upstream.py scripts/test_measure_upstream_feedback.py`.
Final functional commands use the selected debug/release pair and ordinary release
runner described in the continuous-validation contract above.
For the compiled feedback performance adapter, build the current production
`sqllogictest`, then attest with
`python3 scripts/run_upstream.py --write-feedback-provenance target/release/sqllogictest`;
the worker sidecar is a different mode and is not this adapter's input.

Recorded evidence:
`target/final-g01-debug-release-ed7e62d.json`,
`target/final-g01-ordinary-release-ed7e62d.json`, and
`target/final-g01-feedback-performance-ed7e62d.json`.
The latter records 3 warmups/21 samples, cold-cache setup, exact identities and
all raw samples. Warm wall ratios 0.746/0.744, CPU 0.75/0.75, RSS 0.266/0.266,
block I/O 1.0/1.0 and both throughputs pass against the faster pin.

Remaining runner limitations are assigned to A2, source/API mappings to A3,
current full-population measurement to A1/A5. Delivered ENUM, STRUCT-date,
Unicode case, DISTINCT ON, product, settings and cost-repair slices are recorded
in their G03/G04/G06/G07/G08/G10/G11/G16 entries; they are not a new opening wave.

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

**Current:** case conversion, character/grapheme/byte text primitives, search,
formatting, normalization and the bounded regex families below are implemented.
G06.1g/G06.1h/G06.2c is accepted at `8f2ef3f`; its final outcome is recorded below.
Earlier outcome paragraphs retain their tested revision; later entries supersede
their prefix blockers. Remaining work is C1/C4/C5, not another port of these slices.

Selected scalar/string operations and LIKE exist; `lower`/`upper` and
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
block input/output and throughput pass independently. This completes G06.1c. The then-blocking grapheme/codepoint/contains/regex
families were subsequently implemented in the following slices.

**G06.1d frozen validation manifest — grapheme and NUL-safe text primitives.**
This slice owns new implementation modules below `src/function/scalar/text/`,
the shared registration seam in `src/function/scalar/text.rs`, focused component
targets for grapheme and codepoint/predicate behavior, the G06.1d SQLLogic and
performance fixtures, their native/process workload manifests, and this entry.
It implements `substring_grapheme`, `length_grapheme`, `chr`, `ascii` and
VARCHAR `contains`; regex, collation, grapheme-aware `reverse` and the remaining
text catalog stay outside the slice. Fast checks are the new complete component
targets plus the existing `text_substring` and `text_search` targets and focused
scalar batch filters. Shared consumers include prepared execution, direct
flat/constant/dictionary/selected batches, `sum(length_grapheme(...))`,
`sum(ascii(chr(...)))`, `count(*) FILTER(contains(...))`, concatenation, BLOB
casts, LIKE, `instr` and nested display of embedded NULs.
The exact unchanged upstream paths for each pin are
`test/sql/function/string/test_ascii.test`, `test_contains.test`,
`test_contains_utf8.test`, `test_length.test`, `test_substring.test`,
`test_substring_utf8.test`, `test_complex_unicode.test` and `null_byte.test`,
selected with an exact path list. The two substring files must pass their complete
grapheme loops; the first
three `length_grapheme` records of `test_complex_unicode.test` are the selected
reachable prefix before the separately unimplemented `reverse`; `null_byte.test`
must advance through `chr`, `ascii`, storage, `contains`, `instr` and LIKE before
its separately unimplemented regex records. Prefix blockers remain explicit and
are not full-file passes.
Negative/boundary coverage includes U+0000, ASCII and multibyte scalar values,
combining marks, emoji modifiers and ZWJ sequences, variation selectors, empty
strings, empty needles, NULLs, invalid negative/surrogate/out-of-range codepoints,
large positive/negative substring bounds, invalid overloads, prepared parameters,
and scalar/batch error order with cancellation and bounded allocation. Final
functional acceptance runs both new component targets, the existing substring
and search targets, the local G06.1d SQLLogic fixture and the exact dual-pin path
list; `cargo dev coverage` and `cargo dev trace check --workspace --all-targets`
apply because this slice adds Rust interfaces/files.
Gate P declares four independent 50,000-row, single-thread,
release/no-tracing native cases: fused grapheme substring/length over
low-cardinality and high-cardinality complex Unicode values, codepoint/NUL
construction consumed by `ascii`, and variable-needle VARCHAR `contains`.
Four process fixtures repeat the corresponding stored-operand aggregate
consumers 128 times (6.4 million row visits each) with validated one-row
results. Three warmups and 21 serial samples compare both exact pins;
every native wall ratio and process wall, throughput, CPU, peak-RSS and block-I/O
ratio must independently be no worse than the faster reference. Exact results,
samples, pin/source/binary identities and failed attempts remain under `target/`.

**G06.1d final status (executable revision `1c948b0`).** The implementation now
provides utf8proc-backed grapheme length/substrings, NUL-safe `chr`/`ascii` and
VARCHAR `contains` across scalar and physical encodings. Fused grapheme
substring lengths avoid materializing the intermediate string, while predicate
selection applies the built-in `contains` kernel directly behind a crate-private
identity capability; same-named external adapters retain their own semantics.
CTAS and ordinary CREATE results also match the pinned `Count` BIGINT result
shape exposed by the NUL corpus. The focused component targets, existing
substring/search consumers, local G06.1d fixture and all four local process
fixtures pass. `cargo dev coverage` reports no missing instrumentation, and
`cargo dev trace check --workspace --all-targets` completes with no errors,
panics or open spans.

The exact dual-pin upstream report is
`target/next-batch/g06_1d/upstream-final-5.json`. Development passes the complete
ASCII 16/16, contains 17/17, UTF-8 contains 12/12, length 6/6, substring 90/90
and UTF-8 substring 21/21 files; release passes 17/17, 18/18, 13/13, 7/7,
81/81 and 18/18 respectively. The complex-Unicode file reaches 6/7 development
and 7/8 release records before the separate missing `strlen`; the NUL file
reaches 8/9 and 9/10 through `chr`, `ascii`, CTAS/storage, `contains`, `instr`
and LIKE before the separate missing `regexp_matches`. These are explicit
prefix results, not full-file passes.

The final 21-sample native reports are
`target/next-batch/performance/g06_1d/final-{release,development,fastest}-21-v3.json`.
Both pin campaigns and the joint faster-reference gate pass all four cases.
The companion process report is
`target/next-batch/performance/g06_1d/final-process-21-v3.json`; all four
workloads pass independently for wall latency, throughput, CPU, peak RSS and
block input/output. The reports retain the authoritative medians and ratios.
This completes G06.1d. Byte length, regex predicates and grapheme reverse
subsequently landed in G06.1e/G06.2a; production collations and the remaining
catalog stay open.

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

**G06.1e/G06.2a frozen validation manifest — byte/grapheme transforms and regex
predicates.** This batch owns `src/function/scalar/text/{metrics,regex}.rs`, their
registration seams in `src/function/{scalar.rs,scalar/text.rs}`, the existing
`bit_length` and `octet_length` shared contracts, focused
`test/component/{text_metrics_reverse,text_regex}.rs`, the G06 SQLLogic and
performance fixtures/manifests, their explicit Cargo targets, and this backlog
entry. The functional boundary is `strlen(VARCHAR)`, grapheme-aware
`reverse(VARCHAR)`, validation of the already implemented `bit_length` VARCHAR/BIT
and `octet_length` BLOB/BIT overloads, plus `regexp_matches` and
`regexp_full_match` with two or three arguments and constant options `c`, `i`,
`l`, `n`, `p` and `s`. Regex replacement/extraction/splitting and operators,
collations, formatting/padding and the rest of the text catalog remain outside
this slice.

Fast checks are the two complete new component targets plus the existing
`text_grapheme`, `text_codepoint`, `text_substring`, `text_search`, `bit` and
`binary_scalars` shared-consumer targets and focused scalar batch tests. The
unchanged upstream case set is
`test/sql/function/string/test_complex_unicode.test`, `test_reverse.test`,
`test_bit_length.test`, `regex_search.test`, `regexp_unicode_literal.test` and
`null_byte.test`, selected with exact paths against both pins. Prefix blockers
from separately unimplemented later functions remain visible and are not
full-file passes. Negative and boundary coverage includes empty strings,
embedded U+0000, ASCII and multibyte byte counts, combining marks, emoji
modifiers, ZWJ sequences and variation selectors; non-byte-aligned BIT padding;
NULLs; invalid arity and overloads; constant, flat, dictionary, selected,
chunked and prepared execution; partial versus full regex matching; constant and
per-row patterns; invalid syntax in either path; Unicode escapes; literal mode;
newlines; case sensitivity; whitespace in options; rejected `g`/unknown options;
and NULL or nonconstant option expressions. Shared consumers include filtering,
ordering, grouping, nesting and aggregate composition over the new results.

Final functional acceptance runs both new component targets, every named shared
target, the local G06.1e/G06.2a SQLLogic fixture and the exact dual-pin path list.
Because this slice adds Rust files and interfaces, it also runs
`cargo dev coverage` and `cargo dev trace check --workspace --all-targets`.
Gate P declares four independent 50,000-row single-thread release/no-tracing
native workloads: low-cardinality grapheme-aware reverse consumed by length,
high-cardinality byte length consumed by sum, constant-pattern regex predicate
filtering and per-row-pattern regex predicate filtering. Four process fixtures
repeat the equivalent stored-operand aggregate consumers 128 times, for 6.4
million row visits apiece and validated one-row results. Three warmups and 21
serial samples compare both exact pins. Every native wall ratio and process
wall, throughput, CPU, peak-RSS and block-I/O ratio must independently be no
worse than the faster reference; samples, reference/source/binary identities and
failed attempts remain under `target/`.

**G06.1e/G06.2a final status (executable revision `0cf752d`).** The implementation
now provides byte-counting `strlen`, utf8proc grapheme-aware `reverse`, and
NUL-safe partial/full regex predicates with constant options and constant-pattern
compilation. Packed BIGINT byte lengths, compact string-vector paths, bounded
dynamic-pattern caching, literal/prefix/suffix plans and a built-in-only direct
filter selection capability keep the new operations on the batch path without
letting external adapters claim their identity. Successfully compiled constant
patterns are total; dynamic patterns remain fallible and preserve first-error
order. The focused and named shared component targets pass, as do the local
five-record composition fixture and all four 128-loop process fixtures (521
records, zero skips). `cargo dev coverage` reports zero missing instrumentation
points, and `cargo dev trace check --workspace --all-targets` completes with no
errors, panics or open spans.

The exact dual-pin upstream report is
`target/next-batch/g06_1e_2a/upstream-final-v2.json`. Development passes complete
complex-Unicode 18/18, reverse 9/9 and bit-length 8/8 files; release passes
19/19, 10/10 and 9/9 respectively. `regex_search.test` stops before predicate
assertions at the separately unsupported `FROM VALUES` table-function syntax
(0/1 development, 1/2 release), while `regexp_unicode_literal.test` stops at
the separately unsupported `FROM table` shorthand (2/3, 3/4). The NUL corpus
passes both regex predicates and reaches the separate missing `printf` function
(16/17, 17/18). These are retained prefix blockers, not full-file passes; the
unchanged blocked regex assertions are covered directly by the focused component
and local fixtures.

Final 21-sample native reports are
`target/next-batch/performance/g06_1e_2a/final-native-{release,development,fastest}-21-v2.json`.
Both pin campaigns and the joint faster-reference gate pass all four workloads;
the maximum Rust/faster-pin ratios are 0.051 reverse, 0.928 `strlen`, 0.392
constant case-insensitive regex and 0.065 dynamic regex. The companion report
`target/next-batch/performance/g06_1e_2a/final-process-21-v2.json` passes every
workload independently for wall latency, throughput, CPU, peak RSS and block
I/O; its worst ratios are 0.638 wall, 0.500 CPU, 0.723 RSS and 1.000 block I/O.
These are recorded native/resource passes for this slice's executable revision.
A later common integrated sweep passed on `8f2ef3f`; no pending sweep is claimed
for that integrated revision. Broader regex/collation obligations remain open.

**G07.3b/G06.1f/G06.2b frozen validation manifest — FROM-first syntax,
variadic formatting and regex value functions.** This batch has three leaf
owners and one integration owner. The FROM-first leaf owns
`src/planner/binder/query.rs`, `src/parser/dialect.rs`, the narrow vendored
sqlparser parser/AST/test/PATCHES surface required because its current
`FromFirstNoSelect` path returns before DuckDB's trailing WHERE/ORDER/LIMIT
clauses, a focused `test/component/from_first.rs` target, and no scalar-function
files. It adds the implicit star projection for
`FROM relation [WHERE ...]` and `CREATE TABLE ... AS FROM VALUES ...` while
retaining explicit FROM-first projection, alias, order, limit, prepared and
nested-query behavior. The formatting leaf owns a new
`src/function/scalar/text/formatting.rs` implementation and
`test/component/text_formatting.rs`; it covers `printf` and `format`, variadic
binding/coercion, constant and row-varying formats, integer widths, floating
precision, booleans, strings and cast-to-string values, NULL demand, embedded
NULs, excess/missing arguments, invalid specifiers and invalid UTF-8 `%c`
output. The regex-value leaf owns `src/function/scalar/text/regex.rs` and
`test/component/text_regex_value.rs`; it adds `regexp_replace`, scalar-group
`regexp_extract` and `regexp_escape`, including constant/dynamic patterns,
constant options and groups, replacement backreferences, `g`/`k` legality,
empty/no-match/NULL/NUL/Unicode cases, invalid patterns/replacements/groups,
first-error order, prepared reuse and flat/constant/dictionary/selected batches.
Collection/STRUCT regex extraction, `regexp_extract_all`, regex splitting and
collations remain outside this slice. The integration owner alone owns
`Cargo.toml`, `src/function/scalar/text.rs`, any required shared scalar bind or
batch interface, the local combined SQLLogic fixture, native/process workload
manifests and fixtures, and this backlog entry.

Gate-P remediation also owns `src/main/connection.rs`,
`src/main/client_context.rs`, `src/main/settings/{mod.rs,store.rs}` and
`test/component/settings.rs` for a zero-parameter prepared-SELECT physical-plan
cache keyed by exact versioned catalog identity and built-in settings
generation. Parameterized statements, non-queries, custom settings providers,
unversioned catalogs, forced external execution and verification retain the
ordinary bind/plan path. Every cache hit opens fresh transaction, subquery,
operator and sink state. The same remediation owns
`src/execution/expression_executor/batch.rs`,
`src/function/operator/{mod.rs,arithmetic.rs}` and the focused execution
expression test for a crate-owned BIGINT `% power_of_two = 0` selection that
avoids materializing an intermediate remainder dictionary. NULL inputs, zero,
non-power-of-two and overflowing divisors, non-flat vectors, and replaced
operator adapters retain the generic path.

Fast checks are the three complete focused targets:
`cargo test -p duckdb-rust --test from_first`,
`cargo test -p duckdb-rust --test text_formatting`, and
`cargo test -p duckdb-rust --test text_regex_value`; regex edits also run the
existing `text_regex` target, formatting runs the existing `text_codepoint` and
`binary_scalars` consumers, and FROM-first runs the complete `sql` and
`sqllogic_runner` targets. Cache/filter remediation additionally runs the
`zero_parameter_prepared_queries_refresh_snapshots_and_invalidate_cache_keys`
filter in `settings` and the
`bigint_power_of_two_remainder_filter_preserves_signed_remainder_semantics`
filter in `execution`; final acceptance runs both complete targets. Their
negative/boundary coverage includes schema/settings invalidation, mutations,
explicit transaction rollback, pre-run bind failure, cached runtime failure,
signed minima, negative and zero divisors, NULLs, non-power-of-two fallback and
custom operator replacement. The local functional gate is the combined
`test/sql/g06_format_regex_g07_from_first.test` fixture. The exact unchanged
upstream IDs for both pins are
`test/sql/function/string/regex_search.test`,
`test/sql/function/string/regexp_unicode_literal.test`,
`test/sql/function/string/null_byte.test`,
`test/sql/function/string/test_printf.test`,
`test/sql/function/string/test_format.test`,
`test/sql/function/string/regex_replace.test`,
`test/sql/function/string/regex_extract.test`, and
`test/sql/function/string/regex_escape.test`, selected from a target-local path
list with `python3 scripts/run_upstream.py --target <release|development>
--path-list ...`. Each file retains its real first blocker; a prefix does not
become a full-file pass. Full functional acceptance runs every focused and
affected target, the complete local fixture, and that exact list against both
pins. New Rust files additionally require `cargo dev coverage`; shared execution
changes require `cargo dev trace check --workspace --all-targets`.

Gate P declares quiet-host, single-thread, release/no-tracing native workloads
for FROM-first filtered aggregation and CTAS-from-VALUES planning/execution,
constant and low-cardinality dynamic `printf`/`format`, constant-pattern global
regex replacement, scalar-group extraction with both constant and dynamic
patterns, and regex escaping. Matching process fixtures exercise at least 6.4
million row visits for the formatting and regex families and repeated FROM-first
parse/bind/execute cycles. Results and errors are validated before timing. Both
exact pinned C++ references run through `scripts/compare_native.py`, followed by
their joint `scripts/fastest_reference.py` gate; the process adapter gates wall
latency, throughput, CPU, peak RSS and block I/O independently against the faster
pin. Samples, reference identities, source/binary hashes and failures stay below
`target/next-batch/g06_1f_2b_g07_3b/`. Completion requires the frozen integrated
tree's delegated `python3 scripts/verify_chunk.py` sweep after all functional and
performance evidence is final.

The implementation now binds implicit-star FROM-first queries and CTAS from
VALUES, provides variadic `printf`/`format`, and provides `regexp_replace`,
scalar-group `regexp_extract` and `regexp_escape`. Constant formats compile to
statement-local plans, low-cardinality formats use a bounded batch cache, and
the common BIGINT/VARCHAR formatting and constant replacement shapes avoid
per-row constant/string cloning. Repeated zero-parameter SELECT execution now
reuses its immutable optimized physical plan without retaining transaction or
operator state, and the declared FROM-first filter selects even BIGINT rows in
one pass before its existing dictionary-aware SUM reduction. Focused cache and
filter boundary tests pass. Final recorded acceptance is under
`target/next-batch/g06_1f_2b_g07_3b/`: the
`performance/final-native-{release,development,fastest}-21-v3.json` reports
and `performance/final-process-21-v3.json` all pass their declared gates on
their recorded source/binary identities. These supersede the earlier 9-sample
diagnostics. The common integrated sweep subsequently passed at `8f2ef3f`.

The `upstream-final-{development,release}-v3.json` reports retain the exact
selected file outcomes. Development's then-missing `strip_accents` and escaped
literal blockers were closed by G06.1g/G06.1h below. Release-only dynamic-option/
group diagnostic disagreements remain explicit; development is authoritative.
Named-group STRUCT extraction and splitting remain C1. No historical prefix is
promoted into a full-file pass without the later exact report.

**G06.1g/G06.1h/G06.2c frozen validation manifest — escaped literals,
normalization and list regex extraction.** Three isolated leaf owners start
from executable revision `0abcd58`. The escaped-literal leaf owns the narrow
vendored sqlparser tokenizer/parser/dialect surface, `src/parser/dialect.rs`
only if needed, and existing literal/parser tests. It adds DuckDB `E'...'`
decoding with quote, backslash, control, numeric-byte, validated UTF-8 and
embedded-newline coverage, including rejection of raw/decoded NUL and malformed
byte sequences; ordinary quoted strings keep their current meaning. The
normalization leaf owns `src/function/scalar/text/codepoint.rs`
and `test/component/text_codepoint.rs`; it adds `strip_accents` and
`nfc_normalize` against the pinned utf8proc/Unicode tables, preserving empty,
NULL, NUL, prepared and vector-shape behavior. The regex leaf owns
`src/function/scalar/text/regex.rs` and
`test/component/text_regex_value.rs`; it adds scalar-group
`regexp_extract_all` as `LIST(VARCHAR)` with constant/dynamic patterns and
groups, constant options binding, empty and zero-width matches, unmatched capture
NULLs, Unicode/NUL, prepared reuse and physical vector views. Named-group STRUCT
extraction, regex splitting and regex table functions remain outside this slice.
The integration owner alone owns shared registration/interfaces, Cargo targets,
the combined SQLLogic fixture, workload manifests/fixtures and this backlog.

Fast checks are the affected filters in `types`, `text_codepoint` and
`text_regex_value`; final functional acceptance runs those complete targets plus
`sql`, `sqllogic_runner`, `text_formatting`, `text_regex`, `text_grapheme` and
the combined `test/sql/g06_escaped_normalize_extract_all.test` fixture. Exact
unchanged upstream IDs are
`test/sql/function/string/regex_replace.test`,
`test/sql/function/string/test_printf.test`,
`test/sql/function/string/strip_accents.test`, and
`test/sql/function/string/regex_extract_all.test` against both pins with their
real first blockers and record accounting. The `nfc_normalize` contract also
retains the exact normalization assertion in
`test/sql/collate/test_icu_collate.test`; CSV consumers remain blocked on the
separate G19/G20 reader work and are covered locally without relabeling those
files. Negative/boundary coverage includes truncated and invalid escapes,
invalid regex/options/groups, zero-width progress, unmatched groups, combining
marks, characters without decompositions, invalid UTF-8 propagation, embedded
NUL and custom physical vector views. New Rust files require `cargo dev
coverage`; parser/scalar execution changes require `cargo dev trace check
--workspace --all-targets`.

Gate P declares quiet-host, one-thread, release/no-tracing native workloads in
`benchmark/g06_1g_1h_2c_workloads.json`: repeated escaped-literal execution,
50k-row accent stripping and NFC composition, constant and low-cardinality
dynamic `regexp_extract_all`, plus the existing dynamic scalar extraction and
constant replacement consumers. Process workloads in
`benchmark/g06_1g_1h_2c_sqllogic_workloads.json` exercise repeated parse/bind
cycles and at least 6.4 million stored-row visits for normalization and list
regex extraction. Every result is validated before timing. Both exact pins run
through `scripts/compare_native.py`, then `scripts/fastest_reference.py`; the
process adapter independently gates wall time, throughput, CPU, peak RSS and
block I/O. Samples, reference identities, source/binary hashes and failed runs
stay under `target/next-batch/g06_1g_1h_2c/`. Completion requires the frozen
integrated tree's delegated `python3 scripts/verify_chunk.py` sweep after all
functional and performance evidence is final.

The integrated implementation now uses DuckDB-specific byte escape decoding
without changing PostgreSQL/generic dialect behavior; `\u` and `\U` retain
DuckDB's unknown-escape treatment rather than being decoded. Normalization uses
the length-aware pinned `utf8proc_map` with exactly `STABLE|COMPOSE` and optional
`STRIPMARK`, preserving embedded NUL, Hangul recomposition and unassigned scalar
values without the high-level wrapper's unintended `REJECTNA`. Extract-all
matches DuckDB's zero-width advancement, optional-capture NULLs, octal regex
escapes, dynamic groups and RE2 rejection/error contracts. Its bound cache is
shared across batches/prepared execution, and a guarded built-in
`list_position(regexp_extract_all(...), needle)` composition avoids materializing
intermediate lists while retaining scalar fallback and error order. The shared
VARCHAR list-position batch consumer has focused nested-suite coverage.

**Final outcome — accepted at `8f2ef3f`.** Functional acceptance passes
all 215 selected integration tests, two escaped-literal internal tests, one
physical-vector regex test and the four-record combined fixture; coverage has no
missing instrumentation and all-target tracing compatibility passes. Both pins
fully pass all four unchanged selected files in
`target/next-batch/g06_1g_1h_2c/upstream-both-final-frozen.json`, whose
engine revision is `8f2ef3f` and source is not stale. Development records are
40 replacement, 48 printf, 8 accent-stripping and 91 extract-all; release records
are 22, 45, 8 and 92 respectively.

Gate P **passes**: `native-{release,development}-final-21.json` and
`fastest-final-21.json` cover seven native workloads; `process-final-21.json`
covers five process workloads with independent wall, throughput, CPU, peak RSS
and block-I/O gates. All use three warmups/21 samples and retain both exact
reference identities, validated results and source/binary hashes in the same
directory. Native latency alone is not resource evidence; the process report
supplies that scope. Earlier diagnostic attempts are not acceptance.

The delegated `python3 scripts/verify_chunk.py` sweep passed on that tree in
2,306.4 seconds: format 1.7s, check 133.1s, Clippy 192.9s, tests 1,720.6s,
recovery 96.4s (1,632/1,632 boundaries), Kani 161.7s (6/6 harnesses).
Named-group STRUCT extraction, splitting/table functions, production collations
and the broader text/utility catalog remain open. This acceptance closes these
three leaves, not G06 as a whole.

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
- **G07.3 Complete relational syntax.** Preserve implemented DISTINCT ON and
  FROM-first behavior; add GROUP BY ALL, remaining
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

**Status: scoped retained-default functionality implemented.** The former
ADD COLUMN performance blocker was repaired: G11.3a's native and resource gates
passed at `ed7e62d`. The common integrated sweep passed at `8f2ef3f`.
These are bounded, revision-scoped results; broader retained-expression
consumers and unmeasured default workloads remain open under their owning tracks. Catalog columns and private
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

**Functional exit achieved for this representable slice; ADD-consumer performance
accepted at the revision above, broader performance coverage remains scoped:**
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
API statements retain syntax; eligible zero-parameter SELECTs also use the guarded
plan cache described below. The pure search-path
model is connected to a bounded, normalized session setting: unqualified table lookup
searches configured schemas before `main`, creation uses the current schema, and
prepared statements refresh binding when required by the cache eligibility and
catalog/settings identity checks. Catalog-qualified path entries fail
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
to its top-level schema model. Preparation does not yet own the native prepare
transaction/start timestamp. Eligible zero-parameter SELECTs now retain an
immutable physical plan with catalog/settings invalidation (G07.3b/G06.1f/G06.2b);
parameterized and other excluded paths still rebind from syntax.

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
    passed at `ed7e62d`; common integrated sweep passed at `8f2ef3f`.**
    The measurements below retain their own revision, and broader G11.3 remains open.
    Source
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
The three G16.3a cost-repair leaves below have recorded native/resource passes at
`ed7e62d`; the common integrated sweep passed at `8f2ef3f`.
Their measurements remain revision-scoped. E4 owns the remaining statistics,
transformation and physical-cost work; A1 refreshes the broad baseline.

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

**Scheduling: deferred to the absolute final phase after core-engine completion.**
All G22 children and dependent foreign clients remain blocked; the requirements
below are retained for that future phase, not current design or implementation.

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

- **G23.1 Freeze the compatibility population in the final foreign phase.** Inventory configured in-tree
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

**Scheduling:** foreign binary/ABI inventory and loader work is deferred with
G22. Required engine capabilities (for example JSON, Parquet and ICU behavior)
continue through their owning engine groups without waiting for a foreign ABI.
Their loadable-binary compatibility is a distinct final-phase obligation.

**Exit:** the selected extension population works through documented compatible
interfaces. A Rust trait cannot load an arbitrary internal-C++ binary. For those
extensions, either reimplement/port the capability or explicitly resolve a different
compatibility target; do not claim existing-binary compatibility by assumption.
Sources: upstream `src/main/extension/`, `.github/config/`, `api_spec/VERSIONING.md`,
[extension spec](../specs/components/extensions.md).

## G24 — Clients, shell, distribution and acceptance

**Current:** a small SQL CLI and local Rust tests exist. Full client/tooling,
platform and performance parity have not been demonstrated.

**Scheduling:** G24.1 clients and the foreign-library/client portions of
G24.3–G24.5 are deferred with G22 until core-engine completion. Engine/shell
platform checks and engine correctness/performance acceptance remain active;
do not let the future client population create a circular engine exit gate.

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
  the existing <=1.0 ratio rule remains binding. The historical 34-case baseline
  and later repairs require reconciliation in A1/A5; neither establishes general
  performance parity.

**Exit:** every required population is accounted for and passing under the acceptance
policy. Publish functional, file/API/extension compatibility and performance results
separately. A full Rust test sweep or six passing Kani proofs cannot replace them.
Sources: `tools/shell/main.rs`, upstream `tools/`, `.github/workflows/`,
[client testing](../specs/testing/clients.md), [acceptance](../specs/testing/parity.md).

## Handoff template

```text
Goal: Gxx — <goal title from this backlog>
Dispatch leaf: <track ID and numbered leaf, e.g. F2.1>
Baseline: <integrated Rust commit>; pinned development and release from reference-builds.md
Model / effort: <explicit row assignment; record any evidence-driven escalation>
Worktree / owner: <isolated path; lead integrates named shared-file proposals>
Owned chunks: Gxx.1 ... Gxx.n
Dependencies: <specific capabilities; integration owner for shared files>
Validation manifest:
  Owned paths / shared proposals: <exact files/modules>
  Scope: <partial by default; full requires concrete broad-checkpoint/impact reason>
  Impact: <changed contracts, transitive consumers, dependencies/features>
  Exclusions: <unaffected suites including recovery/Kani; rationale, not a pass>
  Evidence inputs: <tested source/dependency/configuration/fixture identities>
  First slice: <one end-to-end case, one negative/boundary, one existing consumer>
  Fast checks: <affected package/target and test filters; reject zero-test runs>
  Cadence: <after each logical behavior batch; one validation process per worktree>
  Upstream cases: <exact unchanged IDs, negative/boundary cases, configurations>
  Functional acceptance: <complete assigned population and interop/API commands>
  Performance: at-parity or better performance (gate P)
    Workloads: <IDs/manifest, changed operation and affected consumers>
    Metrics/configuration: <latency/throughput/CPU/peak-memory/I/O as applicable>
    Baselines: <both pinned C++ identities; faster reference per workload>
    Evidence: <final source/binary hashes, commands, samples, pass/fail/open>
Deliver: implemented behavior, unchanged upstream cases/source mappings,
         relevant cross-consumer regressions, scoped-sweep/applicable Kani result,
         at-parity or better performance evidence, remaining gaps.
Status: implementation=<queued|active|ready|accepted|blocked + dependency>
        functional=<pass|fail|open + revision/report>
        performance=<pass|fail|open + revision/report; docs-only N/A if applicable>
        sweep=<partial|full; pass|fail|open + revision/input identities>
        Kani=<successful|unsuccessful|incomplete|not applicable + scope rationale>
Feedback record: <source hash, command, exact IDs/counts, outcome, lost passes>
Elapsed effort: <implementation, feedback, integration, acceptance; review rework>
Constraints: selected subsystem interfaces; development wins semantic disagreements;
             no skips relabeled as passes; raw evidence only in ignored target/.
Readiness: agent says ready -> freeze relevant integrated inputs -> independent scoped pass.
Completion (implementation): functional acceptance + scoped sweep/applicable Kani + gate P;
            rerun only evidence affected by changed inputs, plus newly affected consumers.
Completion (documentation/instructions): relevant artifact checks only; no engine sweep.
```

## Documentation maintenance

This is the only active implementation plan. The superseded
`specs/value-expression-milestone.md` was removed in this measurement chunk; its
prior contents remain recoverable at `c2fdae7`. The opening-wave assignments have
been replaced by the remaining-work tracks.
The September 15 full-population baseline is explicitly historical; current
slice outcomes and outstanding measurement work are identified at the top.
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
