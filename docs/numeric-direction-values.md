# Numeric integral-direction increment

This continuing scalar-family increment adds `ceil`, `ceiling`, `floor` and
`sign`. It is not full numeric-function or database parity. Pinned development
`99063af2bd` governs correctness; release `d8cdaa33fd` remains separately checked.

Reference implementation: `extension/core_functions/scalar/math/numeric.cpp`
(`SignOperator`, `SignFun::GetFunctions`, `BindGenericRoundFunctionDecimal`,
`CeilDecimalOperator`, `FloorDecimalOperator`) and `src/function/cast_rules.cpp`.
Independent development CLI probes confirm the following behavior:

- `ceil`/`ceiling`/`floor` expose FLOAT, DOUBLE and DECIMAL overloads. Integral
  and BIGNUM arguments use the selected DOUBLE cast, rather than an exact
  integer identity overload. DECIMAL(w,s) returns DECIMAL(w,0); coefficients
  remain exact through width 38, including scale 38 and scale-zero identity.
- `sign` returns TINYINT. Signed and unsigned integral arguments retain their
  full source domains. DECIMAL and BIGNUM use the selected DOUBLE conversion.
  NaN and both signed zeros produce zero; positive/negative infinity produce
  positive/negative one.
- Floating direction functions retain FLOAT versus DOUBLE and negative zero.
  Untyped NULL resolves to DOUBLE for ceil/floor and returns TINYINT from sign.
  Bare string arguments remain ambiguous/rejected; typed VARCHAR, ENUM,
  BOOLEAN, temporal and nested inputs do not receive hidden numeric casts.

The first SQL test exposed that sqlparser produces dedicated CEIL/FLOOR AST
nodes that the binder did not support. The selected dialect now parses those
calls through the ordinary function grammar, including zero or arbitrary second
arguments. Function selection, argument casts and invalid-arity diagnostics
therefore use the ordinary selected catalog path. Datetime `TO` syntax remains a
parser error, as in development. No binder function-name switch, private cast
registry, vendor modification or new public interface was introduced.

Three component tests cover independent Euclidean quotient/remainder checks at
every DECIMAL width/scale, extrema, NULLs, selected vector views, signed floating
zeros, nonfinite values, cancellation and malformed adapter calls. Both
evaluators and optimizers exercise overload metadata, typed parameters, selected
cast replacement, fatal child failures and lazy branches. Typed results cross
joins, groups, window sums, decimal primary keys, failed atomic updates, rollback,
nested NULLs, private/native checkpoints, native WAL recovery and reopen.

The parser AST regression passes. Numeric (30), casts (11), floating (7), binary
scalar (8), workspace/all-target check and clippy pass. Instrumentation coverage
reports 292 files, 2,619 functions and 208 interface methods with no omissions.
Paired SQL/native observations, unchanged upstream-file execution, tracing and
the lead's maintained integrated Kani checkpoint are pending. This commit is an
integrable internal step, not a declaration that the substantial stage is done.
No performance acceptance measurements are claimed. Precision-aware
`round`/`trunc` and `round_even`/`roundbankers` remain the next numeric work.

The first paired campaign built
production binaries in 1m43s on unchanged integrated source. Development matches
218/226 SQL cases and release 190/226. The eight development mismatches are
existing DECIMAL(w,w)-to-VARCHAR formatting: Rust includes a major zero (`0.9`),
where both references omit it (`.9`). All direction-function values/types match
within those failing queries. Independent CLI probes confirm the discrepancy,
including zero, signs, concat and list child rendering. The source is
`DecimalToString::FormatDecimal` in
`src/include/duckdb/common/types/cast_helpers.hpp`: the major component is emitted
only when width exceeds scale. This is a real selected-cast correctness gap,
not numeric tolerance or an assertion to normalize away. Its repair and exact
rerun follow; the initial failures are retained.

All three native producer paths (C++, Rust checkpoint and Rust WAL) pass both
pins, including typed defaults, selected direction expressions, primary-key
updates, rollback and cross-engine checkpoint/reopen. The initial
unchanged upstream floor/ceil file passes all
16 records. Its wrapper exits 1 because a selected file is not whole-suite
parity. After merging the integration lead's source, numeric 31, casts 12 and
binary scalar 8 tests pass. The paired report remains unsuccessful until the
eight development formatting mismatches are repaired; the remaining 28
release-only mismatches are retained earlier numeric/version divergences.

The selected decimal text cast now omits the major zero exactly when width
equals scale. Generic diagnostic `Value` display is unchanged. A new component
test checks all 38 widths, signs, zero, retained batch casts, nested/concat text,
both evaluators/optimizers, typed parameters, text primary-key lookup and native
WAL/checkpoint/reopen. Unquoted `ceil`/`floor` identifiers, qualified columns and
aliases also use ordinary identifier grammar instead of the parser's special
datetime function production. Two additional paired SQL queries retain these
identifier cases.

The repaired paired campaign builds
production binaries in 1m31s on unchanged source: development **228/228 SQL**,
release **200/228 SQL**, and **3/3 native producers on each pin**. All 166 added
numeric-direction checks now match both references. The 28 retained
release-only disagreements remain, so the strict all-pin wrapper exits 1; this
does not hide or normalize any development mismatch. Native results include
exact final values and hashes of each produced checkpoint/WAL.

Numeric 32, casts 12, floating 7, binary scalars 8 and the parser AST unit pass.
Workspace/all-target check and clippy pass. Coverage reports 299 files, 2,701
functions and 209 interface methods without omissions. Instrumentation
compatibility passes in 38.78 seconds, with zero error returns, panics or open
spans; temporary telemetry is deleted. These are ordinary integrated-source
checks, not a new worker Kani run. The lead owns the maintained substantial
checkpoint before marking this stage complete. Precision rounding continues
next through an explicit selected typed-constant binding seam.
