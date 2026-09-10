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

The provisional selected common-type contract accepts optional integer-literal
provenance separately from declared types. It does not evaluate an expression
to manufacture a literal, grant a global implicit narrowing cast, or attach
literal identity to stored values. Hints must fit their signed underlying type.
Builtin signed and exact-numeric adapters can propose a fitting integral target
for one literal and one concrete type; two literals combine their underlying
types. Ordinary no-hint inference is unchanged. Existing replacement adapters
default to their registry-aware proposal, and each hint moves with its operand
when a distinct-family proposal reverses roles. Conflicts and invalid returned
metadata still reject binding. A template binder separately owns the source's
ordered identical-literal/NULL rules described in the expression specification.

`Value::CastAs` and `TryCastAs` use a cast-function set and context where provided. The default variants provide context-free conversions. The throwing and optional-result APIs encode distinct failure contracts; callers must not replace failed conversion with a legitimate SQL NULL unless that is the intended SQL operation. Vectorized casts apply the corresponding conversion semantics across a batch, including NULL propagation and per-value conversion errors.

Source provenance can affect accepted text independently of explicit/implicit
conversion and error recovery. Development's VARIANT fallback invokes
`DefaultTryCastAs` with strict input conversion. TIME/TIME_NS then require a
complete clock string, reject a final one-digit minute without seconds, and do
not retry timestamp text. TIMETZ deliberately parses its clock fields
non-strictly, but strictly consumes the offset and disables timestamp fallback.
Timestamp cast entry points do not forward that strict argument; do not infer
one universal temporal policy. DATE has separate strict year-length and suffix
rules. These are source observations, not a claim that every Rust path is
implemented. A selected-adapter implementation must preserve its replacement,
validation, cancellation and failure-provenance contracts when carrying such
context through nested extraction.

Sources: [VARIANT fallback](../../../duckdb/src/function/cast/variant/from_variant.cpp),
[clock parsing](../../../duckdb/src/common/types/time.cpp),
[cast entry points](../../../duckdb/src/common/operator/cast_operators.cpp),
[calendar parsing](../../../duckdb/src/common/types/date.cpp).

Ordinary DATE casting validates a timestamp suffix but returns the original
calendar date, including for a 24:00 clock. A DATE outside the timestamp range
must not be rejected solely because timestamp construction overflows: the
development cast retries suffix validation with a placeholder calendar date.
This conversion policy is distinct from fully consuming calendar-only parsing
and strict VARIANT parsing. Implementations may reuse the parsed suffix without
copying an unbounded input string, but must retain clock validity, checked
arithmetic, cancellation and the format/range diagnostic distinction.

Source observation: a non-window call whose final qualified identifier is
`date` (case-insensitive, including quoted names) becomes a DATE cast during
parsing. The development transformer checks one argument and discards argument
names, DISTINCT, local ordering, FILTER, WITHIN GROUP and null-treatment
modifiers. OVER takes an earlier window-function branch and is not cast sugar.
The implementation must retain normal cast selection rather than introducing a
scalar catalog entry or bypassing a selected adapter. A single infix colon is
invalid syntax; dictionary, named-argument and slice colons have separate grammar
roles and must not be rejected globally.

Source: [expression transformer](../../../duckdb/src/parser/peg/transformer/transform_expression.cpp).

Decimal arithmetic must preserve both precision and scale constraints; temporal types have units and semantic distinctions that cannot be recovered from an integer's width alone. String and binary values differ even when both have byte storage. LIST and fixed-size ARRAY differ in shape constraints. MAP and UNION add semantic structure over nested storage. These are engineering obligations derived from the type model, not permission to interchange physically similar types.

## Cross-component invariants

The parser can represent an unresolved type expression; binding resolves it into a concrete `LogicalType`; physical planning and vector allocation depend on that resolved type. Serialization must retain the metadata needed to reconstruct the same type. Arrow and public APIs must express supported logical distinctions or reject unsupported conversions explicitly. Extension-defined type identity and lifetime must survive catalog lookup and prepared execution.

### Dynamic VARIANT storage and reconstruction

Source observation: VARIANT logical ID109 carries canonical physical children
`keys VARCHAR[]`, `children STRUCT(keys_index UINTEGER, values_index UINTEGER)[]`,
`values STRUCT(type_id UTINYINT, byte_offset UINTEGER)[]`, and `data BLOB`.
Persistent columns have independent root validity and unshredded children; an
optional ordinary typed tree describes shredded values. This physical STRUCT
layout does not turn the logical column into a user STRUCT.

Shredded wrappers distinguish a missing OBJECT field (NULL typed value and
leftover index zero), a present NULL (NULL typed value and NULL leftover index),
and an unshredded leftover (one-based index). A present typed primitive or ARRAY
does not consult its unused leftover index. OBJECT reconstruction merges typed
and leftover fields. Development's canonical shredded-vector reconstruction
emits OBJECT keys lexicographically, including leftover subtrees; an ordinary
unshredded column retains its stored member ordering. Root NULL uses row validity,
not the nested VARIANT_NULL tag.

The canonical builder emits value descriptors in preorder and reserves each
container's contiguous child-reference range before recursively emitting its
children. Payload offsets are relative to the row's byte buffer. Counts, child
starts and variable-length scalar lengths use bounded UINT32 varints. A nested
VARIANT_NULL tag carries no declared scalar type: an INTEGER NULL child and a
VARCHAR NULL child have the same dynamic wire category. Retaining non-NULL
scalar widths and temporal units must not be confused with preserving those
pre-cast typed-NULL hints.

JSON-to-VARIANT conversion retains empty keys and case-distinct keys, and
collapses only exact duplicate keys, keeping the last value. Engineering
implication: a dynamic OBJECT representation must not silently inherit ordinary
SQL STRUCT's nonempty, case-insensitively unique field-name restrictions.
Readers must retain declared scalar widths and units, validate offsets and child
references before following them, and bound depth, repeated visits and logical
materialization independently. Publication compatibility is a separate
requirement from recognizing a read-side layout.

Engineering implication for checkpoint layout validation: native canonicalization
can replace a retained STRUCT with OBJECT metadata, normalize LIST/ARRAY/TUPLE
to a dynamic ARRAY, resolve UNION/VARIANT wrappers, map ENUM labels to VARCHAR,
and erase pre-cast typed-NULL hints. Exact content equivalence may recognize
these changes only. It must preserve non-NULL scalar tags and widths, decimal
precision/scale/coefficient, floating payload bits (including signed zero and
NaN payloads), BIGNUM negative zero, BIT length, temporal physical fields,
ordered exact OBJECT names, child NULLs and root row validity. SQL VARIANT
comparison or equality keys are not a suitable oracle: they intentionally
equate several representation-distinct scalar values. Retained selected type
validation remains required before a bounded native-content traversal; no
implicit cast or ambient adapter replacement is authorized by this equivalence.

Sources: [canonical types](../../../duckdb/src/common/types.cpp),
[VARIANT column storage](../../../duckdb/src/storage/table/variant_column_data.cpp),
[iterators](../../../duckdb/src/common/types/variant/variant_iterator.cpp),
[canonical builder](../../../duckdb/src/include/duckdb/common/types/variant/variant_builder.hpp),
[JSON conversion](../../../duckdb/src/include/duckdb/function/cast/variant/json_to_variant.hpp).

## Verification requirements

Use type/cast SQL cases and native common/vector tests from [component/API coverage](../testing/component-api.md). Include boundary numeric values, failed narrowing casts, decimal rounding/overflow, temporal boundaries, embedded binary zeros, nested NULLs, empty collections, aliases, and type serialization round trips. Compare logical values and type metadata independently: equal formatted output does not prove identical types. External-format round trips belong additionally to [file-format](file-formats.md) and [Arrow](arrow-adbc.md) verification.
