# Conservative exact numeric SQLLogicTest comparisons

The Python SQLLogicTest oracle previously required identical rendered strings
for every ordinary expected cell, so numeric `42` versus `42.000000` failed.
The original rounding/truncation failures are retained in the
[precision findings](numeric-precision-values.md).

Pinned development `test/sqlite/result_helper.cpp:479-545` first checks exact
strings and regexes, then casts mismatched numeric cells to the query's returned
logical type before `Value::ValuesAreEqual`. `src/common/types/value.cpp:2478`
also permits approximate FLOAT/DOUBLE equality. This increment implements only
a conservative exact subset, not the full source comparator: arbitrary-precision
decimal equality, declared integer bounds/integrality, exact DECIMAL width/scale
representability, and floating NaN/infinity spellings. The query's returned
numeric type is authoritative, not its SQLLogicTest I/R/T header.

No tolerance or lossy numeric conversion is introduced. Expected values needing
rounding and approximate floating comparisons remain failures/explicit oracle
limits. VARCHAR, BLOB, BOOLEAN and other nonnumeric results do not gain numeric
equivalence. Regex assertions, hashing and label digests remain exact and use
the original result strings. Mixed-type valuesort loses column ownership after
flattening, so this fallback is disabled there instead of guessing a numeric
type from the sorted position; homogeneous numeric valuesort remains supported.

The test-oracle suite passes 17 tests with Python warnings treated as errors.
New cases cover exact finite/exponent/zero equivalence, full signed/unsigned
128-bit values, 38-digit decimals, special floating values, wrong values,
precision/range violations, missing/nonnumeric metadata, strict text/regex/hash
checks and mixed-column ownership. A test's initial invalid Python regex escape
was corrected to a raw string; no production behavior depended on that warning.
Unchanged upstream-file reruns follow separately, retaining original reports.

An independent development CLI edge audit found that unsigned casts reject
`'-0.0'` even though the exact mathematical value is zero. The initial fallback
would incorrectly accept this spelling difference. The repair conservatively
declines every minus-prefixed unsigned fallback spelling; this also declines
the valid integral `'-0'` spelling, an intentional false-negative limit rather
than permission to accept invalid unsigned input. Tests cover both small and
128-bit unsigned forms plus missing/unknown result-type metadata. Independent
exponent, trailing-zero and full-width unsigned casts otherwise confirm the
retained positive fixtures. The original upstream reports remain unchanged.

After merging combined base `4b2a027`, all 38 Python test-oracle/runner tests pass
with warnings treated as errors. The repaired unchanged upstream runs pass
ROUND_EVEN **20/20 expanded records**, TRUNC **47/47** and TRUNC precision
**51/51**. Final rounding results
and final truncation results
retain exact source identities and failures. The first exact-fallback
rounding run, before the unsigned
guard, is also retained; that file did not contain the newly audited minus case.

ROUND now reaches 5/17 before an approximate floating assertion: it expects
42.123400 and 42.12345, while both development SQL and Rust return 42.1235 and
42.1235. Independent development CLI casts to VARCHAR confirm those exact
outputs. This is the explicitly unimplemented approximate-comparison portion
of the source oracle, not a numeric result regression or permission to change
the expected SQL file. ROUND integers remains at 4/18 before unimplemented
`test_all_types`. Each selected-file runner exits 1 because these runs are not
whole-suite parity, including the two fully passing truncation files.
