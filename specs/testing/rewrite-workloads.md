# Rewrite workload conformance and benchmarks

[Specification index](../README.md) · [Rewrite principles](../rewrite-principles.md) · [Existing harness inventory](README.md)

Status: future verification requirements for the pluggable rewrite. This document does not describe implemented harnesses, executed tests, or measured performance. The [existing benchmark specification](benchmarks.md) and [coverage matrix](coverage.md) remain source-baseline references.

## Three independent acceptance results

An implementation must demonstrate interface conformance, correctness for its advertised workloads, and measured performance under a declared configuration. Report these separately. A successful format round-trip does not prove a suitable transaction implementation; a correct lookup does not prove selective I/O; an interchangeable adapter does not establish competitive latency.

Harnesses must select adapters through the same composition and capability checks as ordinary callers. Configuration must identify the storage, format, access, transaction, planner, execution, and scheduling implementations in use. Unsupported combinations must be reported explicitly, not counted as passed tests. Do not require every adapter to implement every capability.

## Workload verification matrix

| Workload or change | Required correctness evidence | Required measurements |
| --- | --- | --- |
| Format interchange, initially Vortex and Parquet adapters | Equivalent logical values and schemas within the advertised type subset; NULL/nested cases; scan versus selective-read equivalence where supported; malformed input; ownership and cancellation; write/read checks for writers | Scan and selective-read latency, bytes and requests read, decode/conversion cost, memory, and supported pushdown use |
| OLAP preservation | Equivalent results for scans, joins, aggregation, sorting, windows, and spill; default and alternative planning/execution adapters run applicable shared tests | End-to-end and planning time, throughput, CPU, peak memory, spill, and structural-interface overhead against a comparable reference |
| OLTP | Point reads and writes, constraints, commit/rollback, concurrent histories checked against the declared isolation contract, conflict/deadlock behavior where applicable, and crash recovery for durable adapters | Committed transactions per second, end-to-end p50/p95/p99 latency, retries/aborts, contention, commit latency, and write amplification |
| Graph | Declared vertex/edge identity and direction; cycles, parallel edges and missing vertices; traversal/path semantics; iterative termination; update visibility where supported; results against a small independent oracle | Traversal latency, frontier/adjacency work, memory, bytes accessed, and planner/execution time across degree distributions and path lengths |
| Random access | Key lookup, positional gather, and range results against a reference; missing keys, duplicate requested positions, ordering, stale handles, and snapshot visibility where applicable | Single and batched latency, throughput, bytes fetched per useful byte, decode work, cache behavior, and local versus remote access |
| Mixed workloads | Concurrent scans, point requests, mutations, and graph work preserve each advertised consistency contract; resource isolation and cancellation; explicit rejection of unsupported cross-store guarantees | Per-workload tail latency and throughput, fairness, queueing, memory contention, and starvation under sustained load |

Graph tests must state their semantics before choosing an oracle: reachability, shortest paths, and path enumeration are different operations. Transaction tests must likewise state their isolation and durability requirements; results under different guarantees are not interchangeable.

## Demonstrating real replaceability

Exercise at least two meaningfully different adapters for each seam claimed as proven, retaining callers and applicable conformance tests unchanged. For formats, use the same logical fixture through independent adapters; for planners, hold data and execution fixed while varying the algorithm. Evaluate execution or storage changes separately before combining them so regressions can be attributed.

Include a small latency-sensitive workload alongside an analytical workload. It must be possible to configure a selective access path and suitable execution strategy without introducing concrete-adapter checks into unrelated modules. Instrument actual operations and I/O to verify that a supposedly selective request did not become an unreported full scan.

Fuzz supported operation sequences and capability combinations, including cancellation and adapter failures. Durable compositions need injected failures around commit and recovery. Negative cases must prove that incompatible capabilities and unavailable guarantees are rejected without partial registration, leaked resources, or unintended writes.

## Measurement and promotion rules

Before completing each substantial implementation chunk, run the [Kani
checkpoint](kani.md) and record its findings and limitations alongside the
applicable workload checks. During exploration, Kani proof success is not a
condition for completing a chunk. A passing proof does not establish workload
correctness or performance beyond the properties and inputs it covers.

Record implementation revisions, adapter versions and configuration, hardware, datasets and distributions, indexes/layouts, concurrency, cache state, load generation, offered and completed load, and resource limits. Include warm and cold runs where relevant. End-to-end latency must include queueing and retries; record transaction isolation and durability settings with every transactional result.

Report setup, planning, execution, conversion, and commit costs separately as well as end-to-end. Record samples and variability, errors and aborts, and correctness outcomes rather than reporting only the best throughput. Separate architectural dispatch/composition overhead from differences in algorithms or physical layouts.

The accepted [test and performance parity requirement](parity.md) sets a maximum
cost/latency ratio of 1.0 against the pinned C++ implementation, with no throughput
decrease, for every comparable workload. This supersedes previous 1.25 allowances
and Rust-to-Rust promotion baselines. Preserve equivalent semantics and comparable
configurations. Label external generators and suites with their prerequisites
and deviations; benchmark names alone do not establish conformance.

Promote a workload claim only when the required adapters exist, the applicable correctness and failure tests pass, and measurements meet its stated targets. Until then, describe it as an architectural capability under development, not production support.
