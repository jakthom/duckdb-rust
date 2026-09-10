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

### Retained checkpoint publication metadata

The native main-header version and database-header version are different
namespaces in older files. Main versions 64–69 and the development sentinel
999 do not by themselves identify the active serialization layout. For legacy
database-header values, 0–3 and historical 64 map to storage 64; values 4–7 map
to storage 65–68. The modern database value 69 identifies v2 storage. Unknown
versions and encryption/flag capabilities must be rejected, including when a
caller asks only for successor encoding.

Checkpoint publication must preserve the existing database identifier and
storage compatibility and advance the generation. It must not reset these to
fresh-file defaults simply because tables are reencoded. Any deliberate version
upgrade requires its own capability decision; preserving a v2 header alone does
not authorize a new type's checkpoint or WAL layout. Development additionally
gates empty STRUCT and TUPLE table columns to storage 69 and VARIANT columns to
storage 68, recursively through child types.

Sources: [header version mapping](../../../duckdb/src/storage/single_file_block_manager.cpp)
and [table type gates](../../../duckdb/src/catalog/catalog_entry/duck_table_entry.cpp).

For the Rust rewrite, the selected format may bind a small owned checkpoint
encoder that retains publication metadata. The file layer neither interprets
that state nor rereads/retains all prior table bytes per commit. Construct and
validate the successor binding before external publication; install it only
after publication succeeds. Definite failures keep the prior binding usable.
Uncertain outcomes block further publication until reopening. Stateful file
durability has one transaction-manager owner; connections share that manager.
Formats without such metadata keep their selected stateless encoder. These
representation choices remain provisional; the identity, failure and ownership
contracts do not depend on a particular object layout.

Native snapshot serialization takes its type services from the snapshot's
retained registry, including recursive child/statistics encoding. WAL child
encoding uses the selected log context. Neither path may construct builtin
type services to interpret dynamic values. Native statistics still describe
the native physical ordering; a replacement SQL comparator cannot redefine
the C++ file's statistics ordering. The current synchronous snapshot encoder
has a background maintenance context, not a new promise of interruptible
filesystem operations or a global byte budget.

The provisional native writer can explicitly select storage 64–69 for a new
image; the legacy default remains 64. That preference does not upgrade existing
files. Check recursive table-type capabilities before emitting data, including
empty tables and CREATE/ALTER paths. Ordinary FileCheckpoint publication can
write canonical unshredded VARIANT at storage 68 or newer, and positional TUPLE
and empty STRUCT at storage 69. Reading a shredded column does not require the
successor writer to preserve its compression or shredding strategy, but it must
preserve exact logical content and the existing file identity/version.

WAL publication has a separate capability boundary. Until the selected log
session retains the actual checkpoint version and supports the matching wire
representation, reject VARIANT, TUPLE and empty STRUCT recursively before log
publication. VARIANT recovery checkpoint publication also remains unavailable
until layout validation can compare canonicalized content exactly. SQL equality
is not sufficient: equal numeric values with different tags, widths or floating
bits are not interchangeable durable payloads. These are current implementation
limits, not a reduced target for the value-and-expression milestone.

Strict physical layout validation must compare floating-point bits recursively,
not just in top-level columns. It must accept identical nested NaN payloads and
reject changed payload bits or zero signs. Container metadata, ordering, union
tags and child NULLs remain exact. The current bounded comparison uses borrowed
row views, checks cancellation, and limits each row to 16 million logical values
and depth 64; shared allocations do not waive the logical visit count. This
physical check does not authorize native VARIANT canonicalization or SQL equality
as a substitute for format-owned content validation.

Development storage 69 can persist noncontiguous row groups. Catalog property
105 carries `next_row_id`, defaulting to total physical rows when absent; it is
not redundant row-count metadata. Restore each stored row-group start and the
append high-water mark independently of live/physical cardinality. Check ordered,
nonoverlapping ranges, arithmetic bounds and version capability before restoring
rows or applying a WAL. Source: [table data writer](../../../duckdb/src/storage/checkpoint/table_data_writer.cpp),
[checkpoint reader](../../../duckdb/src/storage/checkpoint_manager.cpp), and
[row-group collection](../../../duckdb/src/storage/table/row_group_collection.cpp).

Three complementary test classes are required: ordinary close/reopen and checkpoint tests; process-interruption/WAL replay tests; and injected file-write or synchronization failures. The native storage fuzzer performs operation sequences with one-shot filesystem faults and verifies the next reopen against the last expected state. It is not a general malformed-database-byte generator.

Assertions should separate acknowledged commits, rejected commits, and indeterminate external failures. Check table contents, catalog objects, indexes, and future database usability, not just whether opening succeeds. Historical storage files and cross-version readers add a separate format-compatibility obligation described in [compatibility testing](../testing/compatibility.md). Relevant code and execution limitations for fault campaigns are in [fuzzing](../testing/fuzzer.md) and [stress](../testing/stress.md).
