# Numeric port progress

Status: **in progress, not accepted as complete**. This records the unsigned and
decimal foundation committed in `760fd9a`, after incorporating Kani commits
through `35f2d9c`. The subsequent [binding follow-up](binding-regressions.md)
fixes the two regressions identified below and refreshes the full SQL report.
The [numeric batch follow-up](numeric-batches.md) records subsequent performance
fixes, all retained failed trials, a passing 34-workload timing matrix and fresh
SQL/native evidence. The full upstream refresh loses no previously passing file.
Development governs correctness disagreements; each comparable workload must
match or beat the faster pinned C++ reference. See the
[acceptance specification](../specs/testing/parity.md).

## Implemented foundation

- UTINYINT, USMALLINT, UINTEGER, UBIGINT and UHUGEINT, including the full unsigned
  128-bit value domain; DECIMAL/NUMERIC precision 1–38 and scale 0–precision.
- Fixed-point literals, exact string conversion and rescaling, checked casts,
  numeric coercion, decimal arithmetic, unsigned arithmetic and aggregate types.
  `round()` has unsigned argument coercion; `typeof()` retains declared metadata
  without executing its argument.
- Type, cast and operator selection stays behind registries. Parameterized cast
  families and operator specialization avoid registering every decimal pair.
  Scalar adapters request their argument types through a common binding method;
  the binder inserts selected casts and validates arity and result metadata.
- Native-width and independent decimal-digit type adapters share ordering,
  equality-key, ownership, validation and cancellation tests. Consumers use the
  selected adapter. The later batch work adds a separate numeric equality
  coefficient capability, which does not advertise signed SQL ordering.
- Numeric values, defaults, constraints and indexes round-trip through JSON and
  native checkpoint adapters. Native numeric WAL encoding and ART keys support
  these types. Foreign index options are parsed as serialized key/value pairs;
  physical indexes are reconstructed from validated logical rows.

At the foundation checkpoint, six numeric component tests cover unsigned
boundaries, every valid decimal
precision/scale pair with representative boundary coefficients, exact rounding,
metadata, aggregates, joins, sets, windows, both type adapters, both index
adapters, both snapshot formats, rollback and reopen. This is not exhaustive
numeric SQL, cast, arithmetic or persistence coverage.

## Independent C++ evidence

The [foundation reference report](numeric-reference.json) retains SQL, typed rows,
complete errors, source/binary identities and outcomes for both references:

| Scope | Development | Release v1.5.5 |
| --- | --- | --- |
| Selected SQL cases | 38/38 match | 20/38 match; 18 documented reference divergences |
| Bidirectional numeric file scenarios | 0/3 complete | 3/3 complete |

The 18 divergences cover decimal addition width, unsigned radix-string parsing,
unsigned negation and division/remainder by zero. Rust follows development.
Error cases compare their declared category, not full diagnostic text; complete
messages remain in the report. Shared scalar values and metadata compare exactly,
with only equivalent floating text such as `0` and `0.0` normalized.

The file scenarios use C++-produced files, Rust checkpoints and Rust WAL, then
read/mutate in both engines and checkpoint again. Release now completes all
three scenarios. Development-produced files are rejected at storage version
999; development reports an internal catalog error opening the Rust checkpoint
and replaying its WAL. These are unresolved compatibility failures, not waived
by development's precedence. Broader codec, file, recovery and storage-version
coverage remains open.

The [initial failed run](numeric-reference-initial.json) and
[coercion follow-up](numeric-reference-coercion.json) are retained. They exposed
the unsigned `round()` type mismatch and index-options decoding failure fixed
in the current source. No C++ library is linked into the Rust engine.

## Refreshed upstream report

The [full SQL report](upstream-parity-numeric.json) and its
[journal](upstream-parity-numeric.jsonl) retain all 5,638 source-file outcomes:
305 passed, 1,887 failed, 3,435 unsupported, eight timed out and three incomplete.
There are 16,968 passed SQL records, including prefixes of failed files. The
preceding report had 197 passing files; the new net increase is 108, with 110
newly passing and two formerly passing files now failing. This spans the
relational work as well as numeric changes, so it is not a numeric-only delta.

Two regressions were visible at this checkpoint: numeric common-type inference
for INSERT VALUES
changes a VARCHAR zero to a scaled decimal string in `rigger/test_536.test`, and
`cte_schema.test` reports an ambiguous table reference. Both are corrected in the
[binding follow-up](binding-regressions.md), without changing upstream assertions.
Additional numeric gaps
include large literals/coercion, scalar functions, full diagnostic wording,
division-by-zero configuration and imported SQLLogicTest numeric output rules.
Neither failures nor unsupported harness controls are counted as passes.

## Performance remains open

The [workloads](../benchmark/numeric_workloads.json) use 50,000 rows, one thread,
three warmups and nine alternating paired samples. The decimal currency cases
explicitly return integer cents so the existing independent integer checksum
can validate them; the conversion is part of both measured queries. Setup and
initial preparation are untimed. High-domain 128-bit output, other scalar/API
paths, memory, I/O, durability and concurrency remain unmeasured here.

Both the [release](numeric-performance-release.json) and
[development](numeric-performance-development.json) campaigns validate every
result, but seven of eight new workloads fail the
[faster-reference gate](numeric-performance-fastest.json). Only unsigned scan
passes. Unsigned filtering and SUM, decimal filtering and SUM, grouped currency
totals, decimal equality joins and partitioned decimal windows need regression
work. Both Rust sample sets are retained and checked against the smaller C++
median for each workload. Earlier successful workloads do not compensate for
these failures; their full current regression matrix also remains to be rerun.

## Local checks and Kani

- `cargo check`, `cargo test --workspace --no-fail-fast` and
  `cargo clippy --workspace --all-targets -- -D warnings` pass. The workspace has
  243 listed tests: 241 execute successfully and two external-CLI analytics tests
  remain ignored in this run.
- All 34 Python harness tests pass, including deliberately wrong results,
  changed benchmark identities, missing cases and faster-reference selection.
- `cargo fmt --all --check` and `cargo dev coverage` pass with no missing
  instrumentation. `cargo dev trace check --workspace --all-targets` passes and
  removes its temporary telemetry.
- `python3 scripts/verify_kani.py` with pinned Kani 0.67.0 verifies all three
  maintained harnesses, with zero failures. Those proofs cover packed sizes and
  window bounds, **not the new numeric representation or arithmetic**. Warnings
  about broader-crate foreign functions/caller locations did not become reachable
  failures in these proofs. This is an exploratory checkpoint, not a parity gate.

The source fixtures for USING chains, nested EXCEPT and window binding again
match the pinned originals byte-for-byte, including required blank record
separators. This repairs the local SQL failure documented in the historical Kani
integration record. That original record and the Kani implementation are unchanged.
The root README remains identical to main.
