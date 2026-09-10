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
cast throws despite being declared infallible). Rust returned NULL at that
checkpoint; the cast-context follow-up below repairs that observable mismatch.
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

The [cast-context follow-up](bit-reference-cast-context.json) records development
61/62 SQL, release 52/62 SQL, and all three native producer/mutation paths passing
on both pins with unchanged source fingerprints. The only remaining development
campaign mismatch is signed BITSTRING modifier grammar. XOR now uses the shared
integer-literal provenance hook: fitting bare literals narrow to the other
integral operand, while explicit INTEGER casts, CASE results and typed API
parameters retain their declared type. BIT string literals bind to BIT.

The selected BIT adapter now uses `CastBehavior::Try` to preserve development's
fatal empty-BLOB error without changing generic TRY_CAST recovery. Strict
empty-BLOB conversion still returns Conversion; invalid text and ordinary
narrowing still produce NULL under TRY_CAST. The extra component test checks
both evaluators/optimizers, nested LIST/STRUCT casts, prepared parameters,
batch rows, failed updates followed by rollback, and cancellation precedence.
This follows independently reproduced development behavior, including its
infallible-cast diagnostic; it does not claim that the C++ behavior is desirable.

Reference error cases compare categories only, with category-label case
normalized (`INTERNAL Error` versus `Internal Error`). Raw complete diagnostics
remain unchanged in the report. A Python harness test checks that normalization
does not modify message bodies or successful VARCHAR results, and that wrong
categories and unsupported results still fail. Exact diagnostic-string parity
is not claimed. Release's three empty-BLOB TRY cases remain Conversion errors,
not Internal errors, and stay recorded as disagreements.

The follow-up passes normal workspace/all-target check/clippy, BIT (7), casts
(11), numeric (27), nine selected Python oracle tests, coverage with no missing
instrumentation, and exhaustive tracing compilation with telemetry removed.
The merged baseline additionally passes nested (17). The lead recorded all six
maintained Kani harnesses passing on combined `b43faaa`; this later cast-context
change still belongs in the next combined run/investigation before declaring
the stage complete. No new performance campaign was run.

The remaining `bitstring_agg(value[, min, max])` requires retained aggregate
bind constants and, for the unary form, upstream-equivalent child statistics.
Deriving bounds from observed group rows would give different values and is not
an acceptable substitute. That shared bind/statistics interface remains under
coordination. Native string constant compression is still explicitly unsupported,
as for VARCHAR/BLOB. Signed type-modifier grammar remains shared lead work.

Full upstream mappings, boundary/error diagnostics, broader native compression,
mixed-family coverage and controlled faster-reference performance measurements
remain open. BIGNUM and core GEOMETRY remain scalar obligations; neither is
replaced by this BIT increment or classified away as an unavailable extension.

## Shared modifier integration and retained regression trial

The [expanded modifier trial](bit-reference-modifier-integration.json) retains
development 69/70 and release 55/70 selected SQL cases, with all three native
producer paths passing against both pins and unchanged source. It exposed a
new parser bug: accepting `BITSTRING(+1)` as an integer constant. Development
rejects that expression as nonconstant, while BIT's separate grammar accepts
and discards it. The failed report is not replaced by the repair.

The repair parses DuckDB custom modifiers as constant expressions, preserves
quoted strings, and handles grouping and directly negated numeric constants.
Unary plus, names, functions and arithmetic expressions remain parser errors;
wrong constant types and integer widths reach the binder. BIT and BIT VARYING
continue to discard their modifier expressions without looking up names or
executing functions. Parameter numbering outside discarded modifiers remains
lexical. Other dialects keep their original precision/modifier grammar.
Normal checks pass BIT (8), types (14), contracts (25) and all-target clippy.
The expanded repaired differential campaign is still pending at this commit.

The subsequent [repaired campaign](bit-reference-modifier-repaired.json), on
integrated source `b1cb75c`, records development 77/77 SQL and release 60/77.
All three native producer/mutation/reopen paths pass against each pin and the
source remained unchanged. The earlier 69/70 failure remains retained above.
The 17 release disagreements are not waived into release passes; development
is the correctness authority. This closes the selected modifier campaign, not
the remaining aggregate, compression, diagnostic or full BIT obligations.
