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
CASE, comparison, conjunction, NULL-test, BETWEEN, IN and LIKE trees use DuckDB's
parsed-node representation. Unqualified `INTERVAL` retains the native VARCHAR-to-
INTERVAL cast, and supported qualified units retain the ordinary DOUBLE/trunc/width-
cast/`to_*` lowering without executing it during DDL. Built-in current-date/local-time
and timezone/calendar defaults remain G04 work. Real sequence objects and `nextval`,
general catalog function identity, multi-catalog routing, non-schema dependencies
and prepared-plan invalidation remain G10 work.
DML-level explicit `DEFAULT`, CHECK constraints and generated columns remain G11 work.
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
pulls one source batch with a 2,048-row maximum, then evaluates omitted defaults
column-major over exactly that batch. A short natural child batch remains a boundary;
the source and defaults therefore share one effect pipeline rather than collecting
the complete source first. Explicitly supplied columns bypass their defaults. All
rows remain staged until evaluation and validation succeed, so a later source or
default failure publishes no partial insert; empty input has no effects and
`DEFAULT VALUES` supplies one row.

Predicate demand depends on consumption mode. Projected and retained conjunction
values evaluate both runtime children, while filter selection can stop once a row
cannot change membership. Development-authoritative evaluation skips undemanded
constant-NULL comparison siblings and decisive pure BETWEEN/IN branches without
reordering remaining observable children. Direct BETWEEN input is evaluated once;
effectful non-NULL input retains eager bound demand. LIKE/NOT LIKE likewise suppress
an undemanded sibling when the other argument is constant NULL. These rules apply
through both scalar and batch evaluators and through retained-default rebinding.

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
development FUNCTION checkpoint used by the focused Rust regression. The process
runs every case independently, so a known release WAL failure cannot hide later
checkpoint results. It covers FUNCTION/CASE/predicate/interval checkpoint exchange
in both directions, Rust-origin WAL recovery, deferred failures, and DuckDB metadata
for an absent default versus explicit `DEFAULT NULL`.

Relevant sources: Rust `src/catalog/expression.rs`, `src/main/client_context.rs`,
`src/planner/binder/{capture,stored}.rs`, `src/planner/stored.rs`,
`src/storage/table/{alter,recovery}.rs`, `src/storage/duckdb/{catalog,parsed}/`;
upstream `src/planner/binder/statement/bind_create_table.cpp`,
`src/catalog/catalog_entry/duck_table_entry.cpp`,
`src/storage/table/{row_group,row_group_collection}.cpp`.

## Runtime catalog identity

Snapshot catalogs maintain process-local catalog/object IDs and a separate observed
catalog version. Schema/table creation uses one version-bound prepared insertion in
both transaction-current and catalog-basis views; replay after a catalog change is
rejected. Rename preserves identity, drop/recreate allocates a replacement identity,
and identified ALTER/DROP resolves the stable object rather than trusting its old
name. Table-to-schema dependencies make restricted schema drops atomic. Private and
native reopen rebuild fresh runtime IDs; no ID or dependency state is serialized.

This is a storage/transaction foundation, not complete catalog resolution. Binder,
logical and physical plans carry stable table bindings. Query/DML validation requires
the exact observed catalog version until per-object schema versions exist; stable DDL
follows rename and never targets a same-name replacement. Physical scan open validates
again for direct public physical-plan callers. Optional identity lookup distinguishes
absence explicitly so `IF EXISTS` does not hide arbitrary catalog errors. Alternative
frontends obtain owned definitions/bindings through `Connection::resolve_table`.

The pure search-path model is connected to a session-only setting for the current
catalog. Binder lookup searches its normalized schema list before `main`; unqualified
creation uses the current schema, and prepared API statements retain syntax and rebind
against the execution session. Catalog-qualified entries are rejected until attachment
routing exists. General objects, attachments, temporary scope and metadata catalogs
remain G10 work. Ordinary quoted function identifiers and the supported `main`
qualification use the selected registry; retained native `main.list_value` follows
the same rule without fake alias entries. Rust preparation remains syntax-only; native
prepare-transaction ownership and retained-plan invalidation are still open.

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
Native UNBOUND metadata is resolved only from its side-effect-free serialized type
tree into concrete built-in scalar or nested types, sharing the enclosing Value
codec's depth/node/byte/cancellation budgets. Qualified, user-defined and extension
types still need selected catalog binding. Unknown ordered fields cannot be safely
skipped without knowing their wire layout.

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

Core fixed-offset timezone extraction reads TIMETZ's stored offset and treats
DATE/TIME/TIMESTAMP inputs as offset zero. `timezone(INTERVAL, TIMETZ)` wraps the
adjusted wall clock modulo one day and retains the requested second-granularity offset.
Rust rejects offsets outside +/-15:59:59 before physical encoding; named zones, DST,
ambient timezone and ICU calendars are separate work.

Core `strptime` binds `text` and `format` by semantic name, compiles only constant
scalar/list formats, and keeps each output timestamp precision and timezone identity.
For format lists, ordinary `strptime` stops on a syntactically matched conversion
failure while `try_strptime` may continue to a later format. TRY also follows the
pinned helper's unusual initialized-1900 result for special timestamp words and
rejects finite calendar parses that collide with infinity sentinels. Nanosecond
composition adds fraction to time-of-day before the date; changing that checked
arithmetic order rejects valid exact boundary values.

Transaction-current TIMESTAMPTZ comes from a selected `TransactionClock` sampled once
before the transaction snapshot is acquired. Query evaluation and retained defaults
only read that captured microsecond instant; background contexts never fall back to the
host clock. Callable aliases use ordinary FUNCTION nodes, while bare or quoted
`CURRENT_TIMESTAMP` is a distinct retained leaf and native class-4 column-reference
encoding. Ordinary columns, SELECT aliases and relation-as-STRUCT values take
precedence over the SQL-value fallback, including grouped ordinal remapping.

Generated math catalog aliases are callable by quoted identifier. This does not add
SQL grammar for exponentiation or postfix factorial. Gamma/log-gamma use the selected
statement IEEE setting, `binom` owns checked HUGEINT overflow and cancellation, and
`isnan` retains exact FLOAT/DOUBLE overloads.

Binary `decode` first validates UTF-8 and therefore does not inspect its optional
mode for valid input. For malformed input, development-authoritative `ignore`
reconsiders a mismatching continuation byte and `replace` writes one `?` for every
consumed malformed byte. Provably NULL arguments suppress all siblings; a dynamic
NULL preserves ordinary left-to-right demand. `unbin` places a partial leading group
in the low bits of its first byte. Both codecs use bounded owned output and poll long
scans rather than aliasing their source values.

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
