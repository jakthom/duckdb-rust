# Logical types and scalar values

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Type and value model

`LogicalType` describes SQL semantics; physical types describe machine representation. `Value` is a scalar container used for constants, API conversion, configuration, and scalar access. Runtime query processing primarily operates on vectors and chunks rather than repeated scalar `Value` access.

Types include numeric, decimal, string/binary, temporal, enum and nested families. `LIST`, `ARRAY`, `STRUCT`, `MAP`, `UNION`, and `VARIANT` need child storage and validity rules in addition to a top-level tag. This checkout also contains geometry-related common/function/storage code; the much larger spatial extension remains separately configured. Aliases, custom types, casts and type constructors are extensibility surfaces.

Sources: [types.hpp](../../../duckdb/src/include/duckdb/common/types.hpp), [type implementations](../../../duckdb/src/common/types/), [cast rules](../../../duckdb/src/function/cast_rules.cpp), [casts](../../../duckdb/src/function/cast/).

## Representation and interface boundaries

`LogicalType` contains a logical identifier, a physical representation, and optional shared immutable `ExtraTypeInfo`. Extra information carries parameters that a tag alone cannot describe: decimal precision/scale, child types and field names, enum dictionaries, or other specialized metadata. Copying a logical type can share this metadata; `Copy` and `DeepCopy` provide stronger copy behavior where needed. Type equality must therefore account for parameters, not just the physical storage width.

`Value` carries a logical type, NULL state, and scalar or nested payload. Its purpose is convenient ownership at scalar boundaries: parsed constants, bound parameters, catalog/configuration values, and client conversion. It is not the per-row representation used by hot vectorized execution loops. A typed NULL is not an untyped zero, an empty string, or an empty child collection. Nested containers can be NULL independently of their children.

Sources: [LogicalType declarations](../../../duckdb/src/include/duckdb/common/types.hpp), [Value declarations](../../../duckdb/src/include/duckdb/common/types/value.hpp), [extra type metadata](../../../duckdb/src/include/duckdb/common/extra_type_info.hpp).

## Type resolution and conversion

Binding selects common types and inserts casts before most physical execution. `MaxLogicalType`, `TryGetMaxLogicalType`, and related helpers expose common-type selection; context-free `Default*` variants use built-in behavior and must not be assumed equivalent to context-aware resolution when extensions add type behavior. SQL overload resolution, assignment, and explicit casts have different acceptance requirements.

`Value::CastAs` and `TryCastAs` use a cast-function set and context where provided. The default variants provide context-free conversions. The throwing and optional-result APIs encode distinct failure contracts; callers must not replace failed conversion with a legitimate SQL NULL unless that is the intended SQL operation. Vectorized casts apply the corresponding conversion semantics across a batch, including NULL propagation and per-value conversion errors.

Decimal arithmetic must preserve both precision and scale constraints; temporal types have units and semantic distinctions that cannot be recovered from an integer's width alone. String and binary values differ even when both have byte storage. LIST and fixed-size ARRAY differ in shape constraints. MAP and UNION add semantic structure over nested storage. These are engineering obligations derived from the type model, not permission to interchange physically similar types.

## Cross-component invariants

The parser can represent an unresolved type expression; binding resolves it into a concrete `LogicalType`; physical planning and vector allocation depend on that resolved type. Serialization must retain the metadata needed to reconstruct the same type. Arrow and public APIs must express supported logical distinctions or reject unsupported conversions explicitly. Extension-defined type identity and lifetime must survive catalog lookup and prepared execution.

## Verification requirements

Use type/cast SQL cases and native common/vector tests from [component/API coverage](../testing/component-api.md). Include boundary numeric values, failed narrowing casts, decimal rounding/overflow, temporal boundaries, embedded binary zeros, nested NULLs, empty collections, aliases, and type serialization round trips. Compare logical values and type metadata independently: equal formatted output does not prove identical types. External-format round trips belong additionally to [file-format](file-formats.md) and [Arrow](arrow-adbc.md) verification.
