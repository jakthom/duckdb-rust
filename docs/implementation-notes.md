# Implementation contracts worth retaining

These notes preserve difficult source-backed findings from the removed progress
reports. They are constraints and open integration questions, not historical test
counts or a second status dashboard. Work ownership is in the
[parity backlog](parity-backlog.md).

## Retained defaults

`ColumnDefinition.default` is an `Option<StoredExpression>`: absence differs from
an explicit typed `DEFAULT NULL`. Catalog validation and private snapshots retain
the owned syntax and provenance. SQL CREATE/SET/ADD capture and selected native
checkpoint/WAL integration are connected for closed representable trees. Native
CASE/predicate-tree coverage still needs its final gate. Row-level explicit
`DEFAULT`, CHECK and generated expressions remain G11 work; current-time defaults
remain G04 work and sequence-backed defaults remain G10 work.
Independent Base64 (`DEFAULT from_base64('AP8=')`) and calendar function defaults
expose this difference. A Rust-produced file can appear to work because the writer
stored the evaluated value instead of retaining the default's semantics.

Retain declared literal type separately from physical payload, typed NULLs,
explicit/TRY_CAST nodes, qualified function names, aliases, named arguments,
operator identity and legacy/modern argument provenance. Do not serialize selected
adapter handles or use diagnostic SQL display as the owned expression representation.
Keep selected type/cast/function/binder/evaluator and settings context throughout
startup, statements, checkpoint encoding and recovery. Do not create a hidden
built-in registry inside a decoder when the selected context is unavailable.

Binding and evaluation demand are different. CREATE/SET DEFAULT must not eagerly
execute expressions that the reference defers until INSERT or ADD backfill. INSERT
evaluates omitted defaults column-major within each fixed 2,048-row standard DuckDB
vector, then advances to the next vector. Explicitly supplied columns bypass their
defaults. All rows remain staged until evaluation and validation succeed, so a
later default failure publishes no partial insert; empty input has no effects and
`DEFAULT VALUES` supplies one row.

For ADD, the pinned development behavior wins the known reference divergence.
Development treats a constant, or one non-TRY cast directly around a constant, as
simple; v1.5.5 treats only a bare constant as simple. A non-simple ADD is modeled as
ADD NULL, UPDATE and SET DEFAULT, so it evaluates only visible rows. A simple ADD
evaluates every retained physical slot, including deleted slots, and can therefore
fail when visible cardinality is zero. The core path resolves the applicable slot
stream once and reuses its values for transaction-current, catalog-basis and WAL
state. After a successful checkpoint reclaims deleted slots, the same ADD can
succeed. VACUUM is not interchangeable, and a rolled-back insertion does not
establish retained state.

Physical slots also retain update order. Regular scalar updates stay in place;
updates that name indexed columns or require delete-insert handling for nested data
mark the old slot deleted and append the replacement in encounter order. Native WAL
recovery, logical scans, checkpoint row streams and checkpoint layout mappings use
that same order. A successful manual or automatic checkpoint compacts the current
acknowledged snapshot only. Failure leaves generation, slots and durable state
unchanged, while older reader snapshots retain their holes and pending writers keep
their own valid basis.

The pinned 1.5.5 release cannot replay a SET DEFAULT FUNCTION record from its
own WAL during startup (`GetDefaultDatabase` is unavailable at that phase). Use
the pinned development process for FUNCTION-default WAL interoperability; the
release remains valid for checkpoint-origin cases.
`scripts/default_interoperability_reference.py` owns the bidirectional process
check, and `scripts/generate_default_function_fixture.py` owns the checked-in
pinned-development checkpoint used by the focused Rust regression. The process
checks retained FUNCTION behavior and deferred failures in both directions, plus
DuckDB metadata for an absent default versus explicit `DEFAULT NULL` across Rust
checkpoint and WAL origins.

Relevant sources: Rust `src/catalog/expression.rs`, `src/main/client_context.rs`,
`src/planner/binder/{capture,stored}.rs`, `src/planner/stored.rs`,
`src/storage/table/{alter,recovery}.rs`, `src/storage/duckdb/{catalog,parsed}/`;
upstream `src/planner/binder/statement/bind_create_table.cpp`,
`src/catalog/catalog_entry/duck_table_entry.cpp`,
`src/storage/table/{row_group,row_group_collection}.cpp`.

## Native parsed expressions and values

Parsed-expression headers have class/kind fields 100/101; alias and location
metadata use optional 102/103 and development location length 104. FUNCTION is
class 9/kind 140. Function fields include legacy name/schema/children (200–202),
FILTER (203), ORDER modifier (204), DISTINCT/operator/export-state (205–207),
legacy catalog (208), modern FunctionArgument objects (209), and QualifiedName (210).
A non-null *empty* order modifier is normal; rejecting every non-null field 204
rejects valid ordinary function defaults. Nullable pointers/objects have wrappers.

Development accepts legacy child aliases as names and modern named arguments as
distinct binding provenance. Nonempty modern qualification replaces legacy names
when the source permits it. Preserve qualification and reject contradictory or
unrepresentable conversions; silently dropping names is not compatibility.
Pre-v2 native output accepts only the positional Named-to-Legacy case with no names
or aliases; genuinely named arguments require the modern representation and an
ambiguous downgrade is rejected before publication.

CAST target metadata and materialized Value support have different version gates:
a type can be a valid CAST target in a version that cannot store that Value payload.
UNBOUND parsed type syntax needs selected type/catalog binding. Unknown ordered
fields cannot be safely skipped without knowing their wire layout.

One codec session bounds an entire recursive root. Current retained-tree limits
are depth 64, 16,384 nodes and 16 MiB of identifier bytes; the typed-Value session
has a 64 MiB byte budget. Reserve unvisited siblings before allocation and account
logical visits through shared subtrees. Preflight/stage a write before appending;
malformed input, cancellation or limit failure must leave caller output unchanged.
These safeguards do not replace global query-memory accounting.

Relevant Rust sources: `src/storage/duckdb/{parsed,value}/`,
`src/storage/duckdb/catalog/unbound.rs`. Independent fixtures and tools remain in
`test/data/`, `scripts/native_nested_expression_reference.py`,
`scripts/native_value_codec_reference.py`, `scripts/verify_native_parsed_codec.py`.

## Exact native content and physical identity

SQL equality is too weak to validate a rewritten native checkpoint. Recursively
compare physical types, declared widths/scales, FLOAT/DOUBLE bit patterns, signed
zero, NaN payloads, NULL validity, UNION active tags, VARIANT child identity and
all nested metadata. Metadata/default validation cannot hide behind a row checksum.

Native deletion vectors may encode row values separately from row IDs. Preserve
deletion holes, append high-water marks and old snapshot identity. An aligned
missing file suffix is acceptable only when validated free-block metadata covers
every omitted block; live referenced blocks must remain present. Free-tail handling
must not accept arbitrary truncated files. Reclamation interacts with G09 demand.

Actual file version belongs to the bound format/session and must be carried into
WAL and successor publication. A default configuration is not the loaded file's
version. Selected VARIANT storage begins at 68 and TUPLE/empty STRUCT support at 69
in the current adapter; other versions/representations require independent checks.
Bidirectional native exchange, private-format round trips and raw wire-codec tests
are three different scopes.

Sources: `src/storage/duckdb/nested/variant/exact/`,
`src/storage/duckdb/{free_tail,visibility,write_support}.rs`,
`src/storage/table/layout/`, `test/compatibility/row_identity.rs`.

## Binding, NULLs, effects and retained services

Literal inference is source-order sensitive and different from typed API parameter
coercion. Preserve full-width integers and declared types through CASE, VALUES,
COALESCE/NULLIF, constructors and nested common types. An explicit cast remains
meaningful even when source and target physical types coincide.

Conditional expansion must bound the fully substituted tree, counting argument
multiplicity and combined depth before cloning. Validate complete input trees before
calling replacement adapters. Do not suppress already demanded failures when the
final result is known NULL. TRY_CAST/TRY-like behavior may recover only permitted
conversion failures; resource, interruption and internal errors must propagate.

Selected casts, operators, functions and child comparators belong to retained bound
objects. Replacing an ambient registry must not silently redirect a prepared object.
Named overloads and typed constant requests must keep their original argument
identity, type/constant availability and effect checks. An interface prerequisite
does not establish that SQL, catalog defaults and all scalar/batch consumers use it.

Sources: `src/planner/binder/{coercion,overload,scalar_expansion}.rs`,
`src/function/{expansion,signature}.rs`, `src/common/cast/failure.rs`,
`src/execution/expression_executor/provenance.rs`.

## Temporal and numeric behavior

Keep physical timestamp validity separate from calendar representability and
arithmetic intermediate ranges. Valid extreme native timestamps cannot be rejected
because a convenient negation, absolute value or narrower conversion overflows.
Retain each precision and timezone/interval component in keys, metadata and files.

Both pins can differ on floating text, decimal/unsigned overloads, calendar periods
and diagnostics. Development is authoritative for demonstrated differences. Preserve
both outcomes. Some temporal operations distinguish constant from column execution;
folding or vectorization must not erase that behavior. Timezone/ICU coverage needs
the matching reference extension, not a substitute core-only result.

The IEEE floating-point setting is registered and selected math consumers exist.
Already-prepared binding/settings retention remains its own issue: reevaluating
everything against the latest session setting may disagree with the native prepared
plan. Extend the existing test/consumer contract rather than treating setting
registration alone as completeness.

## SQLLogicTest oracle limitations

The Python oracle implements exact numeric fallback using the returned logical
type, not merely the I/R/T header. Full reference behavior additionally includes
numeric conversion/rounding and approximate FLOAT/DOUBLE comparison. Adding those
rules must match upstream `test/sqlite/result_helper.cpp` and
`src/common/types/value.cpp`, with deliberate wrong-answer self-tests.

Do not extend numeric equivalence to VARCHAR/BLOB/BOOLEAN, regex assertions, hashes
or label digests. Mixed-type valuesort loses column ownership after flattening;
the current fallback declines it rather than guessing. Minus-prefixed unsigned
fallbacks are conservatively declined because native `'-0.0'` and `'-0'` conversion
can differ. Those false-negative limits remain harness work, not engine fixes.

Expected-error tests must not count missing engine capabilities as correct reference
errors. Skips/unknown directives/timeouts are gaps. Keep original assertions and
record whether failure comes from the engine, oracle or environment. Sources:
`scripts/sqllogic.py`, `scripts/test_verification_harnesses.py`, `test/runner/`.
