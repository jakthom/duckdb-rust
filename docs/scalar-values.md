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
| ENUM | Ordered label metadata and values, anonymous constructors/casts/functions, cross-dictionary behavior, then named catalog identity/dependencies and native checkpoint/WAL support. Same-dictionary ordering follows ordinal; different dictionaries compare through VARCHAR. Labels are byte/case sensitive: `ENUM('a','A')` and empty labels are valid. |
| BIT / bitstring | Public storable type: shape/padding, operators/functions, casts, ordering/keys, vectors, relational execution, indexes and persistence. |
| BIGNUM / varint | Public storable arbitrary-width integer: representation, arithmetic/conversions, comparisons/keys, aggregates and persistent values. |
| GEOMETRY | Public storable core type in this development build, not dismissible as an unavailable extension. WKB/CRS metadata, valid geometry semantics, applicable operations/casts and native encoding remain. Larger spatial-extension obligations are separately inventoried. |
| TYPE / TUPLE | Investigate expression/type-constructor semantics. Development accepts `typeof(NULL::TYPE)` but rejects CREATE TABLE with a TYPE column. Bare TUPLE syntax is rejected; do not infer an ordinary storable scalar from the catalog entry alone. |
| Shared engine integration | Literal-sensitive and combination coercion, constant-NULL return metadata, unnumbered prepared parameters, standalone index DDL, named types, mixed temporal/nested values, and development-native catalog/version compatibility are owned with the integration lead. |

Unsupported operations, reference disagreements, unported upstream files and
unmeasured workloads remain open. The successful first BLOB/UUID path is an
internal increment in the full assignment, not its stopping criterion.
