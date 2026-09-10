# BIT value-and-expression work in progress

Correctness follows pinned development `99063af2bd`; release `d8cdaa33fd`
remains the second compatibility/performance reference. This is an internal
increment in the sustained value-and-expression assignment, not BIT parity.

## Packed value and storage prerequisite

`DataType::Bit` and `Value::Bit(Arc<BitString>)` retain the logical bit length
and packed MSB-first bytes with zero-filled trailing padding. The representation
is provisional and keeps the existing `Value <= 32` / `DataType <= 16` footprint
checks. Selected type/cast adapters expose checked shape validation, logical
prefix ordering, length-sensitive equality keys, numeric physical-bit casts,
VARCHAR/hex parsing, BLOB conversion and flat/constant/dictionary vectors.

Native type 36 has physical VARCHAR storage. Its adapter converts to/from the
reference's padding byte and leading one-filled padding; that format does not
leak into logical comparison or BLOB conversion. Catalog/defaults, primitive
string decoders, column/WAL payloads, overflow storage and native ART keys carry
BIT values. Empty uncompressed NULL placeholders remain distinguishable from
the real zero-length BIT native payload `[0]`, and strict validity rejects a
placeholder marked non-NULL.

Three component tests exercise lengths 1 through 257, exact native padding,
numeric extrema and floating bit patterns, malformed values, cancellation,
shifts/extension against independently constructed text, and selected vectors.
Sixteen snapshot/index/evaluator/join compositions exercise prepared values,
defaults/keys, mixed decimal/UUID schemas, nested BIT children, comparisons,
joins, grouping, sorting, sets, windows, mutations, rollback and reopen. Native
WAL/checkpoint tests include a 70,005-bit overflow value and committed NULLs.

The isolated prerequisite passes normal `cargo check`, its three tests and
workspace/all-target clippy. Coverage reports 255 Rust files, 2,205 functions,
203 interface methods and no missing instrumentation; exhaustive tracing
compilation passes and removes its temporary telemetry. Earlier combined tests
also passed BLOB/UUID (4), ENUM (3), and compression (15). The lead owns the
maintained Kani run/investigation on the integrated code before declaring this
substantial stage complete. An earlier tracing command overlapped source
isolation and returned nonzero; the stable isolated rerun is the reported pass.

## Continuing work and limits

SQL bitwise operators, scalar functions and aggregates are a separate active
increment. Independent C++ producer/checkpoint/WAL compatibility campaigns have
not yet run for BIT: Rust's own native round trip is not external compatibility
evidence. Native string constant compression is still explicitly unsupported,
as for the existing VARCHAR/BLOB path. Integer-literal-sensitive overload
selection belongs to the shared lead binder work, including the distinction
between a bare literal and an explicitly cast or CASE-produced INTEGER.

Full upstream mappings, boundary/error diagnostics, broader native compression,
mixed-family coverage and controlled faster-reference performance measurements
remain open. BIGNUM and core GEOMETRY remain scalar obligations; neither is
replaced by this BIT increment or classified away as an unavailable extension.
