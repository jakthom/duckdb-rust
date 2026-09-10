# BLOB Base64 increment

This is a continuing scalar-family increment, not full binary or database parity.
Development `99063af2bd` governs correctness; release `d8cdaa33fd` remains the
second reference. The implementation adds the catalog's `base64`/`to_base64`
BLOB-to-VARCHAR overload and `from_base64` VARCHAR-to-BLOB overload.

Argument conversion stays in the selected cast registry. A bare SQL string
literal may bind to the BLOB input through its retained explicit cast, but a
VARCHAR column or typed parameter does not acquire that privilege. ENUM labels
can use the ordinary implicit ENUM-to-VARCHAR conversion for `from_base64`;
ENUM-to-BLOB remains rejected. NULL propagation, lazy CASE branches, argument
failures under outer TRY_CAST, invalid output validation and cancellation retain
their ordinary engine contracts. No scalar-name dispatch was added to binding,
execution, indexes or storage.

The primary implementations are `duckdb/src/common/types/blob.cpp` and
`duckdb/extension/core_functions/scalar/blob/base64.cpp`; the reference SQL file
is `duckdb/test/sql/function/blob/base64.test`. Encoding uses the standard
alphabet and padding. Decoding follows the actual pinned behavior:

- Input byte length must be a multiple of four. This check precedes character
  validation, including for UTF-8 input.
- Whitespace, URL-safe alphabet characters and padding outside the last two
  positions of the final quartet are conversion errors. Diagnostics retain the
  first invalid byte's unsigned value and byte position.
- The reference accepts nonzero unused bits, such as `AR==` producing byte 1.
  It also accepts `AA=B`: an equals sign in the penultimate position truncates
  output to one byte even if the final character is not an equals sign. The
  remaining character is still validated; `AA=!` fails at position 3.

These distinctions were inspected in source and independently probed through
the pinned development CLI. The Rust implementation does not rely on a decoder
library's potentially stricter padding policy. Output sizing and allocation are
checked, with cooperative cancellation during traversal; this does not supply
the remaining global query-memory accounting subsystem.

Three new component tests cover fixed independently known encodings, a separate
bit-stream encoding oracle, byte-domain/padding boundaries, scalar vector views,
selected input-cast replacement, both evaluators and optimizers, typed prepared
parameters, NULLs and failed branches. Relational cases exercise binary primary
keys, defaults, joins, groups, sorting, window results, atomic failed updates,
rollback, nested BLOB children, private/native checkpoints and reopen. A native
WAL case retains a 300,007-byte arbitrary-byte payload and its encoded VARCHAR
through update, rollback, recovery and checkpoint.

Ordinary workspace/all-target check and clippy pass. Component suites pass
binary scalars (8), numeric (27), casts (11) and floating (7). Coverage reports
290 files, 2,605 functions and 208 interface methods with no missing attributes.
An initial test attempted to reopen a still-owned Database path and correctly
received a transaction error; the test now releases that handle before reopen.
This was a test-lifetime error, not a relaxed database ownership contract.

The expanded `scripts/binary_scalar_reference.py` retains all preceding
BLOB/UUID queries and adds Base64 cases, padding observations, native defaults,
cross-engine mutation/reopen and indexed decoded-key lookup. The initial
[paired campaign](binary-scalar-reference-base64.json) built uninstrumented
release binaries in 1m39s and retained unchanged source and both pinned binary
identities. All 111 added Base64 cases match both pins, including 192 padding
inputs within six batched queries. Development matches 136/136 SQL cases;
release matches 134/136, retaining its two earlier UUID/UHUGEINT and
constant-NULL concatenation metadata disagreements.

Native Rust checkpoint and WAL producer paths pass on both pins, with exact
post-mutation/reopen values and indexed decoded-key lookup. The C++ producer
path fails on both: the stored `DEFAULT from_base64('AP8=')` is a parsed FUNCTION
expression (class 9, kind 140), which the constant-only native default reader
rejects. Thus native interchange is **2/3**, not complete, and the campaign exits
1. The [wire inventory](native-function-default-inventory.md) identifies the
shared retained-expression/binding dependency. No function is executed through
hidden builtins during decoding, and the failing report is not overwritten.

The full workspace/all-target test suite passes, with the two pre-existing
external-CLI analytics tests ignored. Exhaustive tracing compilation passes in
43 seconds. The unchanged-source focused Base64 trace subsequently passes after
a 2m47s instrumented build: 56,419 completed operations, zero error returns,
panics or open spans, and exact output `AP8=`, `00`, `[A, NULL]`. Temporary
telemetry was deleted after both runs; trace timings are not performance data.

The initial [unchanged upstream file run](upstream-base64.json) reaches 8/17
records, then fails at line 47: an empty BLOB renders as an empty transport
string instead of SQLLogicTest's `(empty)` sentinel. The engine value agrees
with both references. The reference's `test/sqlite/result_helper.cpp:403–431`
applies empty-text and embedded-NUL rules after conversion for every type, not
only VARCHAR. The test transport now applies those two rules after its existing
value renderer, with focused empty BLOB/VARCHAR, NULL, byte-zero, nested NULL and
public-query regression coverage. This does not alter upstream assertions,
database values or the general SQL/client renderer; complete retained selected
text casting in the test transport remains separate work. The failed report is
retained. The [unchanged-file rerun](upstream-base64-rendering.json) passes all
17/17 records after a 27-second uninstrumented worker build; the focused worker
regression and tracing-enabled clippy also pass. The wrapper still exits 1
because this one selected file is not complete upstream-suite parity. The
maintained integrated Kani
run/investigation remains pending for this increment. No performance acceptance
measurement is claimed.

The independent C++ session transport had the same VARCHAR-only sentinel rule.
Its rendered-text empty/NUL handling now follows the pinned SQLLogicTest rule as
well. Both pinned libraries were independently linked into worktree-local
wrappers and checked against the Rust public-query worker: empty BLOB/VARCHAR,
NULL BLOB, byte-zero BLOB, a list containing NULL, and empty/Base64 expressions
retain matching exact rows and column types (two queries per pin). The original
database values are unchanged; no source assertion or paired-result comparator
was relaxed. This wrapper correction does not replace future verification of
fully selected text casts at either test transport boundary.
