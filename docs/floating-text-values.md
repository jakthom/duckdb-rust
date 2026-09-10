# Selected floating-point text casts

This is an internal correctness increment in the value-and-expression milestone,
not a declaration of numeric, nested, or performance parity. Development
`99063af2bd` is the correctness authority; release `d8cdaa33fd` is also observed.
Diagnostic `Value::Display` and CLI display formatting are intentionally unchanged.

Selected FLOAT/DOUBLE-to-VARCHAR casts now use the pinned formatter's digit
generation and layout. Fixed notation applies for scientific decimal exponents
-4 through 15, integral fixed values retain `.0`, and scientific exponents have
an explicit sign and at least two digits. Signed zero, signed NaN, infinity,
subnormal values and the FLOAT-to-promoted-DOUBLE fallback are retained. Both the
ordinary primitive adapter and the alternative exact-numeric adapter use this
implementation; no ambient cast-registry lookup or diagnostic stringification
substitutes for a selected cast.

Nested-to-VARCHAR binding now retains each declared child's selected VARCHAR
cast. LIST, ARRAY, STRUCT, TUPLE, MAP and UNION keep their existing container
layout, escaping and parent-versus-child NULL distinctions. VARIANT's retained
dynamic dispatch and scalar concat's retained casts consequently observe the
same floating text. Registry replacement remains local to newly bound casts;
already bound plans retain their adapters. The existing whole-value temporal
renderability guard still runs before nested rendering, and child failure
provenance, invalid output validation, cancellation and TRY_CAST fatal failures
remain enforced. No temporal-specific formatter was inserted into concat.

## Reference implementation and non-round-tripping witnesses

The primary entry point is `duckdb/src/common/operator/string_cast.cpp:64–73`,
which invokes the pinned vendored fmt default formatter. The relevant sources
are `third_party/fmt/include/fmt/format.h` (default layout and promoted input)
and `format-inl.h` (cached powers, Grisu3, round-weed and exact FPP fallback).
The translated implementation retains fmt's MIT notice. Its bounded exact
integer workspace is local to finite binary64 formatting, not a constraint on
database BIGNUM values or a replacement for their representation.

An initial Rust-shortest-digit experiment did not match the pinned source.
Among 5,000 seeded words per type, 34 FLOAT outputs differed because a declined
Grisu3 conversion falls back using the promoted DOUBLE. For example, FLOAT bits
`ca2454ff` produce `-2692415.75`, not `-2692415.8`. DOUBLE bits
`42eaa13992d343a4` produce `234238063843869.12`, not `234238063843869.13`, because
the exact fallback resolves the midpoint to even.

The broader all-exponent corpus then exposed six signed DOUBLE power-of-two
witnesses that the pinned formatter itself renders incorrectly:

| Positive raw DOUBLE bits | Exact input | Pinned VARCHAR output |
| --- | --- | --- |
| `4500000000000000` | 2^81 | `4.835703278458517e+24` |
| `45a0000000000000` | 2^91 | `4.951760157141521e+27` |
| `7260000000000000` | 2^807 | `A.070116948172427e+242` |

Negative counterparts retain the corresponding leading minus sign. These are
observable development behaviors, not mathematically shortest representations
or valid numeric round trips. Independent standalone C++ CLI queries constructed
each positive input from its full exact decimal integer string, selected both
VARCHAR and raw BIT output, and confirmed the original binary words alongside
the surprising text on both pins. Complete commands, stdout and stderr are
retained in the reports. This confirmation does not depend on the raw-bit worker
transport used for the larger corpus.

The source cause is in FPP's `value.e >= 0` branch. For a power of two with a
closer lower boundary, the numerator shifts by two, but the denominator still
shifts by one. The quotient is doubled while the original decimal exponent
estimate remains. For 2^807 the initial quotient is 17, so the source's character
construction produces `A`. The implementation preserves this narrow observed
behavior rather than claiming a universal parse/format identity.

## Retained checks and limits

The [first retained diagnostic](floating-text-reference-diagnostic.json) records
the initial Grisu-plus-library-fallback failures: FLOAT 6,536/6,536 and DOUBLE
17,282/17,288 on each pin. Its raw failing rows are preserved. Its separate SQL
probe also used `union_value(a=...)`, exposing an existing Rust/frontend syntax
disagreement: the pinned engines require named argument `a:=...` there. The
follow-up uses the reference syntax; no parser behavior was changed by this
slice. The [exact-FPP diagnostic](floating-text-reference-fpp-diagnostic.json)
records the source-backed repair and independent CLI confirmation.

The final [production campaign](floating-text-reference-production.json), run
with `CARGO_BUILD_JOBS=2 python3 scripts/floating_text_reference.py --report
docs/floating-text-reference-production.json --fixtures
test/data/floating-text-production`, builds uninstrumented release binaries and
records unchanged source and pinned binary/library identities. Results:

- Both pins match all 6,536 FLOAT and 17,288 DOUBLE raw-bit cases: every sign and
  biased exponent with zero/one/maximal fraction boundaries, plus seeded words.
- Development matches all 10 SQL cases. Release matches 7/10; its differences
  are singleton TUPLE punctuation and two previously observed temporal error
  categories. Full diagnostics are retained; only category-label case is
  normalized by the declared error-category comparison.
- Native interchange passes all three producer paths on both pins: C++ files,
  Rust checkpoints and Rust WAL. Each checks exact rows and indexed lookups,
  defaults, selected text keys, signed zero, nested floating values, Rust
  mutation/rollback, C++ mutation/checkpoint and reopening.

The script's strict all-pins result remains false and it exits 1 because of those
three release disagreements; the development trial passes. No failing result
has been deleted or renamed a pass. The gzip fixtures retain independent C++
outputs and diagnostic Rust outputs with content hashes. Component tests load
only the C++ expected-text field, reconstruct raw IEEE values directly and make
47,648 exact assertions independently of the SQL fixture transport.

Seven floating component tests pass. They cover both selected numeric adapters,
explicit/assignment casts, scalar/flat/dictionary/constant vectors, all nested
families, registry replacement and retained binding, both SQL evaluators and
optimizers, typed parameters, failed key mutations, rollback, private/native
checkpoint and WAL reopen. A separate deterministic 80,000-iteration sample
checks finite numeric round trips without asserting that property for the
explicit pinned exceptions above. Regression suites pass numeric (27), casts
(11), contracts (27), operators (10), BIGNUM (3), binary scalar (5), nested (24)
and temporal (21). Workspace/all-target check and clippy pass. Instrumentation
coverage reports 288 files, 2,596 functions, 208 interface methods and no missing
attributes; exhaustive tracing compilation passes and deletes its telemetry.
The focused exhaustive SQL reproduction produced `v=[1.0, -0.0]` and the
pinned 2^81 text with 59,176 operations, zero error returns, panics or open spans.
Its wrapper nevertheless exited 1: the final ARRAY test edits occurred during
the trace build, violating the wrapper's unchanged-source guard. This is not
reported as a passing frozen-source trace; temporary telemetry was deleted.

The lead owns the maintained Kani run and investigation on the combined code
before declaring this substantial stage complete. No floating-text acceptance
benchmark was run. The source translation and sampled tests do not establish
all-input equivalence or a formal bound proof; broader performance, malformed
reference outputs outside the retained corpus, diagnostic display, and other
scalar/nested catalog gaps remain separate obligations.
