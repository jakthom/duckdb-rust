# Indexes and Adaptive Radix Trees

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Indexing

The native index subsystem has a generic index/type registration layer and an Adaptive Radix Tree implementation under [execution/index/art](../../../duckdb/src/execution/index/art/). Indexes serve eligible lookup and constraint-enforcement paths, with storage, memory ownership, scan planning, and transaction maintenance responsibilities.

An index change must preserve both query behavior and constraint behavior across append/update/delete, rollback, checkpoint, and restart. Zone-map/statistics pruning is separate from maintaining an ART index. Extension index types are an additional interface; an index's existence does not guarantee that every predicate will use it.

Sources: [index.cpp](../../../duckdb/src/storage/index.cpp), [index type interfaces](../../../duckdb/src/include/duckdb/execution/index/), [table index list](../../../duckdb/src/storage/table_index_list.cpp), [ART tests](../../../duckdb/test/sql/index/art/).

## Index interfaces and ART state

The generic index boundary separates registered index type information, bound expressions, locking, append/delete operations, constraint checks, and persistent storage information. `ART` derives from `BoundIndex` and exposes scan initialization, bounded row-ID retrieval, appending/inserting keys, constraint verification, deletion, merge, vacuum, and serialization to disk or WAL.

`TryInitializeScan` tests whether expressions can form an ART scan; `InitializeFullScan` is a separate path. `Scan` writes a set of row IDs with a maximum count. Those identifiers still need table fetch and the appropriate visibility/filter handling; they are not fully materialized SQL result rows. Index selection belongs to planning and may reject an otherwise valid index for a given predicate or estimated result size.

Source: [art.hpp](../../../duckdb/src/include/duckdb/execution/index/art/art.hpp), [bound index interface](../../../duckdb/src/include/duckdb/execution/index/bound_index.hpp).

## Mutation and constraint protocol

The ART interface makes lock ownership explicit using `IndexLock` for mutation and merge operations. Append paths can return `ErrorData` rather than immediately throwing, allowing the caller to coordinate failure across table and index changes. `VerifyAppend` and conflict-manager paths distinguish ordinary uniqueness checks from foreign-key existence requirements. Deleting a key/row-ID pair must not accidentally remove another row sharing an eligible key representation.

Updates and rollback can require index deltas rather than an unconditional in-place replacement. The tree supports checkpoint-delta merge operations and separate insert/removal merges. Both sides of a merge need the documented locks. A partially failed operation must leave the table and all affected indexes consistent with the transaction outcome.

## Persistence and allocator integrity

`SerializeToDisk` and `SerializeToWAL` produce `IndexStorageInfo`; vacuum reorganizes ART storage. The implementation contains compatibility handling for older representations, including deprecated leaf-chain cases. Memory allocation and persistent references are therefore part of index compatibility, not just in-memory lookup performance.

`Verify`, `VerifyAllocations`, and `VerifyBuffers` expose different integrity checks. A tree can produce correct results for a small scan while still leaking nodes or retaining invalid buffer references; semantic and structural checks are complementary.

## Verification requirements

Test duplicate and NULL keys under each constraint mode, composite/expression keys, conflicting append, delete/reinsert, indexed updates, rollback, parallel build/merge, range and point predicates, and checkpoint/reopen. Compare indexed execution with a sequential-scan result and independently assert constraint enforcement. ART regression cases are under [test/sql/index/art](../../../duckdb/test/sql/index/art/); broader native/storage verification is covered by [component/API](../testing/component-api.md) and [fuzzer](../testing/fuzzer.md) specs. Performance changes need selectivity-sensitive measurements rather than an assertion that index use is always faster.
