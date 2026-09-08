# DuckDB rewrite and engineering specification

This directory records the rewrite's guiding architecture and the engineering reference for the DuckDB C++ code in the sibling `duckdb/` checkout. The source reference is organized as one detailed document per engine component, with testing and test-harness specifications in a separate directory.

## Guiding principle #1: pluggable by construction

The rewrite must be a database system assembled from replaceable implementations behind explicit interfaces. Every major subsystem, including system internals, must be pluggable. Built-in implementations must satisfy the same contracts as alternative implementations; they must not depend on privileged integration paths.

This includes storage and persistence, extensions, parsing and binding, logical and physical planning, optimizer algorithms, execution, scheduling, memory management, file formats, and AI integration. Pluggability is the foundation of the rewrite, not a later extension feature. DuckDB supplies reference behavior and architectural lessons, not mandatory internal dependencies.

The purpose is workload transformation: preserve strong OLAP execution while making OLTP, graph, random-access, and AI-oriented implementations possible through the same interface-oriented architecture. File formats such as Vortex and Parquet are independently selectable data representations, not substitutes for workload-specific access methods, transaction semantics, planners, or execution strategies. No single storage layout, batch size, query language, or execution strategy may be mandatory for every workload.

The rewrite should make the smallest structural changes that establish genuine replaceability, retain useful implementations, and measure OLAP regressions. This is an architectural enabling step; production support and performance for each additional workload require implementations and verification, not just interface declarations.

The [rewrite principles](rewrite-principles.md) define the scope, interface obligations, composition rules, and verification requirements for this decision. They govern future rewrite design; the component and harness documents remain evidence about the existing implementation.

Source-reference baseline: `99063af2bd7092aff02e14184a20e24699d34d71`, inspected 2026-09-08. The source-reference documents describe the observed implementation and derive engineering/verification requirements from its interfaces. Neither those documents nor the rewrite principles assert that a Rust database implementation exists or settle Rust-specific interface mechanisms.

## Start here

- [Rewrite principles](rewrite-principles.md): the governing requirement for pluggable internals and interchangeable implementations.
- [System overview and source inventory](overview.md): scope, baseline, architecture and evidence limitations.
- [Build and generated artifacts](build.md): targets, configuration axes, generators and dependencies.
- [Cross-component contracts](contracts.md): end-to-end query, persistence and external-scan lifecycles.
- [Testing architecture and harness index](testing/README.md): the separate test-system specification and 30-family harness register.
- [Component-to-verification matrix](testing/coverage.md): checks applicable to each change surface.
- [Test execution and reproduction runbook](testing/runbook.md): source-derived commands and prerequisites.

For the SQL execution path, read parser → planner → optimizer → physical planner → execution → scheduler. For durable data, read catalog → transactions → storage → durability, then buffers, indexes, compression and serialization. Public integrations start at APIs/functions/extensions and follow their ownership boundaries into the relevant component.

## GitHub thematic overviews

These documents survey the public `duckdb/duckdb` repository as observed on 2026-09-08. They connect upstream reports, user needs, implementation proposals, and review discussions to the component and testing specifications below.

| Overview | Scope |
| --- | --- |
| [GitHub issues](github-issues.md) | Constraint visibility, recovery/index consistency, compiler safety and latency, wrong results, malformed input, resource lifecycles, interoperability, and documentation. |
| [GitHub discussions](github-discussions.md) | Stable APIs, extensible grammar, SQL feature demand, observability, external-data controls, operational/security concerns, client ergonomics, and community integrations. |
| [GitHub pull requests](github-pull-requests.md) | Implementation themes, review tradeoffs, test-harness changes, artifact compatibility, and explicit alignment of selected merges with the local baseline. |

Each overview states its collection method, sampling limits, and source links. Live GitHub states are distinct from the pinned source baseline: a request is not a roadmap commitment, an open PR is not implemented behavior, and a merge is not proof of release or inclusion in this checkout. No report, benchmark, or CI result was reproduced for this survey.

## Engine component specifications

### Runtime and compilation

| Component | Detailed scope |
| --- | --- |
| [Database runtime and session lifecycle](components/runtime.md) | Database/session ownership, statement/result lifecycle, interruption and catalog routing. |
| [SQL parser and PEG grammar](components/parser.md) | Tokenization, PEG grammar compilation/matching, parse cache, AST transformation and extension dispatch. |
| [Binder and logical planner](components/planner.md) | Binding scopes, names/types/parameters, logical operators, correlation and statement properties. |
| [Logical optimizer](components/optimizer.md) | Pass sequencing, rewrite validity, join ordering, statistics and verification controls. |
| [Physical plan generation](components/physical-planner.md) | Physical-plan allocation, algorithm selection, column resolution, dependencies and ordering. |

### Data representation and execution

| Component | Detailed scope |
| --- | --- |
| [Logical types and scalar values](components/types.md) | Logical/physical types, scalar ownership, nested metadata, common-type selection and casts. |
| [Vectors, chunks, and intermediate collections](components/vectors.md) | Vector layouts, cardinality, validity, selections, references and retained collections. |
| [Expression execution](components/expressions.md) | Bound-expression state, vector/selection evaluation, folding and callback lifetimes. |
| [Physical operators and execution engine](components/execution.md) | Source/operator/sink protocols, chunk flow, finalization, blocking and cancellation. |
| [Pipelines, events, and task scheduling](components/scheduler.md) | Pipelines, metapipelines, events, task producers, worker pools and resumable work. |
| [Join planning, hash tables, and join execution](components/joins.md) | Algorithm eligibility, build/probe state, residual predicates, join kinds and external execution. |
| [Grouped, ungrouped, and distinct aggregation](components/aggregation.md) | Ungrouped/hash/perfect/partitioned strategies, DISTINCT/FILTER, grouping sets and state combination. |
| [Sorting, Top-N, and ordered output](components/sorting.md) | Sort abstraction, runs, Top-N, dynamic boundaries and ordered parallel output. |
| [Window planning, frames, and execution](components/windows.md) | Catalog window APIs, frame/peer boundaries, blocking/streaming strategies and partition state. |

### Catalog, transactions, and persistence

| Component | Detailed scope |
| --- | --- |
| [Catalog, namespaces, and dependencies](components/catalog.md) | Qualified lookup, versioned entries, dependencies, transactional DDL and extension catalogs. |
| [Transactions and MVCC](components/transactions.md) | Transaction layers, MVCC, local writes, conflicts, commit/rollback and reclamation. |
| [WAL, checkpoint, and recovery](components/durability.md) | WAL records, checkpoint publication, recovery, concurrent checkpointing and failure ordering. |
| [Native table and block storage](components/storage.md) | Table/row-group/column/block layout, scans/fetches, mutations, constraints and format defaults. |
| [Compression and statistics pruning](components/compression.md) | Codec callback lifecycle, validity, statistics pruning, persistent ownership and decoding. |
| [Indexes and Adaptive Radix Trees](components/indexes.md) | ART/index interfaces, scans, constraint checking, locking, deltas, vacuum and persistence. |
| [Buffer management, memory, spill, and caching](components/buffers.md) | Managed allocation/pinning, memory reservations, eviction, spill and cache distinctions. |
| [Serialization and format evolution](components/serialization.md) | Schema generation, stable field identities, reconstruction context and compatibility domains. |

### Functions, integration, and operational interfaces

| Component | Detailed scope |
| --- | --- |
| [Function binding and execution interfaces](components/functions.md) | Scalar/aggregate/table/COPY/cast/window registration, binding and execution callbacks. |
| [CSV, Parquet, JSON, and COPY](components/file-formats.md) | Multi-file coordination plus detailed CSV, Parquet, JSON and COPY reader/writer boundaries. |
| [Arrow interchange and ADBC](components/arrow-adbc.md) | Schema/array/stream conversion, release ownership and the separate driver adapter. |
| [Extension registration, loading, and compatibility](components/extensions.md) | Registration/load lifecycle, ABI models, artifact configuration and external-source limits. |
| [File systems, HTTP, and external resources](components/file-systems.md) | File handles, positioned I/O, routing, opener context, HTTP and external resources. |
| [Encryption and secrets](components/security.md) | Storage encryption/key ownership, secrets/providers/storage, scope and trust boundaries. |
| [Embedding APIs, results, and language interfaces](components/apis.md) | C v1/v2, native/consumer C++, result stepping, relations, appenders and language adapters. |
| [Settings, profiling, logging, and errors](components/observability.md) | Settings, profiles/metrics, logging, error domains and cooperative timeouts. |

## Separate testing specification

The [testing index](testing/README.md) distinguishes test execution, test corpora, runtime verification, specialized measurement, and CI gates. Its documents cover:

- Native Catch runner, SQLLogicTest language/oracles, configuration/isolation, and parallel process orchestration.
- Component/API/extension fixtures; shell, Swift, stdin, temporary-directory and infrastructure self-tests.
- [Fuzzers and replay](testing/fuzzer.md), including SQL bytes, CSV/JSON bytes, Parquet bytes, generated reductions and storage operation/fault injection.
- Stress, memory-growth, sanitizer, recovery and independent I/O-metric harnesses.
- Storage/plan/API/artifact compatibility, benchmarks/regression measurements, and build/CI gates.

Separately, [rewrite workload conformance](testing/rewrite-workloads.md) specifies future acceptance checks for interchangeable formats, OLAP, OLTP, graph, random access, and mixed workloads. These are design requirements, not existing harnesses or completed benchmark results.

Test applicability is not test execution evidence. No engine build, database suite, fuzz campaign, benchmark or external CI run was performed to produce this documentation. Local Markdown/source references were checked. Counts in the overview/testing inventory describe tracked source files at the baseline, not coverage percentages or passed test cases.

## Evidence and maintenance conventions

Each component identifies its responsibilities, principal data structures/interfaces, lifecycle and ownership rules, failure/edge cases, and verification obligations. Implementation observations are backed by links to local source; requirements and illustrative sequences express engineering implications rather than an exhaustive formal model of every branch.

For claims about existing DuckDB behavior, headers, implementation, CMake registration, generated API schemas and workflow calls take precedence over stale prose. The overview records discovered differences such as the current PEG parser, separate v1/v2 generation, external SQLSmith integration, and independent I/O configuration schema. External extension/client implementations are described only through the integration code present here. For the rewrite's architectural direction, [guiding principle #1](rewrite-principles.md#1-pluggable-by-construction) takes precedence over reproducing the source implementation's internal structure.

Source links assume the following sibling layout:

```text
ddb/
├── duckdb/              # inspected C++ source
└── duckdb-rust/
    └── specs/
        ├── README.md
        ├── rewrite-principles.md
        ├── overview.md
        ├── build.md
        ├── contracts.md
        ├── github-issues.md
        ├── github-discussions.md
        ├── github-pull-requests.md
        ├── components/  # engine component specifications
        └── testing/     # independent testing/harness specifications
```

Internal spec links work within this repository; source links additionally require the sibling checkout. Commands in the testing runbook execute from `ddb/duckdb`, not from `duckdb-rust` or `specs`. When the source revision changes, refresh the baseline, ownership/signature observations, build/registration lists, corpus counts, and workflow conditions together. Keep measured outcomes in a separate run report with exact binary/configuration/artifact provenance.
