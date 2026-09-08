# Buffer management, memory, spill, and caching

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Buffer, spill, and cache contracts

`BufferManager` allocates and pins managed blocks, tracks memory, provides prefetch operations, and exposes temporary storage. `StandardBufferManager` and `BufferPool` implement eviction/accounting policies. A `BlockHandle` tracks a block; a `BufferHandle` pins accessible memory. Unpinned memory cannot be treated as a stable raw address.

`TemporaryMemoryManager` coordinates memory reservations among memory-intensive operators. `TemporaryFileManager` supports spill. External-file caching is separate from native table storage and from the general object cache; cache invalidation and remote file metadata affect correctness as well as performance.

Sources: [buffer_manager.hpp](../../../duckdb/src/include/duckdb/storage/buffer_manager.hpp), [standard_buffer_manager.cpp](../../../duckdb/src/storage/standard_buffer_manager.cpp), [buffer pool](../../../duckdb/src/storage/buffer/), [temporary files](../../../duckdb/src/storage/temporary_file_manager.cpp), [external file cache](../../../duckdb/src/storage/external_file_cache/).

## Allocation and pinning protocol

The buffer manager distinguishes temporary allocations, block-manager-backed allocations, direct buffer allocation, and pinning an existing block handle. `BufferHandle` represents the interval in which the bytes are resident and accessible. Releasing the pin permits eviction according to the block's ownership and destruction policy; retaining a `BlockHandle` preserves identity, not necessarily a stable address.

`Prefetch` and `CreatePrefetchTasks` allow I/O preparation to be scheduled before consumption. They do not remove the consumer's obligation to pin and handle errors. Resizing, reserving memory, accounting by `MemoryTag`, and selecting destroyable versus spillable storage must agree: the manager cannot safely evict arbitrary external pointers or reconstruct data that was marked disposable but is still needed.

## Operator memory reservations

`TemporaryMemoryManager` is owned by the buffer pool and coordinates concurrent `TemporaryMemoryState` objects. A registered state remains active while it is in scope. It records remaining desired size, a minimum reservation, current reservation, and a materialization penalty used in allocation decisions.

`SetRemainingSize` does not itself update the reservation. `SetRemainingSizeAndUpdateReservation` and `UpdateReservation` do; `SetZero` clears both remaining demand and reservation. This distinction matters when an operator switches from build to probe/output or finishes a partition. Keeping stale demand can starve other operators; assuming a requested size was granted can exceed the intended managed budget.

The policy considers the current memory limit, temporary-directory availability, thread/connection settings, and per-query constraints. The header's reservation constants are implementation tuning parameters, not a public guarantee that every operator receives its full working set.

Source: [temporary_memory_manager.hpp](../../../duckdb/src/include/duckdb/storage/temporary_memory_manager.hpp).

## Spill, caches, and failure behavior

Spilling transfers reconstructible intermediate data to temporary storage and later reloads it through managed buffers. It can fail because of memory pressure, unavailable temporary storage, I/O errors, or cancellation. An operator must have a correct out-of-core algorithm; assigning it a temporary directory does not automatically make every allocation spillable. The configured managed-memory limit should not be described as an exact process-RSS ceiling because non-buffer allocations and external libraries also consume memory.

Object caching, external-file caching, and native block buffering retain different objects and have different invalidation keys. A remote file's metadata/change semantics are especially important: stale cached bytes cannot be treated as the current file simply because their path matches.

## Verification requirements

Test pin/unpin/reload, low-memory pressure, simultaneous memory-intensive operators, absent/full temporary directories, cancellation during I/O, and cleanup after exceptions. Force external execution and compare results with in-memory execution. Measure memory and I/O using the separate [stress](../testing/stress.md) and [I/O metric](../testing/io-metrics.md) harnesses; a query-result match alone does not prove bounded memory or effective prefetching.
