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

## Setting prerequisite

The selected builtin setting now declares the source BOOLEAN metadata, true
default, session-default scope and both global/session support. Normalization
preserves Boolean and NULL payloads; current_setting therefore retains NULL
instead of replacing it with true. Existing configuration providers own
publication, reset, cancellation and snapshot lifetimes unchanged.

Three connected tests run with both snapshot and locked providers, covering
global/session precedence, independent connections, NULL masking and reset,
retained snapshots, nontransactional SET across rollback, numeric SQL casts,
failed-input atomicity, selected Boolean cast replacement, Resource failure and
invalid callback output. An initial replacement test passed an Arc to a builder
method that accepts an owned CastRegistry; that test-only compile error was
corrected before final verification. Check, settings 11, all-target clippy,
coverage (374 files / 3,604 functions / 239 interface methods, missing 0) and
trace compatibility pass. No new math consumer or prepared retention behavior
is claimed by this prerequisite; it is an internal step toward the next slice.

The invalid-string SET category remains the existing Conversion-versus-native-
InvalidInput gap. A direct BoundCast::attempt call could retain cast failure
origin but would bypass the selected expression evaluator used by SET today;
that shortcut is not used. A retained origin-aware context/wrapper needs shared
coordination before fixing the category. No comparator has been changed.

## Selected math implementation

The provisional family now registers sqrt, ln, log/log10, log2 and pow/power.
The selected overload request advertises the source DOUBLE signatures (including
both log arities), validates the returned candidate and retains its signature
with the statement's IEEE mode. Ordinary binding inserts selected casts;
evaluation accepts only those retained DOUBLE inputs. The old sqrt callback's
hidden numeric conversion and unconditional negative-input Execution error are
removed. Frontends without selected overload binding explicitly reject the
adapter rather than silently selecting builtin conversions.

Strict sqrt rejects negatives; strict logarithms reject negatives and zero;
strict base-log validates its base before its value and rejects a zero logarithm
of the base. Strict pow only rejects zero to a negative power: it still produces
NaN for negative nonintegral powers and infinity for overflow. IEEE mode uses
the ordinary floating operation. NaN, infinities and signed zero remain typed
DOUBLE values. No diagnostic Value display or floating text formatter changes.

Four connected tests cover both evaluators/optimizers, every fixed numeric
family plus DECIMAL/BIGNUM casts, string-literal versus typed-VARCHAR binding,
prepared parameters, lazy branches and physical constant-NULL demand. Retained
bound callbacks are executed under a different ambient mode to prove that they
do not reselect settings. Missing frontend capabilities, invalid selected
indices/arity, malformed input and cancellation fail explicitly. Selected cast
replacement and Resource failures remain observable. Results cross nested text
casts/concat, joins, grouping, windows, primary-key tables, prepared updates,
atomic failed mutation, rollback, both snapshot formats, native WAL/checkpoint
and reopen, including stored NaN, infinity and NULL.

Two test-only construction issues were repaired: the persistence test used a
nonexistent constructor instead of FileCheckpoint::open, and its first concat
combined LIST with scalar strings without the explicit VARCHAR cast required by
both engines. No engine rule was loosened for either test. Check, numeric 55,
settings 11, contracts 71, casts 13, nested 42 and all-target clippy pass;
coverage reports 376 files / 3,632 functions / 239 interface methods, missing 0.
Trace compatibility also passes (one completed build operation, no errors,
panics or open spans); temporary telemetry was deleted.
The paired refresh and integrated Kani remain pending for this continuing math
slice. The parent separately reports checkpoint14 at abf7eed passed all six
maintained Kani harnesses; that validates the earlier combined rejection slice,
not these later IEEE additions or full prepared-plan parity.

## Initial paired math refresh and regression investigation

[The initial paired report](numeric-ieee-reference-initial.json), on frozen
37c2962 source, records 1,086/1,088 development SQL passes and 3/3 native numeric
round trips on both pins. All earlier 944 development passing SQL identities
remain passing. Exact FLOAT/DOUBLE text is checked without numerical tolerance;
no comparator change hides exceptional values or small arithmetic differences.

Besides the previously recorded invalid-string SET category gap, the expanded
campaign exposed a new math binding omission. `pow(CAST('bad' AS DOUBLE),
NULL::DOUBLE)` returns NULL in both references, whereas the initial Rust math
consumer raises Conversion. The initial test incorrectly expected that error.
An independent development CLI query, repeated after `PRAGMA disable_optimizer`,
returns NULL in both executions, so this is not an optimizer-only effect.
`src/function/function_binder.cpp:614–651` probes foldable inputs after selecting
an overload; any proven NULL replaces a default-NULL function before ordinary
function binding. A failed recoverable probe can therefore precede a successful
NULL probe. Physical Constant-NULL execution in child order alone cannot express
this earlier binding boundary. The repair will use the existing selected
`is_provably_null` request and TypeOnly specialization, without changing their
failure rules, inventing literal values or creating a new shared interface.
This initial evidence remains immutable, including its incorrect expected-error
annotation, for comparison with the repaired campaign.
