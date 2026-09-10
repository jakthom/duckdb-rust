# Scalar values: integrated work in progress

This is a progress record for the sustained value-and-expression milestone, not
a declaration of scalar or database parity. Correctness follows development
`99063af2bd7092aff02e14184a20e24699d34d71`; release
`d8cdaa33fda8df955cc76ef58a280f68f4cd43fa` remains an independent compatibility and
performance reference. The pre-milestone 34-workload performance gate passed at
`f5f0fae`; it has not yet been rerun for the combined family implementation.

## First BLOB and UUID path

BLOB owns binary bytes, including zeros and invalid UTF-8. UUID owns its complete
128-bit network/text-order value. Both have explicit logical types, selected type
and cast adapters, owned flat/constant/dictionary vectors, canonical equality
keys, and SQL comparisons. The earlier 32-byte `Value` and 16-byte `DataType`
footprint checks still pass on the worker's 64-bit target.

The current family path includes string parsing/formatting, BLOB/UUID casts,
development UUID/UHUGEINT casts, BLOB concatenation, `encode`, `decode`,
`octet_length`, `hex`/`to_hex`, and `unhex`/`from_hex`. UUID integer storage flips
bit 127 only at the native codec boundary; unsigned in-memory ordering therefore
matches canonical text ordering. BLOB vector/WAL payloads are raw bytes, but
serialized scalar defaults use escaped text, as C++ `Value::SerializeInternal`
requires. Confusing those two encodings was detected by the independent native
campaign and repaired.

Four component tests exercise conversion failures, all 256 byte values, UUID
extrema, vector encodings and slices, selected key/comparison contracts, and 16
combinations of snapshot format, index, evaluator and join adapters. End-to-end
cases include typed numbered prepared parameters, decimal-containing schemas,
defaults, PRIMARY KEY/UNIQUE enforcement, joins, membership, sets, grouping,
ordering, windows, mutations, rollback and reopen. A separate WAL test checks
snapshot visibility and 300 KB non-UTF-8 overflow values before and after
checkpointing. These tests do not establish standalone CREATE INDEX support.

Normal `cargo check`, the four binary-scalar tests, existing numeric (24), types
(10), logging (7), and workspace/all-target clippy pass after the default-codec
repair. The earlier full workspace run passed with two ignored external CLI
analytics tests; the integrated lead owns the final combined run. Coverage and
trace compatibility are checked for the changed source. Kani is scheduled on the
combined integration branch before the substantial stage is declared complete;
the worker does not count an unexecuted checkpoint as passing.

`scripts/binary_scalar_reference.py` retains typed results, exact binary/source
identities and failures against both pinned references. The initial report is
`binary-scalar-reference-initial.json`; it preserves the default-codec failure
and constant-NULL metadata discrepancy. Follow-up evidence does not overwrite
that report. No timing campaign has run concurrently with worker builds.

The follow-up `binary-scalar-reference-defaults.json` records 24/25 development
SQL cases and 23/25 release cases passing. The remaining development mismatch is
constant-NULL concatenation's result type (`"NULL"` in development, `INTEGER` in
release, currently `BLOB` in Rust). Release additionally lacks development's
UUID/UHUGEINT casts. All three release native producer paths pass after the
default-codec repair. Development's three native paths still fail at the shared
version-999 / catalog-index reconstruction gaps that the integration lead is
addressing. Both campaigns verify unchanged source identities and retain all
failures; neither report claims full compatibility.

Final worker instrumentation checks report 218 Rust files, 1,890 functions and
195 interface methods with no missing attributes; exhaustive trace compilation
passes and its temporary telemetry is deleted. The post-fix normal focused test
and clippy pass includes the additional rejection of BLOB/BINARY/VARBINARY type
parameters, matching both references.

## Remaining scalar family scope

This inventory comes from the pinned development `duckdb_types()` catalog and
direct SQL probes, not from declarations that happen to compile in Rust.

| Family / integration | Remaining behavior |
| --- | --- |
| Numeric | Expand unsigned/decimal operators, casts, common-type/overload semantics, scalar/aggregate catalog, native configuration/compression coverage, and performance workloads beyond the existing foundation. |
| BLOB / UUID | Base64 and remaining binary/UUID functions, full upstream mapping, literal-sensitive coercion, broader compression/invalid-file cases, and isolated performance comparisons. |
| ENUM | Anonymous SQL, selected values/casts/functions/keys, nested child values and native checkpoint/WAL are implemented in the worker increment below. Named catalog identity/dependencies and aliases remain lead-owned; cross-scalar combination coercion, empty-dictionary grammar diagnostics, batch-dependent range behavior, expanded upstream mapping and performance remain open. |
| BIT / bitstring | Public storable type: shape/padding, operators/functions, casts, ordering/keys, vectors, relational execution, indexes and persistence. |
| BIGNUM / varint | Public storable arbitrary-width integer: representation, arithmetic/conversions, comparisons/keys, aggregates and persistent values. |
| GEOMETRY | Public storable core type in this development build, not dismissible as an unavailable extension. WKB/CRS metadata, valid geometry semantics, applicable operations/casts and native encoding remain. Larger spatial-extension obligations are separately inventoried. |
| TYPE / TUPLE | Development accepts `typeof(NULL::TYPE)` but rejects CREATE TABLE with a TYPE column; expression/type-constructor semantics remain open. TUPLE is a real nested family in `test_all_types()` and `src/common/types.cpp`, including unnamed-STRUCT compatibility on old storage. The nested worker owns TUPLE and its row/tuple constructor investigation; it is not silently omitted because bare TUPLE syntax is rejected. |
| Shared engine integration | Literal-sensitive and combination coercion, constant-NULL return metadata, unnumbered prepared parameters, standalone index DDL, named types, mixed temporal/nested values, and development-native catalog/version compatibility are owned with the integration lead. |

Unsupported operations, reference disagreements, unported upstream files and
unmeasured workloads remain open. The successful first BLOB/UUID path is an
internal increment in the full assignment, not its stopping criterion.

## Ordered ENUM increment

The provisional representation is `DataType::Enum(Arc<EnumType>)` and
`Value::Enum(Arc<EnumValue>)`: ordered label metadata plus a checked 32-bit
ordinal. Dictionary equality is ordered label equality, not a catalog name.
Labels are case-sensitive and may be empty; duplicates are invalid. Empty
dictionary metadata is valid for upstream query-created types, although bare
anonymous `ENUM()` is rejected. Value/type footprint checks remain unchanged.

Same-dictionary comparisons and keys use ordinals. Different dictionaries use
selected VARCHAR casts for comparison. ENUM-to-numeric/temporal/binary casts
retain the selected VARCHAR-to-target cast, so values follow their labels, not
numeric ordinals. Numeric and other unsupported sources cannot become ENUM
labels through implicit stringification: explicit conversion fails and
`TRY_CAST` returns NULL, matching the pinned references. String functions use
the selected implicit ENUM-to-VARCHAR conversion. Enum-first/last/range retain
declared metadata while still evaluating their arguments and preserving errors.

The native family codec writes type id 104 and ordered enum type-info id 6.
Column, constant/default, statistics, ART keys and WAL paths retain unsigned
ordinals with one-, two- or four-byte physical widths; all-ones physical NULLs
do not become valid dictionary values. Named/aliased native type metadata is
still explicitly unsupported until the lead integrates named catalog semantics.

Three component tests cover scalar functions and selected vector casts, enums
inside lists/structs, mixed UUID/BLOB/decimal schemas, prepared parameters,
PRIMARY KEY enforcement, same- and cross-dictionary joins, grouping, sorting,
windows, set operations, successful and rejected mutations, rollback and reopen.
The relational test runs all 16 snapshot/index/evaluator/join combinations.
The WAL/checkpoint test covers dictionary sizes 3, 255, 256 and 65,536, including
the native width transitions, defaults and NULLs.

The independent initial campaign is `enum-reference-initial.json`: 24/28 SQL
cases pass on both references; native producer paths pass 6/6 on release and
4/6 on development. Development failures are a shared metadata-reader gap in
the older worker base: field 204 appears where the reader expects field 200.
The nested worker independently identified development's new StringStats
204..207 layout, relevant to the fixture's BLOB column; the next combined
campaign must verify that diagnosis and repair. This must not be conflated with
the lead's separately repaired per-column ownership metadata. The report
also preserves wrong diagnostic categories for unsupported casts and empty
anonymous ENUM grammar, missing cross-scalar combination coercion, and the
upstream batch-first-row range behavior. Follow-up reports never overwrite this
evidence. No full ENUM parity or timing acceptance is inferred from these tests.

The follow-up `enum-reference-casts.json` verifies the unsupported-source cast
repair and three TRY_CAST cases: 26/29 SQL cases pass on each reference. Native
results remain release 6/6 and development 4/6 on the same older integrated
base. Both source fingerprints are unchanged through their campaigns. The two
production CLI/worker builds use `--release --no-default-features` and pass.
The full workspace suite passed before the cast diagnostic repair, with two
pre-existing ignored external-CLI analytics tests. Post-repair checks pass for
ENUM (3), numeric (25), BLOB/UUID (4), types (12), normal check and all-target
workspace clippy. Coverage reports 235 files, 2,036 functions and 200 interface
methods with no missing instrumentation; exhaustive trace compilation passes
and removes its temporary telemetry. These checks do not replace integrated
Kani or controlled performance measurements.

The pinned development `extension/core_functions/scalar/enum/enum_functions.cpp`
reads only row zero for `enum_range_boundary` and broadcasts that result. An
independent two-row query returns `[z, a]` twice while ordinary per-row Rust
execution returns `[z, a]`, `[a]`. This observable discrepancy is retained as
open: matching it requires a deliberate batch-semantics decision, not an
unreported result normalization. Empty `ENUM()` reaches a sqlparser parse error
before the Rust binder, while C++ reports a binder error; that diagnostic gap
also remains explicit.

Unary numeric `trunc` was delivered as a separate shared dependency for temporal
interval lowering. It preserves complete signed/unsigned domains and widths,
divides decimal coefficients toward zero with result `DECIMAL(width,0)`, and
retains FLOAT/DOUBLE truncation. Bare NULL resolves to BIGINT as in development.
The numeric component suite now has 25 passing tests for that dependency.

Kani remains an integrated checkpoint owned by the lead. This worker increment
is not declared a completed substantial stage before the maintained suite is
run, investigated and reported there. No benchmark campaign has run alongside
concurrent builds; label-lookup/metadata costs and isolated scalar workloads
remain performance obligations, with the faster pinned reference as target.
