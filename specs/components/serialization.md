# Serialization and format evolution

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Serialization boundary

Serialization connects several otherwise distinct components: parsed syntax, bound expressions/logical plans, catalog metadata, table storage and WAL records. The declarative [serialization schemas](../../../duckdb/src/include/duckdb/storage/serialization/) define object fields, while [BinarySerializer](../../../duckdb/src/include/duckdb/common/serializer/binary_serializer.hpp) and [BinaryDeserializer](../../../duckdb/src/include/duckdb/common/serializer/binary_deserializer.hpp) implement the binary protocol, including field IDs and optional properties. Generated implementations live under [storage/serialization](../../../duckdb/src/storage/serialization/).

Deserializing a bound plan can require a `ClientContext`, bound parameter map, catalog/function resolution and extension-specific bind-data reconstruction. Persisting an arbitrary process pointer would not satisfy this contract. Stable field IDs, defaults/optional fields, feature/version selection and explicit unsupported cases determine whether an old artifact remains interpretable.

Statement/plan round-tripping within one build, cross-version plan execution, and database-file reopening are separate verification obligations. They share serialization machinery but differ in the state that must be reconstructed and the compatibility policy applied.

## Schema-to-code workflow

Serialization schemas describe named properties with stable identifiers and type/default information. Generated C++ implementations dispatch concrete object types and read/write their properties using serializer/deserializer interfaces. The generator and schemas must change together; editing only generated output risks losing the change at regeneration and leaves the source-of-truth contract inconsistent.

Property identity is more important than declaration position for compatibility. Reusing an existing field identifier for a different meaning can cause an old reader to misinterpret data. Adding an optional field with a compatible default is different from adding a required field or changing a nested object's representation. Type discriminators and enum values also participate in the serialized interpretation.

Sources: [serialization schemas](../../../duckdb/src/include/duckdb/storage/serialization/), [serialization generator](../../../duckdb/scripts/generate_serialization.py), [serializer interface](../../../duckdb/src/include/duckdb/common/serializer/serializer.hpp), [deserializer interface](../../../duckdb/src/include/duckdb/common/serializer/deserializer.hpp).

## Reconstruction context and unsupported state

A parsed statement mostly reconstructs syntax-owned data. A bound expression or plan additionally depends on logical types, parameter bindings, functions, catalog objects, and extension-provided bind data. Deserialization must supply the correct contextual lookup state and reject unavailable or incompatible dependencies. An operator that opts out of serialization cannot be made portable merely by serializing its base-class fields.

A pointer/reference relationship inside one process must become a reconstructible identity, owned serialized payload, or explicit unsupported case. Function pointers, allocator addresses, mutexes, and live execution tasks are not meaningful cross-process representations. For this reason, serialized logical plans are not snapshots of an actively running executor.

## Three compatibility domains

| Domain | Required oracle | Important limitation |
| --- | --- | --- |
| Same-build statement/plan round trip | Reconstructed tree verifies and executes equivalently | Does not prove historical format support |
| Cross-version serialized plan | Writer/reader pair can reconstruct dependencies and execute | Extension and function availability still matter |
| Persistent database/WAL | Reopen/replay restores committed catalog and data | Uses storage-version policy and publication/recovery rules |

Compatibility selection must be checked at the writer and reader boundaries. A current build successfully reading its own output is the minimum local case, not a general forward- or backward-compatibility guarantee.

## Verification requirements

Regenerate implementations and check for unexpected differences; run copy/serialize verification and targeted tests for each new or changed field. Exercise omitted defaults, unsupported operators, parameterized plans, custom bind data, nested types, historical artifacts, and malformed/truncated data. The [compatibility harness](../testing/compatibility.md) specifies plan versus database-file checks; [configuration](../testing/configuration.md) controls in-query verification. Record the exact writer/reader commits and storage compatibility target when reporting cross-version results.
