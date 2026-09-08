# Native table and block storage

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Logical-to-physical storage decomposition

```text
Native table catalog entry
  DataTable
    RowGroupCollection
      RowGroup
        ColumnData (specialized for nested/logical types)
          ColumnSegment
            Compressed/uncompressed data and persistent block references
        Row/update version information
    Table indexes and table statistics

StorageManager -> BlockManager -> database FileHandle
BufferManager -> BufferPool -> pinned/unpinned buffers -> temporary spill files
```

| Component | Responsibilities and interface |
| --- | --- |
| `DataTable` | Scan/fetch/append/update/delete, constraints, index coordination, table state |
| `RowGroupCollection` / `RowGroup` | Horizontal partitioning into groups; scans, statistics, checkpoint serialization |
| `ColumnData` specializations | Scalar, validity, list, array, struct, variant and other column layouts |
| `ColumnSegment` | A range of column values with a compression implementation and statistics |
| `RowVersionManager` / update segments | Transaction-visible row changes and deletion/update information |
| `StorageManager` | Opening/recovery, storage compatibility, checkpoint and table I/O management |
| `BlockManager` | Block IDs, read/write, reference/free-block management, header and synchronization |
| Metadata manager/readers/writers | Persistent metadata references and traversal |

Sources: [data_table.hpp](../../../duckdb/src/include/duckdb/storage/data_table.hpp), [table storage](../../../duckdb/src/storage/table/), [storage manager](../../../duckdb/src/include/duckdb/storage/storage_manager.hpp), [block manager](../../../duckdb/src/include/duckdb/storage/block_manager.hpp), [metadata](../../../duckdb/src/storage/metadata/).

## Storage constants and format evolution

The default row group contains 122,880 rows. The default block allocation size is 262,144 bytes, with an ordinary default block header of 8 bytes. File headers use 4,096-byte units. Encryption changes block/header accounting; configured block sizes and alternate format versions also matter. These numbers describe defaults, not every database or buffer allocation.

The storage format has explicit version definitions and compatibility selection. At this commit, `DEFAULT_STORAGE_VERSION_INFO` names `StorageVersion::V2_0_0`. That source constant is not a claim that the checkout is a published v2.0.0 release. Build versions derive separately from Git/version configuration, and some format/feature configurations select different compatibility behavior.

Sources: [storage_info.hpp](../../../duckdb/src/include/duckdb/storage/storage_info.hpp), [storage compatibility](../../../duckdb/src/common/storage_compatibility.cpp), [version map](../../../duckdb/src/storage/version_map.json).

## Scan and fetch contract

`DataTable::InitializeScan` establishes a transaction-aware scan over projected `StorageIndex` columns and filters. `InitializeParallelScan` and `NextParallelScan` allocate scan work to local scan states. `Scan` fills a chunk of visible rows; physical storage boundaries need not coincide with output batch boundaries. `Fetch` retrieves rows by row identifier with transaction visibility, while `FetchCommitted` is explicitly a different interface. Callers must choose the intended visibility rather than treating the latter as an interchangeable fast path.

Row-group and segment statistics can rule out ranges only when the filter proof is valid for all visible values, including updates. Nested projections require storage paths rather than a flat top-level ordinal alone. Row identifiers are internal storage identities; a scan's output position is not a stable replacement for them.

## Mutation and constraints

The append path has explicit initialization, chunk ingestion, and finalization through `InitializeLocalAppend`, `LocalAppend`, and `FinalizeLocalAppend`. It writes through transaction-local storage and participates in constraints and index maintenance. Committed integration uses append/merge operations and may incorporate optimistically written blocks. Revert operations handle failures during append/commit.

Deletes and updates use initialized mutation state, row identifiers, and typed chunks. Constraint validation includes checks for append, update, delete, and foreign-key-related behavior. A physical DML operator must preserve enough columns and row identity to perform these checks even if the query's final result projects no table columns.

Checkpointing serializes table metadata and column/row-group data through table writers while holding appropriate storage coordination. Dropped tables and columns have commit-drop cleanup paths, so logical removal and physical block release are not synonymous.

## Block and metadata ownership

`BlockManager` supplies persistent block identity and I/O; `BufferManager` supplies resident/pinned access. A persistent block reference can survive eviction, while a raw pointer into an unpinned block cannot. Metadata uses its own readers/writers and references to navigate catalog/table structures. Free-block accounting and reference tracking must prevent both premature reuse and permanent leaks across checkpoint publication.

The native layout is columnar inside row groups, but version information and indexes provide additional structures. An engineering implementation cannot reconstruct transaction semantics from column segments alone. Compression, statistics, MVCC, and checkpoint metadata jointly define a table scan's correct result.

## Verification requirements

Cover empty tables, partial final vectors/row groups, nested projections, append/update/delete across segment boundaries, local plus persistent rows, constraint rollback, indexed mutation, checkpoint/reload, and concurrent readers. Run forced compression and small-vector variants to expose boundary assumptions. Use [component tests](../testing/component-api.md), [configuration variants](../testing/configuration.md), [compatibility](../testing/compatibility.md), and [storage fuzzing](../testing/fuzzer.md). Verify persistent size/free-block behavior separately from logical query output when changing block allocation or reclamation.
