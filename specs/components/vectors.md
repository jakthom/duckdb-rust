# Vectors, chunks, and intermediate collections

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Vector/chunk contract

| Abstraction | Representation and obligations |
| --- | --- |
| `Vector` | Typed column values, size, validity, and backing buffers; may reference another representation |
| Flat vector | Direct element storage |
| Constant vector | One value logically repeated |
| Dictionary vector | Selection over another vector; logical row indexes must be mapped |
| Sequence vector | Start/increment representation |
| FSST vector | Compressed strings |
| Shredded vector | Specialized representation for decomposed variant data |
| `SelectionVector` | Logical-to-physical row selection |
| Validity mask | SQL NULL state independent of payload bits |
| `DataChunk` | Collection of typed vectors with consistent row cardinality |
| Column/tuple collections | Retained columnar or row-oriented intermediate data, including partitioned/batched forms |

The default standard vector size is 2,048 rows, defined by [vector_size.hpp](../../../duckdb/src/include/duckdb/common/vector_size.hpp). It is a build default, not a universal constant applications should hard-code. Short chunks are normal, and tests deliberately rebuild with other vector sizes.

This revision's vectors carry their own sizes. Typed reading uses `Vector::Values<T>()` and related iterators; writing uses typed vector writers. Writers update vector size and enforce their expected write count. `DataChunk` provides cardinality consistency operations; older `SetCardinality` overloads are deprecated. Element access must handle compressed/selected vectors, nested child lifetimes, and NULLs correctly.

For scalar element-wise operations, reusable unary/binary/generic executors handle common vector patterns. Type-erased storage and execution code can still require `UnifiedVectorFormat`. A general function must not assume every input is flat or that every payload is valid. The list iterator has a documented dictionary-vector exception requiring flattening when applicable.

Sources: [data_chunk.hpp](../../../duckdb/src/include/duckdb/common/types/data_chunk.hpp), [vector types](../../../duckdb/src/include/duckdb/common/enums/vector_type.hpp), [vector iterators](../../../duckdb/src/include/duckdb/common/vector/vector_iterator.hpp), [vector writers](../../../duckdb/src/include/duckdb/common/vector/vector_writer.hpp), [vector operations](../../../duckdb/src/common/vector_operations/).

## Allocation, references, and shape

`DataChunk::Initialize` allocates or prepares child vectors using the declared output types; `InitializeEmpty` creates the typed shape without vector-data allocation. `Reset` returns a chunk to reusable initialized state. `Reference`, `ReferenceColumns`, and vector `Reference` share existing storage; they are not deep-copy operations. `Slice` can create selected views. Code retaining input beyond the current call must retain the associated buffers or materialize owned data before the producer resets/reuses its chunk.

A vector's logical row count, its physical payload length, and a nested child collection's size are different quantities. A constant vector may represent many rows using one value. A dictionary maps row indexes through a selection. A list row uses child offset/length metadata; child values do not have one-to-one cardinality with parent rows. The parent NULL mask does not substitute for child validity.

`Flatten()` changes representation to a flat vector without changing logical values; the method is const-qualified even though its internal buffers and vector type can change. It should be invoked for a genuine interface requirement, not assumed to be a free operation. The deprecated count-taking overloads are evidence of a transition to vector-owned cardinality. New code must keep the chunk's declared cardinality consistent with all children rather than relying on legacy setters to resize data implicitly.

Sources: [vector.hpp](../../../duckdb/src/include/duckdb/common/types/vector.hpp), [data_chunk.hpp](../../../duckdb/src/include/duckdb/common/types/data_chunk.hpp).

## Batch transformation protocol

A typical filter evaluates a Boolean expression, obtains selected row indexes, and slices input into output. A projection can reference unchanged columns while evaluating other expressions into owned output vectors. A sink retaining many batches moves data into a column or tuple collection with an explicit allocator and retention policy. Result collectors similarly turn transient operator output into result-owned data or a bounded streaming buffer.

Validity must be applied after the representation's row mapping. Reading `data[i]` and `validity[i]` from an arbitrary input is insufficient for a dictionary or constant. String results additionally need heap/buffer ownership: copying a string descriptor does not necessarily copy its bytes. `AddHeapReference` is one mechanism for preserving backing storage when values reference another vector's heap.

## Engineering acceptance criteria

Every generic vector operation should be checked with flat, constant, selected/dictionary, all-NULL, mixed-NULL, empty, and short final batches. Nested operations need independent parent/child validity and empty-child cases. Execute across alternate standard-vector-size builds to expose fixed-size assumptions. `Vector::Verify` and `DataChunk::Verify` are debug consistency checks, not a substitute for semantic tests or bounds checks in public interfaces.

Tests should compare representation-independent values while separately asserting intended zero-copy behavior where that is the feature. The [expression executor](expressions.md) consumes these contracts; the [configuration harness](../testing/configuration.md) enables representation verification modes and the [component/API harnesses](../testing/component-api.md) cover vector and collection behavior.
