# CSV, Parquet, JSON, and COPY

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## File scans and COPY

CSV ingestion is implemented in core, including dialect/type sniffing, encoding, buffering, scanner state machines, parallel partitions and reject/error handling. The common multi-file layer handles file lists, schema reconciliation, partition information and shared scan concerns. Parquet and JSON provide format-specific scanners/writers through extension registration.

Projection and predicate pushdown are negotiated capabilities. A scan can expose filter/statistics hooks and nested projections, but the planner must preserve residual filters that the source cannot fully enforce. File boundaries, record boundaries, schema changes and parallel partitions must preserve exactly-once row production.

Sources: [CSV scanner](../../../duckdb/src/execution/operator/csv_scanner/), [multi-file layer](../../../duckdb/src/common/multi_file/), [Parquet extension](../../../duckdb/extension/parquet/), [JSON extension](../../../duckdb/extension/json/), [COPY execution](../../../duckdb/src/execution/operator/persistent/).

## Shared multi-file contract

The multi-file layer expands file lists, reconciles reader schemas with the requested output schema, carries partition/virtual-column information, and coordinates per-file scan state. A file's physical column ordinal need not match its output ordinal after name-based schema reconciliation or projection. Missing columns, casts, and partition-derived values must be handled consistently across every file.

Metadata discovery and schema sampling can happen during binding before row production. A prepared file scan may therefore hold bind data that describes external objects whose contents can later change. Reader repeatability, cache metadata, and reopen behavior must not be inferred from native-table MVCC. External files do not automatically participate in a DuckDB transaction snapshot.

## CSV scanner component

CSV separates dialect/type sniffing, buffer management, state-machine tokenization, row/field assembly, type conversion, and error/reject handling. Parallel boundaries must find valid record transitions; a quoted newline is not an ordinary row separator. Reader options determine delimiter, quote/escape behavior, headers, NULL representations, encodings, and schema inference. Sampling proposes a schema but does not prove every later row conforms.

The scanner must handle records crossing buffer boundaries, very long fields, empty fields versus NULLs, escaped quotes, and partial final input. Reject collection is an explicit mode with diagnostics, not permission to silently discard errors in ordinary reads. The [CSV scanner interfaces](../../../duckdb/src/include/duckdb/execution/operator/csv_scanner/) and implementation form a component independent of SQL parsing.

## Parquet component

The extension separates file/row-group metadata, column schemas, page/encoding readers, nested-value reconstruction, statistics, prefetch decisions, and writers. Projection can avoid reading unneeded columns; statistics can exclude eligible row groups/pages only when their interpretation is valid. Definition/repetition levels and logical annotations govern nested/nullable reconstruction; physical primitive types alone are insufficient.

Writing must coordinate schema, column encoders, row groups, statistics, metadata/footer publication, and output lifecycle. Type mappings include decimals, temporal values, nested structures, and specialized logical types. Compression and encryption support must be checked against the selected configuration. Source entry points are the [Parquet extension](../../../duckdb/extension/parquet/) and its [interfaces](../../../duckdb/extension/parquet/include/).

## JSON component

The JSON extension provides scalar document operations as well as file readers/writers. These are different front doors: a scalar JSON string is already bounded by a SQL value, while a reader must discover document/record boundaries, sample structure, infer or honor a schema, and transform parsed values into typed vectors. Missing fields, JSON null, SQL NULL, mixed record types, and malformed input require explicit handling.

`json_reader`, `json_scan`, `json_structure`, and `json_transform` separate those responsibilities. Reader errors may occur after successful schema sampling. See [JSON interfaces](../../../duckdb/extension/json/include/) and [implementation](../../../duckdb/extension/json/).

## COPY output and verification

Physical COPY operators coordinate serial/parallel or batch output, writer state, partitioned outputs, and finalization. Partial file creation is not equivalent to a committed native-table change; recovery and cleanup are format/output-lifecycle responsibilities.

Test each format's reader and writer independently, then round-trip representable logical values. Include schema variation, nested NULLs, buffer/page boundaries, compressed input, projection/filter equivalence, parallel exactly-once production, and malformed files. [Fuzzing](../testing/fuzzer.md) gives the precise CSV/JSON/Parquet byte-entrypoint coverage; [I/O metrics](../testing/io-metrics.md) checks scan efficiency; [client tests](../testing/clients.md) cover external readers and Arrow interoperability. A self-round-trip alone can hide a matching reader/writer bug.
