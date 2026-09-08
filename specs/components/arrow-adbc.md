# Arrow interchange and ADBC

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Arrow and ADBC

Arrow support includes importing/exporting schemas, arrays and streams; mapping logical/nested types; and keeping external buffers/dependencies alive. Standard `ArrowSchema`, `ArrowArray`, and `ArrowArrayStream` release callbacks are ownership boundaries. Dictionary, offset, nested validity and versioned layout cases require explicit tests. Arrow interoperability does not imply that every conversion is zero-copy.

ADBC support is a separate driver-facing interface layered over the engine and Arrow. Its tests load the relevant library through a driver manager rather than merely testing native vector code.

Sources: [common Arrow code](../../../duckdb/src/common/arrow/), [ADBC](../../../duckdb/src/common/adbc/), [Arrow table functions](../../../duckdb/src/function/table/arrow.cpp), [Arrow tests](../../../duckdb/test/arrow/), [ADBC tests](../../../duckdb/test/api/adbc/).

## Data ownership and schema conversion

Arrow conversion is a boundary between DuckDB logical types/vectors and Arrow schemas/arrays. `ArrowConverter::ToArrowSchema` produces a schema from names/types and client properties. `ToArrowArray` exports a chunk using the requested conversion properties. Nested appenders implement type-specific output for structures, lists/list views, maps, unions, enums, strings, and scalar values.

`ArrowArrayWrapper` and `ArrowArrayStreamWrapper` manage release callbacks and stream iteration. `GetNextChunk` returns a retained array wrapper; imported vectors must keep that owner's buffers alive for as long as references remain. A stream release does not authorize reading previously freed buffers. Conversely, ownership transfer must not result in both caller and wrapper invoking the same release operation twice.

Sources: [arrow_converter.hpp](../../../duckdb/src/include/duckdb/common/arrow/arrow_converter.hpp), [arrow_wrapper.hpp](../../../duckdb/src/include/duckdb/common/arrow/arrow_wrapper.hpp), [Arrow appenders](../../../duckdb/src/include/duckdb/common/arrow/appender/).

## Import and export semantics

Schema interpretation includes child order, dictionary encoding, extension metadata, temporal units, decimal precision, and offset/length conventions. Sliced arrays can have nonzero offsets, including nested offset/validity interactions. DuckDB vector selections and Arrow array offsets are different mechanisms; conversion must compose them correctly.

Some types/layouts can reference existing memory; others require conversion or owned output buffers. Exporting a flat primitive column and exporting strings or a nested dictionary do not have identical zero-copy properties. Output batch size also differs from total query cardinality. Consumers must read until end of stream rather than assuming one batch contains the result.

Arrow scans are table-function integrations, so projection/filter capabilities, parallelism, and external dependencies interact with [function binding](functions.md) and [execution](execution.md). An external producer can fail after returning a valid schema; stream errors must reach the query's error channel and still trigger cleanup.

## ADBC driver boundary

ADBC provides database/connection/statement handles, options, statement execution, metadata operations, and Arrow result exchange through the driver interface. The implementation includes wrappers and a single-batch stream helper; it is a distinct adapter, not a replacement for DuckDB's native C handle APIs. Driver initialization/version negotiation and driver-manager loading form part of deployment.

The driver must translate engine failures into ADBC status/error ownership conventions and release engine resources when driver objects are released. Parameter binding and result streams cross both an ADBC object-lifetime boundary and an Arrow buffer-lifetime boundary.

Sources: [ADBC headers](../../../duckdb/src/include/duckdb/common/adbc/), [driver implementation](../../../duckdb/src/common/adbc/).

## Verification requirements

Test sliced arrays, nonzero offsets, dictionaries, nested validity, empty batches, multiple batches, producer errors, schema/type metadata, and delayed release. Exercise importing from and exporting to an independent Arrow consumer. ADBC tests additionally load through the driver manager and cover driver/connection/statement cleanup. The [client harness spec](../testing/clients.md) and [component/API spec](../testing/component-api.md) describe prerequisites and oracles; native vector tests alone cannot verify foreign release callbacks.
