# Rewrite principles

[Specification index](README.md) · [Source-system overview](overview.md) · [Testing specification](testing/README.md)

Status: accepted architectural direction for the rewrite. This document states design requirements, not implemented capabilities. The source-system and test-harness specifications describe the existing DuckDB checkout and remain separate from these requirements.

## 1. Pluggable by construction

The rewrite must be a database system assembled from replaceable implementations behind explicit interfaces. Every major subsystem, including system internals, must be pluggable. Built-in implementations must use the same interfaces and satisfy the same contracts as alternative implementations.

This is the rewrite's first and governing architectural principle. It applies from the initial design, not as an extension layer added after a fixed engine has been built. DuckDB supplies reference behavior, workloads, and architectural lessons; its concrete implementations and internal dependencies are not mandatory architecture for the rewrite.

Replacing a module with another conforming adapter must not require changes to its callers. Selection belongs in explicit composition and configuration, not scattered checks for concrete implementation types. A new adapter may need registration, packaging, and configuration, but must not require a fork of unrelated engine modules.

### Scope: internal modules as well as external integrations

The following inventory establishes required areas of pluggability, not final interface signatures or an exhaustive module decomposition.

| Area | Replaceable implementations include |
| --- | --- |
| Storage and persistence | Storage engines, catalogs, transaction and concurrency models, indexes, compression, logging/WAL, checkpointing, and recovery |
| Query compilation | Parsers and grammar integrations, binding and name resolution, logical-plan construction, optimizer passes, join enumeration, cardinality estimation, cost models, and physical planning |
| Execution and resources | Physical operators and algorithms, expression evaluation, schedulers, memory allocation, buffer/cache policy, spill, and device execution |
| Data access and interchange | File systems, object-store access, file-format readers/writers, metadata discovery, vector/encoding adapters, and import/export |
| Extensions and embedding | Registration and loading, functions and types, client adapters, configuration, observability, and security policy providers |
| AI integration | Model providers, inference and embedding operators, retrieval, and optional parser, logical-plan, optimizer, or execution adapters |
| Verification and experiments | Harness adapters, generators/fuzzers, correctness oracles, fault injectors, workloads, and benchmark result collection |

Pluggable storage means replacing the persistence implementation, not merely adding another external table scan or file reader. File-format implementations must have their own interfaces instead of being inseparable from one storage engine or one SQL entry point.

### Purpose: workload transformation without a fixed OLAP architecture

The architecture must preserve an efficient OLAP implementation while allowing OLTP, graph, random-access, and AI-oriented workloads to use appropriate adapters. OLAP is an important workload to retain, not a set of internal assumptions every other workload must inherit. This is an application of guiding principle #1, not a competing priority or a promise that every adapter supports every workload.

Separate these concerns so that one choice does not silently dictate the others:

- **Data representation:** formats such as Vortex and Parquet, encodings, physical layouts, and interchange. A file reader or writer is not by itself a transaction engine or a graph implementation.
- **Access methods:** scans, key lookups, batched gathers, range seeks, adjacency access, and mutations. Random access must distinguish byte-range I/O, positional row access, and indexed key lookup; one capability does not establish the others.
- **State and consistency:** identity, catalog visibility, indexes, transaction isolation, conflict handling, durability, and recovery. Read-only, append-oriented, and mutable adapters must advertise their actual guarantees.
- **Planning and execution:** query frontends, logical operations, algorithm selection, data flow, scheduling, and resource policy. Bulk-vector execution must not be the only usable path for small requests, iterative graph operations, or latency-sensitive work.

The seams must permit point and batched access without requiring a full scan, and graph-specific operations without requiring every frontend or optimizer to know each concrete graph implementation. Logical-plan contracts must carry the relevant operator semantics, types, identities, effects, and capabilities. Planning adapters must declare which operations they understand; unsupported operations must be preserved through a valid path or rejected explicitly, not rewritten under incompatible relational assumptions.

Execution interfaces must permit suitable batching, demand-driven work, iterative state, and cancellation without imposing unnecessary materialization or one global batch size. Representation conversions and transitions between execution strategies must be explicit, observable, and costed. Shared semantic obligations remain mandatory even when physical implementations differ.

Composition must validate the capability set required by a workload. A read-only format adapter must not be treated as a transactional writer. Where mutable state is layered over file-backed data, the selected storage and transaction adapters must own visibility, indexes, publication, and recovery. Likewise, composing multiple stores does not establish cross-store atomicity or a shared snapshot: those guarantees require an explicit coordination contract, otherwise requests that require them must be rejected.

### Scope of the structural rewrite

Prefer the smallest structural changes that establish real seams, preserve useful implementations behind adapters, and allow new implementations to be added without changes to unrelated callers. Minimize disruption, not the scope of replaceability. Avoid both a fixed OLAP engine wrapped in extension hooks and an obligatory replacement of every working algorithm.

"Slight rewrite" is a scope-control goal, not an effort estimate or an assertion that new workload engines are cheap. OLTP transaction paths, graph semantics and traversal, selective access, and workload-specific scheduling still require implementation and validation. Interface design enables that work; it does not deliver its correctness or performance automatically.

Keep the default analytical path efficient through suitable composition and dispatch choices. Establish measured overhead and regression budgets before accepting structural changes. No particular dispatch mechanism, zero-overhead claim, latency target, or production-readiness claim is settled by this principle.

The user subsequently set the [acceptance requirement](testing/parity.md) to
zero performance regressions against the C++ implementation and complete DuckDB
test parity. Earlier measured 1.25 allowances are historical evidence, not active
acceptance criteria. Work on performance should address regressions, without
expanding into unrelated performance improvements.

The 2026-09-10 clarification separates correctness from performance baseline
selection: when pinned release and development behavior disagree, development
is authoritative. For each comparable workload, performance must match or beat
the faster of the two references. Preserve both outcomes and all known
divergences; the detailed rules are in the acceptance requirement linked above.

### Interfaces are full contracts

Each interface must specify the types and semantic invariants callers rely on, ownership and lifetimes, error behavior, concurrency and ordering, cancellation, resource accounting, configuration, and supported capabilities. Applicable contracts must also cover transaction visibility, durability, effects, serialization, and compatibility/version negotiation.

Built-in and alternative adapters must obey those contracts equally. Downcasts, undocumented shared state, or concrete implementation handles required by unrelated callers defeat the seam. Shared types and coordination rules must be explicit; they must not silently require the default storage, planner, or execution implementation.

Interfaces should expose substantial behavior through small, coherent contracts. This requirement does not imply a public extension point or dynamic dispatch for every helper function. Internal implementation details may stay private; independently replaceable major modules may not disappear behind an opaque, fixed engine.

### Composition and compatibility

An explicit composition root must select, construct, connect, and validate adapters. Dependencies between modules must be expressed through declared interfaces and capabilities. Built-ins are ordinary adapters, not privileged paths around this composition.

Pluggability does not mean every combination is valid. For example, storage, transactions, WAL, and recovery may require coordinated capabilities or a compatible adapter bundle. Those requirements must be declared and checked before use. Bundles must expose their constituent contracts rather than making one persistence implementation mandatory for the rest of the system.

Compile-time selection, startup configuration, and runtime loading are distinct integration mechanisms to choose per interface. Replaceability does not promise live hot-swapping of stateful modules, an immediately frozen binary interface, or automatic interoperability between incompatible semantics.

### Planner experiments, formats, and AI

Planner algorithms must be independently selectable and benchmarkable through the same declared inputs, outputs, and validity rules. Experiments must be able to vary passes, enumeration, estimation, costing, and physical selection without editing the SQL frontend or execution callers.

File-format interfaces must describe schema and metadata access, reads and writes, pushdown capabilities, ownership, and I/O behavior. Unsupported capabilities must have explicit rejection or fallback behavior. A format adapter must not depend on private state of the default planner or storage implementation.

AI functionality must be optional and attach through declared interfaces at the appropriate stage, including parsing, logical planning, optimization, or execution. Its contracts must expose relevant effects, nondeterminism, batching, cancellation, resource limits, and external access. No particular model provider or AI runtime becomes a mandatory engine dependency.

### Verification and acceptance

Each substantial Rust implementation chunk must run and report a [Kani
checkpoint](testing/kani.md) before it is declared complete. During exploration,
proof success is not a stage-completion condition. Investigate counterexamples
and record tool limitations without forcing implementation choices to suit the
verifier. Ordinary edit/test passes use the normal checks. Formal proof coverage
and acceptance requirements can be established once the design settles.

The interface is the primary conformance-test surface. Built-in and alternative adapters must run the same applicable contract tests. Adapter-specific tests supplement, rather than replace, those checks. Fuzzing, failure injection, and benchmarks must select implementations through the declared seams.

Every major module's rewrite specification must identify its interface, replaceable adapters, dependencies, compatibility constraints, and conformance suite. Before declaring a seam proven, demonstrate at least two meaningfully different adapters and replacement without caller changes. A fake can help test an interface, but does not establish production interoperability or performance.

Benchmarks must record adapter selections, versions, configuration, workloads, and correctness results so algorithm comparisons remain reproducible. A design that requires editing unrelated callers to replace an internal implementation fails guiding principle #1, even if it also provides a public extension loader.

The [rewrite workload conformance specification](testing/rewrite-workloads.md) defines additional acceptance requirements for format interchange, OLAP preservation, OLTP, graph, random access, and mixed workloads. Interface conformance, workload correctness, and workload performance are separate results; passing one does not imply the others.

### Supporting context

DuckDB's current workload guidance describes its focus on larger, less frequent queries rather than many small concurrent queries. This motivates making latency-sensitive planning and execution independently replaceable; it is not a claim that existing DuckDB lacks transactions or point-query support. [DuckDB workload tuning](https://duckdb.org/docs/current/guides/performance/how_to_tune_workloads).

Parquet describes a column-oriented file format, while the Vortex file specification includes selective column/row access among its design considerations. These are examples for testing representation and access seams, not evidence that either format supplies a complete OLTP or graph system. [Apache Parquet overview](https://parquet.apache.org/), [Vortex file specification](https://docs.vortex.dev/specs/file-format).

These external references were consulted on 2026-09-08 and are distinct from the pinned source baseline. The architectural requirements above are design decisions, not claims made by those projects.
