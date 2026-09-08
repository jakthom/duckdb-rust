# Compression and statistics pruning

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Compression and pruning

Compression implementations provide analysis, compression, segment scan, partial scan/select/filter, point-fetch, append where supported, serialization, and block-visit hooks. Choice depends on physical type, data properties, configuration and format support.

The tree includes uncompressed/constant storage, bitpacking, RLE, dictionary/string techniques including FSST, floating-point techniques including ALP/Chimp/Patas, validity encodings and Zstandard paths. Their support sets differ; presence of a codec does not mean it is valid for every type or format version.

Segment/row-group statistics support pruning and optimizer estimates. Compression correctness includes NULL validity, partial-vector boundaries, nested children, updates, checkpoint/reload, and fallback behavior. Statistics pruning must never eliminate visible qualifying rows.

Sources: [compression](../../../duckdb/src/storage/compression/), [CompressionFunction](../../../duckdb/src/include/duckdb/function/compression_function.hpp), [statistics](../../../duckdb/src/storage/statistics/), [table scan](../../../duckdb/src/function/table/table_scan.cpp).

## Codec interface and lifecycle

`CompressionFunction` registers a compression type for a physical data type and a set of callbacks. Analysis initializes codec-specific state, consumes candidate vectors, and estimates suitability. Compression initializes checkpoint/compression state, consumes vectors, and finalizes persistent segment representation. Reading initializes segment-scan state and provides the supported full/partial scan, fetch, select, or filter paths. Optional hooks expose segment information and visit referenced block IDs.

Not every codec implements every fast path. Dispatch must use a supported callback or a correct fallback; selecting a codec based solely on its name is insufficient. `CompressionValidity` distinguishes codecs that need a separate validity representation from those that do not. Physical compression data and SQL NULL state must remain coordinated across this choice.

Source: [compression_function.hpp](../../../duckdb/src/include/duckdb/function/compression_function.hpp), [compression metadata](../../../duckdb/src/include/duckdb/function/compression_info.hpp).

## Statistics and pruning boundary

Compression analysis estimates encoding suitability; optimizer statistics estimate query properties; storage segment statistics support pruning. These may consume related data but have different correctness roles. A suboptimal codec or a poor cardinality estimate can make a query slow. An invalid min/max or NULL summary that eliminates a qualifying row makes the query wrong.

A segment filter can return a conservative result when statistics cannot establish exclusion. Updates, nested values, NaNs, collation-sensitive strings, and type-specific ordering need careful handling. Stored statistics must be serialized consistently with their column type and rebuilt or combined correctly as row groups change.

## Persistence and memory requirements

Compressed segments may reference dictionary data, overflow strings, or additional blocks. Block-visit and serialization hooks are therefore part of storage ownership, not optional diagnostics when the codec actually has external references. Checkpoint/reclamation must discover every required block. Scan state must retain pins or owned buffers while decoding and must not expose pointers into temporary scratch storage after it is reused.

Partial scans and point fetches must respect segment offsets and return the same values as a full decode. A vector boundary is not necessarily a codec-group boundary. A decoder cannot assume its input was emitted by a trusted current writer when it is reading historical or malformed persistent files.

## Verification requirements

For each supported type/codec pair, test encode/decode, empty and constant input, NULL patterns, extreme values, partial ranges, point fetch, filters, updates, and checkpoint reopen. Compare forced-codec execution with an uncompressed semantic reference and check unsupported-type rejection/fallback. Include malformed/truncated payload tests where available and sanitizers for decoding. [Configuration testing](../testing/configuration.md) exercises forced compression; [compatibility](../testing/compatibility.md) covers historical artifacts; [fuzzing](../testing/fuzzer.md) describes the actual byte-input targets, which do not exhaustively cover native codecs.
