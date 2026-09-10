# Nested values: implementation record

This is an in-progress part of the [value-and-expression milestone](../specs/value-expression-milestone.md), not a completed family or parity claim.

## First integration increment

The provisional model shares recursive metadata and scalar payloads behind `Arc` while keeping the outer `DataType` at at most 16 bytes and `Value` at at most 32 bytes. LIST, ARRAY, STRUCT, MAP, UNION and VARIANT have distinct metadata/payload shapes. Shape validation checks child physical types, ARRAY cardinality, map non-NULL keys and UNION tag bounds. The selected type registry additionally validates child adapters, duplicate MAP keys and metadata limits. The registry binds child comparisons and keys once; changing the execution context's registry does not select new child implementations.

Implemented SQL paths include LIST literals and `list_value`, `array_value`, STRUCT dictionary literals, LIST subscripts, STRUCT field/subscript extraction, `list_extract`/`array_extract`, `struct_extract`, `map`, declared LIST/ARRAY/STRUCT/MAP/UNION types, and recursive LIST/ARRAY/STRUCT/MAP casts. STRUCT casts match target fields by name and fill absent target fields with NULL. Nested child NULL compares greater than a non-NULL child and equals another child NULL; container NULL remains the consuming operator's responsibility. MAP equality and order retain entry order. Canonical keys frame every child, including NULL, through its retained adapter.

The first four component tests exercise:

- constructors, decimal children, array shape failures, duplicate/NULL MAP keys, failed field access and nested comparison;
- nested values through joins, grouping, DISTINCT, min/max, sorting, partitioned windows and SQL UNION;
- child/container NULL, canonical keys, selected dictionary vectors and compact outer representations;
- a STRUCT containing DECIMAL, DATE and LIST(UBIGINT) through insert, update/delete rollback, prepared payload extraction, a JSON checkpoint and reopen.

`cargo check`, `cargo test --test nested` (4 passed), and `cargo clippy --workspace --all-targets -- -D warnings` pass for this increment. The full workspace suite passed with two existing ignored external-CLI analytics tests. A subsequent physical-depth guard rejects 66 nested VARIANT containers, and composite coercion ranking now checks availability through the selected child cast registry; focused nested/type/cast/operator tests passed again after those changes (4/12/8/8). Instrumentation coverage reports 220 files, 1,927 functions, 199 interface methods and no missing attributes. The latest trace check passed in 26.44 seconds and deleted temporary telemetry. Integrated Kani evidence is pending the first combined family checkpoint; these commits are internal steps, not completion of the nested assignment. No acceptance benchmarks have been run for these changes.

## Reference observations

Lightweight probes use pinned development `99063af2bd7092aff02e14184a20e24699d34d71`:

- `[1,NULL] > [1,2]` and `[1,NULL] = [1,NULL]` are true.
- MAP entry reordering changes equality, even when the same key/value pairs occur.
- `{'a':1,'b':2}::STRUCT(b BIGINT,c VARCHAR)` produces `{'b': 2, 'c': NULL}`; a STRUCT cast with no common field fails binding.
- `union_value(i:=NULL)` is a non-NULL UNION with a NULL active member. Its tag is the anonymous enum of declared member names.
- Development additionally introduces TUPLE identity; its older-file representation uses an unnamed STRUCT. This remains within the nested work inventory.

These observations guide implementation; they are not a full differential campaign.

## Remaining work

All six requested families remain in scope. UNION SQL construction/extraction, member coercion and enum tags; VARIANT dynamic semantics; TUPLE/unnamed rows; list aggregation and remaining nested functions; slicing, lambdas and shape/coercion edge cases; formatting and mixed-family semantics need expansion. ARRAY/common-type, STRUCT field-union and composite overload ranking rules need broader reference coverage.

Native DuckDB nested checkpoint/WAL codecs are not implemented in this increment. The private JSON round trip is not native compatibility evidence. Native default and ART encoding reject nested values explicitly, and native nested indexes must follow reference-supported behavior rather than a blanket assumption that every type is indexable. Further tests must include nested registry replacement, adversarial payload/resource cases, alternate execution/index compositions, mixed temporal/scalar families, independent C++ files, native rollback/recovery/reopen, upstream regressions and isolated faster-reference performance campaigns.
