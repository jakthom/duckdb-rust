# Native typed Value metadata prerequisite

This increment implements C++ `Value::SerializeInternal` / `Value::Deserialize`
for supported scalar values and LIST, ARRAY, STRUCT, MAP, UNION, VARIANT and
TUPLE. It is not a parsed DEFAULT expression codec and does not enable catalog
defaults by itself. The integration owner retains that separate work.

The owned entry points in `src/storage/duckdb/value.rs` are:

```rust
read_typed(reader, actual_storage_version, selected_types, query)
    -> Result<(DataType, Value)>;
write_typed(output, declared_type, value, actual_storage_version,
            selected_types, query) -> Result<()>;
```

The integration owner has added an internal `ValueCodec` session with `new`,
`read_typed` and `write_typed` methods. One enclosing parsed-expression root must
use one session so all literal payloads/metadata share the existing 16-million
visit and 64-MiB budgets and selected binding cache. The convenience functions
above create a single-member session. A failed member poisons its session;
subsequent calls do not consume input or append output, and budget use is not
rolled back. Individual writes remain staged; the expression owner must also
stage the entire enclosing expression. This prevents multiplying the allowance
by the number of literal expression leaves once the parsed codec is connected.

Two new tests lower private test budgets to demonstrate aggregate byte/node
exhaustion across otherwise valid members, no partial append, no resumption,
mixed declared metadata and cancellation between members. All eleven codec
tests, check/clippy and coverage pass (377 files/3,657 functions/239 methods,
none missing). This is still an internal parsed-codec prerequisite; the next
substantial integrated checkpoint runs the maintained Kani suite.

Both module registrations (`duckdb::value` and `variant::value`) are initially
`cfg(test)` prerequisites so no unused production interface or publication path
is introduced before caller integration. Remove those two guards when wiring
the parsed literal reader/writer. Existing `catalog/constant.rs` and
`writer/constant.rs` are unchanged.

## Semantics and limits

Root metadata always retains the declared type, including SQLNULL and typed
NULL. No casts, storage-NULL normalization, SQL-name dispatch or builtin-registry
fallback occur. Bindings are selected from the explicit registry, cached for
this operation, and validated independently of the query's ambient registry.
Writer output is staged and appended only after all validation succeeds.
Selected binding/validation, interruption and resource failures are not
reclassified as data conversion errors.

Known child types are explicit for actual storage version 64 and inherited at
65–69. The reader also accepts explicit child metadata at newer versions and
checks it against its parent's declared child type. Nested metadata and NULL
children remain typed; MAP validity and UNION tags/inactive members are checked.
VARIANT uses the existing canonical four-child native representation and exact
scalar tags, including decimal widths and floating payload bits. It does not
store a second opaque byte representation in logical values.

The codec carries one 16,777,216-visit and 64 MiB variable-payload/name-byte budget
through recursive values, metadata and every VARIANT child. Temporary encoded
and decoded string representations are charged separately. Depth is limited to
64, individual serialized strings/blobs to 16 MiB, and each explicit type tree
to 4,096 nodes, in addition to the selected registry's metadata limits. These
are provisional bounded-codec limits, not DuckDB maximum-size parity or complete
allocator accounting. Collection counts are checked before allocation; loops
check cancellation. Canonical VARIANT conversions share remaining visits and
bytes instead of resetting them for siblings.

VARIANT publication metadata requires actual storage 68; TUPLE and empty STRUCT
writes require 69. The reader recognizes a legacy unnamed STRUCT as TUPLE under
development semantics, but the writer does not silently downgrade TUPLE identity
when asked to write version 64. TYPE values, GEOMETRY, extension/aliased logical
metadata, unbound type expressions and direct internal OBJECT publication remain
unsupported. There is no general native DEFAULT, performance or formal-proof
claim in this prerequisite.

TIMETZ constant metadata uses signed LEB128 because C++ dispatches its packed
value through physical INT64. The pre-existing `temporal::read_metadata` /
`write_metadata` unsigned path is deliberately not copied here. Column/WAL
fixed-width bytes are a different wire protocol. Decimal metadata likewise
does not interpret a non-NULL signed-minimum coefficient as a column NULL
sentinel. Timestamp scalar validation follows the shared temporal constructor;
the temporal owner's separate signed-minimum-domain repair remains independent.

## Independent evidence

`scripts/native_value_reference.cpp` links existing libraries from the unchanged
pinned development and release checkouts. It does not rebuild either reference.
Library, executable, helper and source identities are recorded. The final helper
also checks the loaded library's own version/source ID, not just the CLI pin.

`test/data/native-value-metadata.json` retains 81 C++-produced and independently
C++-reread raw Value fixtures at versions 64, 65, 68 and development 69. The Rust
unit test reads all 81, preserves declared metadata, writes them and checks a
second exact Rust serialization. Coverage includes all seven nested families,
legacy/inherited child types, nested NULLs, decimal/integer widths, raw floating
values, TIMETZ, other temporal types, BLOB escapes, BIT, BIGNUM negative zero,
ENUM and UUID, UNION active NULLs and empty records.

The initial raw-byte campaign passed
149/157 reader comparisons. Eight VARIANT cases differed only in descriptor
allocation order or unused payload bytes under NULL leaves. C++ can retain
those bytes when reserializing, so full physical-byte identity is not a valid
canonical-content oracle. No production codec was weakened to hide this result.

The subsequent exact-content campaign
passes 157/157 reader cases. Both C++ readers decode the original and Rust output.
For VARIANT, C++ `UnifiedVariantVectorData` and `VariantUtils` traverse the
original tags and child references; the helper records ordered exact-name keys,
child NULLs, and C++-serialized leaf types and raw scalar bits. Other nested
values retain their full typed recursive content. This excludes only allocation
indices and unused VARIANT bytes, not numeric widths, tags, floating bits or
object ordering. It does not use SQL comparison, grouping keys, casts or value
display as an oracle.

The post-instrumentation campaign
and loaded-library identity campaign
repeat the unchanged 157/157 cases on the delivered Rust source; both pass.

Delivery `41957c6` passes ordinary `cargo check --workspace --all-targets`,
library tests 76/76 (nine new codec tests), checkpointing 18/18, compatibility
19/19, contracts 49/49, nested 42/42, numeric 46/46 and temporal 31/31.
`cargo clippy --workspace --all-targets -- -D warnings` passes. `cargo dev coverage`
reports 356 files, 3,352 functions, 228 interface methods and no missing
instrumentation. `cargo dev trace check --workspace --all-targets` completes in
62.35 seconds with no error returns, panics or open spans; temporary telemetry is
deleted. Instrumented duration is not a performance measurement.

Maintained Kani is pending the integration owner's next substantial combined
checkpoint; this new codec has no claimed formal coverage. The preceding root
checkpoint's proofs do not verify this pending codec. No performance benchmark
was run for this increment.
