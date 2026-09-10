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

## Second integration increment

UNION now has a named `union_value` constructor, field extraction, widening by declared member name, scalar-to-member selection through the selected child cast registry, and ambiguity rejection. A typed NULL cast such as `(NULL::INTEGER)::UNION(i INTEGER,s VARCHAR)` creates the active `i` member with a NULL child; untyped `NULL::UNION(...)` stays a NULL container. This required the lead's opt-in cast NULL-handling contract, with scalar and column/batch SQL regressions. `struct_pack`, bracket/field access, `list`/`array_agg`, aggregate-empty NULL results, and list window frames are exercised by the fifth nested test. All nested families explicitly decline PK/UNIQUE index admissibility while retaining equality, sorting and grouping semantics.

After incorporating the first combined temporal/BLOB/UUID base, ordinary check, nested tests (5), binary-scalar tests (4), temporal tests (2) and clippy passed. After the NULL-cast integration, nested (5), type (13) and cast (8) tests and clippy passed again. This is another integrable increment, not completion of UNION, the function catalog, or the nested milestone.

## Native checkpoint integration increment

LIST, ARRAY, STRUCT, MAP and UNION now serialize native child metadata, statistics,
validity and column streams. Native reads preserve active-NULL UNION members,
NULL containers, empty lists/maps, fixed array cardinality and decimal children.
Primitive codec selection remains through the supplied decoder registry. The
shared reader also consumes bounded development string statistics (fields
204–207) while preserving legacy statistics decoding.

Independent pinned release and development fixtures are retained under
`test/data/duckdb/nested-{release,development}/`, with producer identity, SQL,
hashes and observed codecs. Four of six files pass exact selected-value reads;
the release 125,013-row file also passes Rust publication and exact reopen. The
10,013-row development fixture remains unsupported because its VARCHAR child
uses DICT_FSST codec 15; the development 125,013-row fixture uses unsupported
ROARING codec 13 for child validity. A specifically named negative regression
records these gaps; it is not a compatibility pass. Both original failed fixtures
are retained unchanged for decoder follow-ups.

The first large-file run found an incorrect child coordinate assumption:
C++ applies the enclosing row-group origin recursively to all segment identities,
including LIST/ARRAY children, while list endpoints remain local. Both reader
and writer were repaired, and the multi-row-group fixtures cover the regression.
A selected three-row release-producer file also passed Rust update/insert,
Rust reopen, and independent reopen by both pinned C++ CLIs.

This is an internal storage increment, not completion of the family assignment.
Nested WAL, native non-NULL defaults, dynamic VARIANT/TUPLE persistence, the
remaining string/validity decoders and adversarial/resource expansion remain open. No
acceptance performance measurements were taken. Kani is run by the integration
lead at combined checkpoints; the first combined checkpoint reported five of
five maintained harnesses passing, with bounded coverage rather than full parity.

For this checkpoint increment, ordinary check, nested tests (8, including one
explicit unsupported-codec test), existing compatibility tests (14), the focused
packed-string-statistics unit test and all-target clippy passed. Coverage reports
241 files, 2,088 functions, 201 interface methods and no missing attributes.
The final instrumentation check passed in 28.01 seconds with zero reported errors,
panics or open spans; temporary telemetry was deleted. An earlier overlapping
trace/check attempt returned nonzero despite a completed compiler invocation;
the stable retry is the executed passing instrumentation result.

## Remaining work (continuing assignment)

All six requested families remain in scope. UNION enum tags and broader member-coercion coverage; VARIANT dynamic semantics; TUPLE/unnamed rows; ordered list aggregation and remaining nested functions; slicing, lambdas and shape/coercion edge cases; formatting and mixed-family semantics need expansion. ARRAY/common-type, STRUCT field-union and composite overload ranking rules need broader reference coverage.

Native DuckDB nested WAL and non-NULL default codecs are not implemented yet. The private JSON round trip is not native compatibility evidence. Native default and ART encoding reject nested values explicitly, and native nested indexes must follow reference-supported behavior rather than a blanket assumption that every type is indexable. Further tests must include nested registry replacement, adversarial payload/resource cases, alternate execution/index compositions, mixed temporal/scalar families, independent C++ files, native rollback/recovery/reopen, upstream regressions and isolated faster-reference performance campaigns.
