# Absolute-value increment

ABS now retains its selected logical input domain instead of treating every
signed value as the full physical i128 container. This repairs the confirmed
`abs('-128'::TINYINT)` mismatch: development returns
`Out of Range Error: Overflow on abs(-128)`, while the earlier Rust implementation
reached `Internal Error: scalar function returned an invalid logical value`.
The other narrow signed minima had the same cause; HUGEINT's checked-absolute
failure previously used Execution rather than OutOfRange.

The pinned development source is
`extension/core_functions/scalar/math/numeric.cpp:99-274` and
`src/include/duckdb/common/operator/abs.hpp`. All five signed widths explicitly
reject their own minimum, unsigned overloads preserve the exact domain, and
floating overloads use absolute value with the sign bit cleared, including
negative zero. The decimal callback retains its exact width and scale. NULL
overload resolution selects BIGINT; the decimal template returns SQL NULL only
when its child is provably NULL, not merely because a column contains NULL.
BIGNUM selects an ordinary bound DOUBLE cast, rather than a hidden conversion.

The family-local adapter keeps those rules behind normal scalar registration,
selected casts and evaluators. Its decimal template uses the existing explicitly
speculative NULL probe, preserving lazy failed-cast branches and fatal adapter
failures. It does not add statistics shortcuts, alter shared vector/storage
representations, or change diagnostic Value display. Input contract validation
stays Internal; a valid signed input whose absolute value exceeds its declared
type reports OutOfRange.

Three tests cover every signed/unsigned width, every DECIMAL width/scale, NULLs,
flat/sliced/dictionary/constant value views, floating bits, cancellation, invalid
physical inputs, SQL overload errors, selected BIGNUM-cast replacement and fatal
failure, lazy decimal branches, typed prepared parameters, nested and concat
output, joins/groups/windows, primary keys, atomic failed updates, rollback,
private/native checkpoints, native WAL and reopen. Both expression evaluators
and both optimizers are exercised. An initial test oracle used `-minimum - 1`
for the i128 maximum, which itself overflows before subtraction; rewriting the
independent expectation as `-(minimum + 1)` repairs the test. No production
overflow check was weakened. An isolated clippy test-slice clone warning was
also repaired.

On combined base `4b2a027`, numeric 38, contracts 29 and casts 12 tests pass;
workspace/all-target check and clippy pass. Instrumentation coverage reports
317 files, 2,937 functions, 214 interface methods and zero omissions.
The lead owns the maintained integrated Kani checkpoint before substantial
stage completion. No performance acceptance claim is made for this increment.

The [expanded paired report](numeric-absolute-reference.json) retains all prior
607 numeric SQL cases and adds 65 ABS cases. Its unchanged production source
build takes 1m41s. Development matches 669/672 and release 333/672; all three
native producer paths pass both pins, now also reading exact ABS expressions
over DECIMAL and UHUGEINT after each cross-engine mutation/reopen. The 63 new
non-CASE cases match both references. The two new CASE witnesses return the
correct value but expose an existing shared literal-combination metadata gap:
`CASE WHEN false THEN abs('-128'::TINYINT) ELSE 1 END` is INTEGER in Rust versus
TINYINT in both references; SMALLINT behaves analogously. Independent ABS-free
CLI probes confirm `typeof(CASE WHEN false THEN 0::TINYINT ELSE 1 END)` is
INTEGER in Rust versus TINYINT in development. Explicit `ELSE 1::INTEGER` is
INTEGER in both. The lead owns that frontend repair; neither the expected type
nor the raw failure has been removed. The third development failure is the
previously retained bare full-width UHUGEINT literal. Older release semantic
disagreements remain unchanged, so the strict all-pin wrapper exits 1.

The [unchanged upstream ABS file](upstream-numeric-absolute.json) passes its
single record, including FLOAT/DOUBLE negative zero. The wrapper's exit 1 still
denotes incomplete whole-suite coverage, not this file failing. Instrumentation
compatibility passes with zero error returns, panics or open spans in 46.79s;
temporary telemetry is deleted. This compatibility check is not a traced SQL
reproduction or an ABS arithmetic proof. No new worker proof result is claimed.

One separately recorded next scalar gap is setting-dependent IEEE math:
development's default `sqrt(-1)` yields NaN, while the current Rust callback
returns Execution. `BindIEEEFloatingUnary` in the same native numeric source
selects behavior through `ieee_floating_point_ops`; Rust does not yet expose
that setting. Correct implementation requires coordinated setting and affected
function/operator contracts, not a special case inside ABS. This remains open.
