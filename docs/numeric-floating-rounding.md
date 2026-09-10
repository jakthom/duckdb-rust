# Floating-to-integral cast repair

This is an internal correctness increment in the sustained value-and-expression
milestone, not numeric parity. Pinned development `99063af2bd` is authoritative;
release `d8cdaa33fd` remains an independently observed compatibility reference.

FLOAT and DOUBLE casts now round halfway values to the nearest even integer
before checking the destination range. This applies to all ten signed/unsigned
integer widths and both selected primitive/exact-numeric cast implementations.
For example, DOUBLE `60.5` becomes INTEGER `60`, `61.5` becomes `62`, and `-0.5`
becomes unsigned zero. Exact floating `-2^127` remains rejected by HUGEINT, as in
the pinned reference; a signed integer HUGEINT minimum remains a valid value.
DECIMAL conversions and the SQL `round` function keep their separately observed
half-away-from-zero behavior. Floating-to-signed overflow messages now retain
the destination-range diagnostic used by interval unit lowering.

The primary reference implementation uses `nearbyint` before its range check in
`duckdb/src/include/duckdb/common/operator/numeric_cast.hpp`; HUGEINT's additional
floating boundary is in `duckdb/src/common/types/hugeint.cpp`. Independent SQL
observations validate those distinctions rather than treating all numeric casts
as one rounding policy.

The [retained campaign](numeric-floating-rounding-reference.json) records exact
typed results, complete diagnostics, source fingerprints and binary/library
identities. Development matches all 62 SQL cases. Release matches 34/62, with all
28 mismatches identified as release/development disagreements: the 18 previously
recorded numeric differences and ten new floating-rounding/range-check cases.
Both pins complete all three C++-producer/Rust-checkpoint/Rust-WAL scenarios,
including cross-engine mutations, defaults, decimal keys and reopening. The
script returns nonzero because its strict both-reference aggregate remains false;
the development-specific outcome is true and source fingerprints are unchanged.
No failed release outcome has been removed or called a pass.

Component tests use an independent floor/fraction oracle across both floating
types, ten integral widths, scalar/flat/dictionary/constant vectors, alternate
cast registration, ties, finite boundary neighbors, NaN and infinities. SQL
checks cover both evaluators and optimizers, prepared floating parameters,
window sums, rounded primary-key collisions, failed updates, rollback, WAL,
checkpoint and read-only reopen. The focused interval overflow reproducer also
checks the required diagnostic phrase. The existing persistence campaign is
broader numeric compatibility evidence; the new component test is the direct
rounding-specific mutation/reopen evidence.

Normal workspace/all-target check and clippy pass, as do numeric (27), temporal
(12), casts (8) and floating (3) component tests. Coverage reports 269 Rust files,
2,366 functions, 206 interface methods and no missing instrumentation.
Exhaustive tracing compilation passes and deletes its temporary telemetry.
The integration lead owns the maintained Kani run/investigation on the combined
code before declaring this substantial stage complete. No new performance
acceptance campaign was run for this slice; controlled faster-reference timing,
broader numeric functions and remaining scalar families stay open.
