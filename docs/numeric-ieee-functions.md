# IEEE-dependent numeric functions: initial evidence

The retained [initial CLI probes](numeric-ieee-initial-cli-probes.json) record
unmodified outputs, commands, source commits, binary hashes and reference
versions. Each sequence runs in a separate in-memory process. Development
`99063af2bd` is the correctness authority; release `d8cdaa33fd` remains an
independent observation, not a reason to preserve the wrong development result.
This is a gap inventory before implementation, not a passing parity report.

Development's default `sqrt(-1)` returns NaN. Release returns OutOfRange and the
current Rust `9cd0303` callback returns Execution. Development's default ln/log/
log2 of zero return negative infinity, and pow(-1,0.5) returns NaN. Rust lacks
the setting and those additional functions. The affected source is
`extension/core_functions/scalar/math/numeric.cpp`: selected unary/binary bind
callbacks read `ieee_floating_point_ops` and retain the chosen math callback.
The initial scoped implementation will cover sqrt, ln, log/log10, log2 and
pow/power through selected DOUBLE casts and statement-local binding. Broader
trigonometric/gamma and function-catalog obligations remain visible follow-ups.

## Setting semantics

`IeeeFloatingPointOpsSetting` in `src/include/duckdb/main/settings.hpp` declares
BOOLEAN, default true and LOCAL_DEFAULT scope. SQL GLOBAL and SESSION overrides
are available. A SQL NULL override remains visibly NULL in current_setting;
the typed native `Settings::Get<...>` accessor falls back to the declared true
default when the stored value is NULL. This is not normalization of NULL into
true. Raw probes confirm the distinction after both prior false and GLOBAL
NULL settings. Integer 2 casts to true. An invalid string raises InvalidInput
in development; shared SET conversion-category behavior needs independent
validation rather than family-local error rewriting.

The existing Rust SettingRegistry, owned SettingsSnapshot and selected scalar
bind/cast interfaces can express this family without a private registry or a
new public trait. The setting definition will be isolated from the functions.
Setting retention belongs to the selected bound function, not a runtime lookup
from the evaluator's ambient context.

## Prepared-plan dependency

Independent native SQL PREPARE probes expose an existing shared gap. A prepared
constant sqrt(-1) keeps the IEEE mode from preparation after SET changes; its
parameterized counterpart binds with the mode at execution. Preparing
current_setting also retains its original constant, and prepared ORDER BY keeps
its original NULL ordering after the default changes. These are not unique to
IEEE math. The Rust public PreparedStatement currently retains syntax only and
rebinds on every execution; an existing settings test explicitly expects that
refresh and is not reference-parity evidence for retained preparation.

`src/main/prepared_statement_data.cpp::RequireRebind` checks explicit always-
rebind policy, unresolved parameters, parameter logical types and referenced
catalog identities. It does not invalidate every bound plan for arbitrary
setting changes. A correct shared solution must retain selected bound plans or
bindings while preserving catalog invalidation and typed-parameter rebinding;
merely freezing all settings or special-casing sqrt cannot meet that contract.
The integration lead owns this dependency. No prepared infrastructure or its
existing tests have been changed by this investigation. Rust SQL PREPARE is
itself unsupported in the initial CLI report, so those Rust parser failures are
not a substitute for future public-API prepared tests.

## Other affected arithmetic

Native arithmetic.cpp's BindBinaryFloatingPoint also consumes IEEE mode and the
separate null_on_division_by_zero setting. Rust's selected operator interface
currently lacks contextual specialization and retains nonnullable division
metadata. Exposing the math setting must not imply operator-wide setting parity:
division/remainder binding, nullable results and that separate setting remain
coordinated shared work. No runtime settings read has been inserted into the
existing arithmetic kernel, and no aggregate/vector performance paths changed.

No performance or Kani result is claimed by this read-only initial inventory.
