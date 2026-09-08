# Transactions and MVCC

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Transaction layers and visibility

| Layer | Responsibility |
| --- | --- |
| `TransactionContext` | Connection transaction lifecycle, autocommit and explicit transaction state |
| `MetaTransaction` | Coordinates lazily created per-attached-database transactions |
| `TransactionManager` | Abstract start/commit/rollback behavior for a database implementation |
| `DuckTransactionManager` | Native transaction IDs/timestamps, active transactions, commit/checkpoint coordination and cleanup |
| `DuckTransaction` | Native transaction view, undo state, local storage, WAL/commit work |
| `UndoBuffer` and row/update version structures | Rollback and visibility of catalog/data changes |
| `LocalStorage` | Transaction-local appended/modified table data and commit integration |

Native storage uses MVCC and optimistic conflict detection. Readers use a transaction view while writes retain the metadata needed for rollback and older readers. Concurrent connections can execute through the shared runtime, but conflicting writes can fail. This is not a promise of arbitrary simultaneous writers from separate processes; persistent file opening/locking remains a separate boundary.

One `MetaTransaction` can read multiple attached databases but can modify only one ordinary attached database. System and temporary databases are explicit exceptions in `MetaTransaction::ModifyDatabase`. This prevents interpreting attach support as a general distributed atomic commit mechanism.

Sources: [transaction_context.hpp](../../../duckdb/src/include/duckdb/transaction/transaction_context.hpp), [meta_transaction.cpp](../../../duckdb/src/transaction/meta_transaction.cpp), [duck_transaction_manager.cpp](../../../duckdb/src/transaction/duck_transaction_manager.cpp), [duck_transaction.cpp](../../../duckdb/src/transaction/duck_transaction.cpp), [local_storage.cpp](../../../duckdb/src/storage/local_storage.cpp).

## Commit protocol

At the engineering level, a native write commit coordinates these responsibilities:

1. Flush eligible transaction-local bulk-append blocks before acquiring central commit locks.
2. Determine whether normal WAL durability or an eligible checkpoint path will protect the commit.
3. Serialize WAL-writing work under the storage WAL lock while allowing permitted transaction activity to proceed.
4. Apply local/undo commit work and assign/publish committed transaction state according to the transaction-manager protocol.
5. Flush the selected durability state; revert/rollback on errors and propagate failure to the query result.
6. Retain old versions while readers can still need them, then schedule cleanup.

This is a responsibility sequence, not a replacement for the exact lock ordering in the implementation. In particular, the large automatic-checkpoint path can skip a WAL write and imposes stronger commit coordination to avoid exposing subsequent commits based on undurable state.

Sources: `DuckTransactionManager::CommitTransaction`, `DuckTransaction::PreFlushOptimisticBlocks`, `WriteToWAL`, `Commit`, and the [storage commit interface](../../../duckdb/src/include/duckdb/storage/storage_manager.hpp).

## Transaction state and write sets

`TransactionContext` determines whether an operation participates in an existing explicit transaction or an automatically scoped one. `MetaTransaction` obtains a transaction for an attached database on demand. The native transaction records its visibility horizon, transaction identity, local table storage, and undo information. Catalog changes, appended rows, deletions, and updates have different undo payloads but share commit/rollback coordination.

Local appends can accumulate columnar data before integration into committed table storage. A transaction's reads must account for its own local changes as well as snapshot-visible persistent rows. Updating or deleting existing rows requires row/version metadata; copying the latest physical bytes alone cannot provide an older reader's view. Constraint/index maintenance is part of this write protocol rather than a post-commit repair step.

Sources: [duck_transaction.hpp](../../../duckdb/src/include/duckdb/transaction/duck_transaction.hpp), [local_storage.hpp](../../../duckdb/src/include/duckdb/transaction/local_storage.hpp), [undo_buffer.hpp](../../../duckdb/src/include/duckdb/transaction/undo_buffer.hpp), [row versions](../../../duckdb/src/include/duckdb/storage/table/row_version_manager.hpp).

## Conflict, rollback, and reclamation contracts

Optimistic execution permits work to proceed before commit, but a conflicting write can still reject a transaction. Failure handling must distinguish a statement-level error, an invalidated transaction, and a database/storage failure. A client must not assume that a failed statement guarantees the enclosing transaction remains usable; query/result error propagation belongs to the [runtime](runtime.md).

Rollback restores logical visibility and reverts transaction-local or undo-recorded changes. Physical memory/block reclamation can occur later because other readers still need historical versions. Cleanup therefore uses visibility bounds and transaction-manager coordination; immediately freeing every replaced version at commit would be unsafe.

The storage commit object exposes `FlushCommit` and `RevertCommit`, and its documented destruction behavior reverts an unflushed commit. This RAII boundary protects exceptional exits, but it does not make arbitrary external side effects transactional. A COPY to an external location or an extension action needs its own lifecycle contract.

## Example visibility scenario

Consider two connections reading the same table. One starts a transaction before the other updates and commits a row. The older transaction must continue to receive the version appropriate to its snapshot, while a newly started transaction can observe the committed update. If both attempt conflicting changes, the engine may reject a write rather than merge arbitrary updates. After the old reader ends, version cleanup can become eligible. This scenario separates publication, read visibility, and reclamation.

## Verification requirements

Use multi-connection tests with explicit synchronization to exercise old readers, own writes, conflicting updates/deletes, transactional DDL, rollback after constraint failure, and commit exceptions. Durability requires reopening after commit and failure injection; in-memory result checks alone are insufficient. Exercise the one-modified-attached-database restriction and system/temporary exceptions. The [durability spec](durability.md), [storage fuzzer](../testing/fuzzer.md), and [stress harnesses](../testing/stress.md) cover distinct parts of this contract.
