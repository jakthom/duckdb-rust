# Native function-default dependency

The [Base64 campaign](binary-scalar-reference-base64.json) retains an independent
C++-producer failure on both pins: a BLOB column declared
`DEFAULT from_base64('AP8=')` is rejected as parsed expression class 9, kind 140.
Rust-produced checkpoint and WAL paths pass because the current catalog stores
the already evaluated default value. This asymmetry is an open native/default
semantics gap, not a Base64 result discrepancy or a passing interchange result.

This document is a source-backed wire and integration inventory. It does **not**
claim that a generic function-default decoder, retained default expressions, or
selected statement-context evaluation has been implemented. The existing native
reader continues to return Unsupported for FUNCTION defaults. The integration
lead owns that shared implementation; no Base64-specific exception is added.

## Verified parsed-expression wire shape

The correctness source is development `99063af2bd`; release `d8cdaa33fd` is also
inspected. Primary implementations are:

- Development `src/parser/expression/function_expression.cpp:130–201` and
  release `src/storage/serialization/serialize_parsed_expression.cpp:204–231`.
- Development `src/storage/serialization/serialize_nodes.cpp:370–381` for
  `FunctionArgument`, and `src/parser/qualified_name.cpp:11–19` for qualified
  identifiers.
- Both pins' `src/storage/serialization/serialize_result_modifier.cpp` and
  `src/include/duckdb/parser/result_modifier.hpp` for the nested order modifier.
- `src/include/duckdb/common/serializer/serializer.hpp` for nullable pointers
  and list element encoding; these wrappers cannot be skipped as raw objects.

Every parsed expression starts with required fields 100 (expression class) and
101 (expression kind), then optional 102 (alias), 103 (query location), and in
development 104 (32-bit query-location length). Source spans are diagnostic
metadata, not evaluation inputs. Objects end with the normal u16 terminator.
FUNCTION is class 9, kind 140.

| FUNCTION field | Payload and semantics |
| --- | --- |
| 200 | Function-name identifier; legacy scalar name remains alongside newer qualification. |
| 201 | Legacy schema identifier. |
| 202 | Legacy list of nullable parsed-expression pointers. Development writes this only for storage before V2_0_0. Argument names are stored as child aliases. |
| 203 | Nullable parsed-expression FILTER pointer. Non-null filtering is not an ordinary closed scalar default. |
| 204 | Nullable ORDER_MODIFIER pointer. A normal function constructor supplies a **non-null empty modifier**, so non-null is not itself an error. The inner required field 100 is enum **2**, and optional field 200 is the orders list. |
| 205 | Optional DISTINCT Boolean, default false. |
| 206 | Optional is-operator Boolean, default false; preserve this for selected operator binding, not generic stringification. |
| 207 | Optional export-state Boolean, default false. |
| 208 | Legacy catalog identifier. |
| 209 | Development V2_0_0 list of FunctionArgument objects. Each has optional field 100 (argument-name identifier) and optional field 101 (nullable parsed-expression pointer). |
| 210 | Development QualifiedName object. Its optional field 100 is an ordered list of identifiers, including qualification components. Written for V2_0_0 or a path that legacy catalog/schema fields cannot represent. |

Development reads the legacy children first and uses the modern arguments only
when that legacy list is empty. It transfers legacy aliases into argument names.
A nonempty modern qualified path replaces the legacy name components. Rust must
preserve those identities or explicitly reject a conflicting/unrepresentable
combination; silently dropping a schema, alias or argument name is not support.

The empty order modifier is particularly important for this witness: rejecting
every non-null field 204 would continue rejecting ordinary Base64 defaults even
after adding the FUNCTION tag. Nonempty ordering, FILTER, DISTINCT and exported
aggregate state require their own expression semantics and should remain
explicitly unsupported in an initial closed-scalar subset.

## Retained expression and binding requirements

The current `ColumnDefinition.default` is a `Value`. The native reader evaluates
CAST through `Value::cast`, and `SnapshotFormat::decode` receives only a type
registry. It has no selected function registry, cast registry, evaluator,
language binding service or statement context. Adding an evaluator call inside
the wire reader would therefore bypass selected services and freeze behavior
at database load; it is not the agreed solution.

The next shared path should retain a bounded decoded expression tree and bind
and evaluate supported closed expressions through selected statement services.
The integration lead may revise its representation. Required retained details
include declared literal type (not merely `Value::data_type()`), typed NULL,
explicit CAST/TRY_CAST nodes, argument names/aliases, qualified function identity
and operator identity. These determine literal privilege, overloads, child
casts, error recovery and prepared-insert behavior. The tree must not be an
opaque value placeholder or an SQL string reconstructed from diagnostic Display.

Pure/closed status requires the selected function/operator effects contract,
not a function-name whitelist. Unknown catalog entries, unresolved names,
nonconstant inputs and unsupported effects must fail explicitly. No volatile
default behavior is invented in the initial supported subset. The eventual
catalog/default migration also needs private/native serialization, ALTER/WAL,
prepared rebinding, rollback and reopen coverage; this wire inventory does not
establish those paths.

Malformed-field tests should cover truncated objects/pointers, invalid Boolean
tags, argument pointers without expressions, missing function names, duplicate
or conflicting legacy/modern representations, unsupported qualification, named
argument aliases, nonempty modifiers, unknown fields/classes and bounded tree
depth/node/collection sizes. Independent fixtures should retain both legacy
children and modern named-argument layouts. Those are validation obligations
for the decoder prerequisite, not tests already counted as passing here.
