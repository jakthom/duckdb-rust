# WAL, checkpoint, and recovery

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## WAL, checkpoint, and reopening

The [write-ahead log](../../../duckdb/src/storage/write_ahead_log.cpp) records durable changes; [wal_replay.cpp](../../../duckdb/src/storage/wal_replay.cpp) reconstructs committed state during recovery. Checkpoint code serializes catalog/table metadata and table storage into durable blocks and advances the database's root/header state.

Checkpointing has full, concurrent, and in-memory/vacuum-related decisions. Other active readers, update/catalog undo requirements, writer locks, and storage configuration influence which path is valid. The storage manager can transition to a `.wal.checkpoint` while a checkpoint is in progress and reconcile concurrent writes afterward. Therefore, “checkpoint always runs with no other activity” is not an accurate model of this revision.

Sources: [checkpoint_manager.cpp](../../../duckdb/src/storage/checkpoint_manager.cpp), [checkpoint implementations](../../../duckdb/src/storage/checkpoint/), [storage_manager.cpp](../../../duckdb/src/storage/storage_manager.cpp), [single_file_block_manager.cpp](../../../duckdb/src/storage/single_file_block_manager.cpp).

## Durable objects and interfaces

`WriteAheadLog` emits typed records for catalog and data changes, table selection, deletes/updates, checkpoints, and other supported operations. WAL data is interpreted with the corresponding entry definitions and deserialization context; it is not an SQL text transcript. Replay reconstructs durable state under the storage manager during database open.

`StorageManager` owns access to the WAL and checkpoint-WAL paths and coordinates `WALStartCheckpoint`, `WALFinishCheckpoint`, automatic checkpoint eligibility, and storage commit state. `CheckpointWriter` traverses catalog objects and table data into persistent metadata, while `CheckpointReader` rebuilds schemas, tables, views, sequences, macros, indexes, types, and other supported entries. A checkpoint's root metadata identifies the newly published durable database state.

Sources: [write_ahead_log.hpp](../../../duckdb/src/include/duckdb/storage/write_ahead_log.hpp), [wal_entry.hpp](../../../duckdb/src/include/duckdb/storage/wal_entry.hpp), [storage_manager.hpp](../../../duckdb/src/include/duckdb/storage/storage_manager.hpp), [checkpoint_manager.hpp](../../../duckdb/src/include/duckdb/storage/checkpoint_manager.hpp).

## Ordering and recovery obligations

The essential durability boundary is successful completion of the storage commit protocol, not merely appending bytes to an operating-system buffer. Data blocks, WAL flushes, and metadata/header publication must follow the implementation's ordering rules. A newly published root must not depend on blocks that can be lost after the operation reports success. Conversely, blocks written speculatively before publication must not become visible as a committed transaction solely because they exist in the file.

Concurrent checkpointing complicates this ordering: old checkpoint state, writes participating in the new checkpoint, and concurrent WAL activity can coexist. Rotation through `.wal.checkpoint` is part of the recovery protocol. These files must not be manually removed as disposable build artifacts while a database is active or awaiting recovery.

Replay must distinguish complete committed work from incomplete tails and must report malformed/incompatible data through the storage error path. Recovery and idempotence should be checked by reopening repeatedly, including after a recovery operation itself is interrupted. Exact accepted corruption/truncation cases are implementation-specific and require targeted tests rather than a blanket claim that every damaged file is recoverable.

## Fault model and verification

Three complementary test classes are required: ordinary close/reopen and checkpoint tests; process-interruption/WAL replay tests; and injected file-write or synchronization failures. The native storage fuzzer performs operation sequences with one-shot filesystem faults and verifies the next reopen against the last expected state. It is not a general malformed-database-byte generator.

Assertions should separate acknowledged commits, rejected commits, and indeterminate external failures. Check table contents, catalog objects, indexes, and future database usability, not just whether opening succeeds. Historical storage files and cross-version readers add a separate format-compatibility obligation described in [compatibility testing](../testing/compatibility.md). Relevant code and execution limitations for fault campaigns are in [fuzzing](../testing/fuzzer.md) and [stress](../testing/stress.md).
