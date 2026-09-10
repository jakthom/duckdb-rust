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

## SQL operators/functions and independent native evidence

The next slice registers selected `&`, `|`, `~`, `<<`, `>>` and `xor` kernels
for BIT and all ten fixed-width integral types. Numeric operations preserve the
selected physical width, including 128-bit extrema; left-shift negatives and
overflow error, while out-of-range right-shift counts return zero. BIT shifts
retain logical length. Functions include `bitstring`, `bit_length`, `bit_count`,
`get_bit`, `set_bit`, `bit_position`, `bitstring_byte_comparable`, BIT length
aliases/octet length, and `bit_and`/`bit_or`/`bit_xor` aggregates with ordinary
grouped and window execution. Development's surprising HUGEINT popcount result
(`bit_count(-1::HUGEINT) = -128`, returned as TINYINT) is checked explicitly.

The initial `bit-reference-initial.json` records development 50/55 SQL and
release 45/55 SQL, with native C++-producer/Rust-checkpoint/Rust-WAL paths 3/3
on each pin. Investigation found an actual empty-BLOB cast bug, now repaired:
ordinary BLOB-to-BIT rejects empty input. The campaign also mistakenly marked
unparenthesized negative BIT casts and unsupported BIT-to-ENUM casts as success
queries. The follow-up keeps those expressions as separate expected-error cases
and adds the intended parenthesized numeric casts; no upstream assertion was
changed. Its native mutation changes a bit instead of redundantly setting zero.

`bit-reference-empty-blob.json` records development 56/59 SQL, release 50/59,
and native paths 3/3 on each pin. Both reports retain source fingerprints,
binary/library identities, typed results and failures. The production builds
use `--release --no-default-features`; each took about 1m24s. Development still
differs on signed type-modifier parsing, integer-literal-sensitive XOR binding,
and its INTERNAL Error for `TRY_CAST(''::BLOB AS BIT)` (the selected reference
cast throws despite being declared infallible). Rust currently returns NULL
for that TRY_CAST; this is an open observable mismatch, not a passing result.
Release additionally differs on empty VARCHAR casts, TRY_CAST narrowing errors,
the byte-comparable function, and logical BIT sorting/minimum behavior.

Seven retained native fixtures supplement the campaign. Both pins produce a
mixed BIT/list/struct/decimal schema with defaults, a primary key and 70,005-bit
overflow data. Release fixtures actually use Dictionary and FSST; development
actually uses DICT_FSST with EMPTY_VALIDITY. Development's forced legacy
Dictionary/FSST requests fall back to Uncompressed because those encoders are
disabled after storage V1_2_0; their manifests record that fact and they are not
counted as exercising legacy compression. The generator initially rejected that
fallback until the source policy and actual compression were checked.

These fixtures exposed a legacy-FSST NULL-placeholder bug after the report
campaign: decoded empty BIT payloads need an external validity check, just like
uncompressed placeholders. The narrow repair retains strict validity and does
not accept an empty non-NULL DICT_FSST dictionary entry. All seven fixtures now
check exact rows, nested NULL/container distinctions, read-only byte stability,
rollback, real updates/deletes and checkpoint reopen in the BIT component suite.
The report fingerprints precede this FSST repair; the independent fixture tests
are the direct validation evidence for it.

Current normal checks pass BIT (6 tests), compression (15), numeric (25),
operators (8) and workspace/all-target clippy. Coverage finds no missing
instrumentation across 257 files, 2,224 functions and 203 interface methods.
Exhaustive tracing compilation passes and removes its temporary telemetry.
The integration lead still owns the maintained Kani checkpoint and controlled
performance comparisons before declaring the substantial stage complete.

## Continuing work and limits

The remaining `bitstring_agg(value[, min, max])` requires retained aggregate
bind constants and, for the unary form, upstream-equivalent child statistics.
Deriving bounds from observed group rows would give different values and is not
an acceptable substitute. That shared bind/statistics interface remains under
coordination. Native string constant compression is still explicitly unsupported,
as for VARCHAR/BLOB. Integer-literal-sensitive overload selection and signed
type-modifier grammar belong to the shared lead work, including the distinction
between a bare literal and an explicitly cast or CASE-produced INTEGER.

Full upstream mappings, boundary/error diagnostics, broader native compression,
mixed-family coverage and controlled faster-reference performance measurements
remain open. BIGNUM and core GEOMETRY remain scalar obligations; neither is
replaced by this BIT increment or classified away as an unavailable extension.
