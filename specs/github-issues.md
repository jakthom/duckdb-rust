# GitHub issues: thematic engineering overview

[Specification index](README.md) · [Discussion themes](github-discussions.md) · [Pull-request themes](github-pull-requests.md)

## Scope and evidence

This document summarizes the engineering themes in the public [duckdb/duckdb issue tracker](https://github.com/duckdb/duckdb/issues), the upstream repository configured as the sibling checkout's Git remote. It is not an issue tracker for this `duckdb-rust` directory and does not assume a Rust engine exists here.

Snapshot: **2026-09-08, 17:02–17:12 UTC**. Read-only GitHub API queries enumerated 575 distinct open issues, matching the initial open-issue count, and the 100 most recently updated closed issues. The latter is an activity sample, not the 100 most recently resolved defects. Descriptions and up to ten latest comments were retrieved for 31 selected issues spanning the themes below. Selection was purposeful, not random; theme order expresses engineering synthesis, not measured frequency, severity, or maintainer priority.

States and labels are observations at collection time. `reproduced` means the repository applied that label, not that this documentation exercise reproduced the problem. An open issue can describe an older binary or a problem already addressed in newer work. A closed issue does not by itself prove a fix is released. Reporter explanations, performance measurements, and security impact claims remain attributed reports unless explicitly qualified otherwise. Reproducer attachments and commands were not executed.

The component specifications describe local commit `99063af2bd7092aff02e14184a20e24699d34d71`. Live issues cover several release and development branches; they supplement that baseline, not silently amend it.

## Executive synthesis

The selected issues concentrate attention on boundaries: committed versus transaction-local data, logical versus physical column identities, scalar versus nested-vector cardinality, parser depth versus process resources, and API progress versus scheduler readiness. Many small SQL examples exercise several components at once. A useful engineering response is therefore an explicit invariant plus an end-to-end regression, not merely a unit test of the function named in a stack trace.

Three especially important patterns are silent semantic errors, recovery behavior that only appears across process lifecycles, and regressions exposed by extensions or language bindings. The issue set also includes longstanding feature requests and an organizational announcement, so treating every open item as a current engine defect would misrepresent the tracker.

## 1. Constraint enforcement must use the correct visibility domain

Schema changes and foreign keys expose multiple notions of “the rows in this table”: committed storage, transaction-local appends, deleted rows, and rows inserted earlier in the same statement.

| Evidence | Observed state | Engineering concern |
| --- | --- | --- |
| [#25452: NOT NULL after a transactional delete](https://github.com/duckdb/duckdb/issues/25452) | Open; reproduced; PR submitted | Validation can reject a constraint because it sees a NULL row the transaction deleted. |
| [#25467: primary key over transaction-local NULLs](https://github.com/duckdb/duckdb/issues/25467) | Open; needs triage | Index creation can miss NULLs appended in the current transaction and accept invalid data. |
| [#25456: nested-field NOT NULL behavior](https://github.com/duckdb/duckdb/issues/25456) | Open; reproduced | The target of a nested constraint must not be confused with its enclosing column. |
| [#25414: ALTER TYPE introducing NULL](https://github.com/duckdb/duckdb/issues/25414) | Open; PR submitted | A type-rewrite expression must preserve existing column constraints. |
| [#7168: multirow self-referencing foreign keys](https://github.com/duckdb/duckdb/issues/7168) | Open; longstanding | Per-row or per-chunk validation can reject references that are valid after the complete statement. |
| [#57: adding foreign keys through ALTER](https://github.com/duckdb/duckdb/issues/57) | Open; longstanding feature request | Schema migration requires dependency registration, existing-data validation, and persistent catalog changes, not just syntax. |

These failures are related but not interchangeable. Scanning more committed rows can fix one validation problem while still missing local appends. Deferring checks to statement completion changes buffering and failure-rollback requirements. For nested fields, rejecting an unsupported operation can be safer than accepting it with the wrong scope; the associated review remains active in the [PR overview](github-pull-requests.md#1-constraints-cross-the-parser-catalog-storage-and-transaction-boundaries).

Engineering implications: define visibility separately for DDL validation, index construction, parent lookup, and statement-local self-reference. Test both acceptance of valid mutations and rejection of invalid ones, including rollback after partial work.

Related specs: [catalog](components/catalog.md), [transactions](components/transactions.md), [storage](components/storage.md), [indexes](components/indexes.md). Verification: [SQLLogicTest](testing/sqllogictest.md), [component/API fixtures](testing/component-api.md), [stress and recovery](testing/stress.md).

## 2. WAL recovery and index persistence are one consistency problem

The recovery reports concern the agreement of table contents, catalog metadata, index storage, and publication of a checkpoint—not merely whether a WAL file can be read.

- [#25454](https://github.com/duckdb/duckdb/issues/25454), open and reproduced, combines a generated column, a newly added primary key, and WAL replay. The linked fix concerns logical versus physical column identifiers. This is a serialization-boundary issue with a small SQL setup.
- [#23788](https://github.com/duckdb/duckdb/issues/23788), open, reports indexed queries disagreeing with table contents after shutdown checkpointing with pending WAL state. Later discussion supplies a sequential three-process reproduction; the initial report's two-engine arrangement is not a necessary condition for the later example. The associated fix remains draft.
- [#25395](https://github.com/duckdb/duckdb/issues/25395), open, reports a replay conflict against a retained production WAL. The reporter's synthetic attempt did not reproduce it. Treat this as an incident needing artifact-backed investigation, not a minimized, independently confirmed engine defect.
- [#24767](https://github.com/duckdb/duckdb/issues/24767), open, reports a Windows recovery failure involving rename and an open WAL handle. It highlights platform-dependent file-handle lifetime assumptions; the proposed cause is not verified here.

Engineering implications: model a durable database as a mutually consistent set of table blocks, index roots, catalog identities, and recovery records. A successful reopen is insufficient if index scans and sequential scans disagree. Recovery tests should distinguish orderly shutdown, crash-like termination, replay followed by shutdown, and a second reopen; these are different paths.

Verification should retain the original failing artifacts when a synthetic reproduction is incomplete. For reduced tests, prove that the checkpoint or replay path of interest was actually reached. The review of [draft PR #23848](https://github.com/duckdb/duckdb/pull/23848) is an unusually direct example: a later comment acknowledges that disabling checkpoint-on-shutdown prevented an earlier test from exercising the reported bug.

Related specs: [durability](components/durability.md), [indexes](components/indexes.md), [serialization](components/serialization.md), [file systems](components/file-systems.md). Verification: [stress](testing/stress.md), [compatibility](testing/compatibility.md), [fuzzing and storage fault injection](testing/fuzzer.md).

## 3. PEG parsing introduces a safety–flexibility–latency tradeoff

[Issue #24618](https://github.com/duckdb/duckdb/issues/24618) reports parser stack exhaustion on deeply nested input despite a binder depth limit. It remained open at collection time, but a later member comment reports that a heap-based matcher no longer reproduced the failure on a specified debug revision. [#25433](https://github.com/duckdb/duckdb/issues/25433), a parser-related ASAN stack-overflow report, was closed as completed. These states should not be flattened into a claim that the baseline still has both failures.

[Issue #25301](https://github.com/duckdb/duckdb/issues/25301) reports substantially slower preparation on a 2.0 development build than on 1.5.5, separating parse time from total preparation. A member response identifies added grammar flexibility as a tradeoff and continued performance work as necessary; it does not promise parity or announce a whole-prepare cache. The reported timing ratios are not measurements made for this specification.

The front end also includes planning failures unrelated to parser recursion: [#25430](https://github.com/duckdb/duckdb/issues/25430), open and reproduced, concerns parameter inspection while planning a correlated subquery with a STRUCT-access predicate.

Engineering implications: limits must apply before unsafe resource consumption, not only in a later compiler phase. Heap-backed parsing moves the resource problem; it does not eliminate the need for allocation bounds, cleanup on failure, and useful diagnostics. Performance tests should separate tokenization/parsing, AST transformation, binding/planning, and API preparation overhead, using both tiny frequent statements and adversarial nesting.

Related specs: [parser](components/parser.md), [planner](components/planner.md), [APIs](components/apis.md). Verification: [fuzzer](testing/fuzzer.md), [benchmarks](testing/benchmarks.md), [configuration and sanitizers](testing/configuration.md).

## 4. Semantic preservation is broader than matching ordinary scalar values

Several reports show why an optimizer or vector interface can pass conventional tests while changing SQL meaning.

| Evidence | Observed state | Invariant under pressure |
| --- | --- | --- |
| [#25266: NULL-safe semi-join after an uncommitted insert](https://github.com/duckdb/duckdb/issues/25266) | Open; reproduced | Statistics used to simplify `IS NOT DISTINCT FROM` must remain valid for transaction-local data. |
| [#24829: volatile QUALIFY below a window](https://github.com/duckdb/duckdb/issues/24829) | Closed; completed | A predicate referencing only partition keys is not necessarily constant within a partition when it is volatile. |
| [#25417: signed zero changed by ORDER BY](https://github.com/duckdb/duckdb/issues/25417) | Open; reproduced | Reconstructing sorted values must preserve an observable floating-point sign bit, not merely numeric equality. |
| [#25377: NULL filters over an extension table function](https://github.com/duckdb/duckdb/issues/25377) | Open; needs triage | Chunk and child-vector cardinality must agree at the extension-to-engine handoff. |

The cardinality explanation for #25377 comes from a community comment and is a diagnostic lead, not a verified root cause here. The QUALIFY case, by contrast, has a [merged narrow fix in #24874](https://github.com/duckdb/duckdb/pull/24874): retain volatile predicates above the window while preserving eligible stable pushdowns.

Engineering implications: metamorphic comparisons need appropriate oracles. Optimized versus unoptimized results are useful for NULL-sensitive joins and volatility, while signed zero requires an observable representation-sensitive check. Extension fixtures should intentionally vary empty, partial, nested, and selected vectors rather than assume setting a parent chunk size updates every child.

Related specs: [optimizer](components/optimizer.md), [vectors](components/vectors.md), [expressions](components/expressions.md), [sorting](components/sorting.md), [windows](components/windows.md). Verification: [SQLLogicTest oracles](testing/sqllogictest.md), [fuzzer](testing/fuzzer.md), [component/API tests](testing/component-api.md).

## 5. Malformed input and exceptional execution need dedicated invariants

[Issue #25268](https://github.com/duckdb/duckdb/issues/25268), open and reproduced, concerns an out-of-bounds condition combining CSV empty lines and duplicate NULL-string configuration. [#25280](https://github.com/duckdb/duckdb/issues/25280), also open and reproduced, concerns per-row TRY handling with nested CASE/COALESCE expressions. [#25480](https://github.com/duckdb/duckdb/issues/25480), newly open and awaiting triage, reports a VARIANT writer assertion while handling invalid nested lists through TRY_CAST.

These cases make error recovery part of normal execution correctness: after a conversion or evaluation fails, nested child counts, selection state, and partially constructed results still need to be consistent. A “returns NULL on failure” API does not relax memory-safety requirements.

[Issue #25062](https://github.com/duckdb/duckdb/issues/25062), open and under review, alleges insufficient validation of per-row FSST dictionary offsets and lengths in a crafted native database. Its significance is the trust boundary between a checksummed block and semantic bounds inside that block. A valid checksum is not a proof that all decoded offsets are safe. This document neither validates exploitability nor assigns a vulnerability identifier or severity.

Engineering implications: retain separate fuzz targets for SQL, external-format bytes, nested-value construction, and native storage corruption. Sanitizer failures, assertions, semantic mismatches, and timeouts are different failure classes and need distinct triage records.

Related specs: [file formats](components/file-formats.md), [expressions](components/expressions.md), [types](components/types.md), [compression](components/compression.md). Verification: [fuzzer](testing/fuzzer.md), [native runner](testing/native-runner.md), [CI](testing/ci.md).

## 6. Resource regressions depend on lifecycle, transport, and embedding

[Issue #25162](https://github.com/duckdb/duckdb/issues/25162), open and awaiting triage, reports retained allocator memory across repeated upsert/delete cycles, contrasting 1.4.4 with later builds. [#25416](https://github.com/duckdb/duckdb/issues/25416), open, reports progressive slowdown in a long-running Windows/.NET workload and allocator-dependent behavior. Neither should be reduced to a single-query peak-memory benchmark or treated as proof of one allocator's general superiority.

[Issue #25165](https://github.com/duckdb/duckdb/issues/25165), open, contrasts large CSV input delivered through stdin with the same workload read by filename: the reporter observes growing temporary storage only on the pipe path. Seekability, source lifetime, buffering, and downstream progress are important diagnostic dimensions; the file's format alone does not define its execution behavior.

[Issue #25282](https://github.com/duckdb/duckdb/issues/25282), closed as completed, reports intermittent result-stream latency associated with task waiting. Its [merged fix #25418](https://github.com/duckdb/duckdb/pull/25418) makes readiness checks and scheduler wait accounting relevant to API-level latency. This is distinct from the C ABI ergonomics discussed in [discussion #24991](https://github.com/duckdb/duckdb/discussions/24991).

Engineering implications: capture memory by owner/tag, process RSS, temporary bytes, and end-to-end latency separately. Repeat complete workload cycles and teardown. Compare seekable and non-seekable sources, caller-driven and worker-driven progress, and multiple platforms with exact build provenance.

Related specs: [buffers](components/buffers.md), [runtime](components/runtime.md), [scheduler](components/scheduler.md), [APIs](components/apis.md). Verification: [stress](testing/stress.md), [clients](testing/clients.md), [benchmarks](testing/benchmarks.md), [I/O metrics](testing/io-metrics.md).

## 7. Interoperability failures often originate in representation contracts

[Issue #24091](https://github.com/duckdb/duckdb/issues/24091), open, describes VARIANT losing its intended type identity when imported through Arrow and missing export support in the reported version. The associated [PR #24157](https://github.com/duckdb/duckdb/pull/24157) is open: a proposal for a canonical Arrow extension representation is not yet evidence of a released round-trip guarantee.

[Issue #25391](https://github.com/duckdb/duckdb/issues/25391), open, reports inclusive and exclusive filter boundaries becoming indistinguishable through the v2 interface. Such distinctions affect pushed-down predicates and external scan correctness even when the C function signatures remain callable.

[Issue #24983](https://github.com/duckdb/duckdb/issues/24983), open, concerns preview-extension availability on Windows. Artifact publication and platform/version compatibility can block use independently of relational correctness. Compatibility coverage must therefore include both value/schema semantics and the ability to load the intended binary combination.

Related specs: [Arrow/ADBC](components/arrow-adbc.md), [types](components/types.md), [functions](components/functions.md), [extensions](components/extensions.md). Verification: [compatibility](testing/compatibility.md), [component/API tests](testing/component-api.md), [CI](testing/ci.md).

## 8. Documentation and organizational context share the tracker

[Issue #25247](https://github.com/duckdb/duckdb/issues/25247) reports stale source paths and client-location guidance in `AGENTS.md`, with a later recheck against another upstream revision. It is an engineering-navigation issue: inaccurate source maps can misdirect work even when no database query fails. The maintenance implication for these specs is concrete—validate links and pin implementation observations to a revision.

[Issue #25035](https://github.com/duckdb/duckdb/issues/25035) links an announcement about DuckLabs joining AWS. It is organizational context, not a defect report, technical design, licensing audit, or evidence of a particular future implementation. No conclusion about project governance or future openness is inferred from its placement in the tracker.

## Cross-cutting verification priorities

The following are recommendations derived from the themes, not official upstream priorities or evidence that the existing suites lack all such tests.

| Priority surface | Useful regression shape | Harness specification |
| --- | --- | --- |
| Silent wrong results | NULL-safe predicates, volatility, signed zero, and extension-vector boundaries with explicit oracles | [SQLLogicTest](testing/sqllogictest.md), [fuzzing](testing/fuzzer.md) |
| Persistent consistency | Mutation → WAL replay → checkpoint/shutdown → second reopen; compare indexed and sequential access | [Stress/recovery](testing/stress.md), [compatibility](testing/compatibility.md) |
| Constraint visibility | Committed rows, local appends/deletes, same-statement references, rollback, generated/nested columns | [Component fixtures](testing/component-api.md), [coverage matrix](testing/coverage.md) |
| Resource safety | Deep grammar, malformed nested values, corrupted codec metadata, bounded cancellation | [Fuzzer](testing/fuzzer.md), [configuration](testing/configuration.md) |
| Operational regressions | Repeated lifecycle cycles, stdin versus file, embedded scheduling, platform/artifact combinations | [Clients](testing/clients.md), [benchmarks](testing/benchmarks.md), [CI](testing/ci.md) |

When refreshing this overview, re-read later comments and linked PR states before retaining a problem statement. Preserve the distinction between the version that failed, the commit that changed behavior, and the release or artifact a consumer actually uses.
