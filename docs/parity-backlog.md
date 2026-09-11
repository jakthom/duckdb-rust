# DuckDB functional parity: agent handoff backlog

Audited 2026-09-10 against Rust `20c8214216e6e1b59c80e9b5fcffcdee4ab9e232`
plus the local verification-runner changes. This is the maintained work map;
update the relevant entry when behavior changes instead of adding another
checkpoint/progress report. Source and tests take precedence over this snapshot.

## Target and evidence

The target is the local C++ DuckDB development checkout at
`99063af2bd7092aff02e14184a20e24699d34d71`, with a separate compatibility
obligation against v1.5.5 at `d8cdaa33fda8df955cc76ef58a280f68f4cd43fa`.
Development behavior wins demonstrated disagreements. See
[acceptance](../specs/testing/parity.md) and [reference builds](reference-builds.md).
This is a bounded source target, not an assertion about every future DuckDB
release or every community extension ever published.

Functional parity means observable SQL, results/types/errors, catalog and
transaction behavior, durable files, configuration, resource behavior, APIs,
clients, and the extensions included in the agreed target. It does not require
copying C++ classes or algorithms. The rewrite's
[replaceable-subsystem requirements](../specs/rewrite-principles.md) still apply.
Alternative OLTP, graph, AI and non-DuckDB formats are separate expansion goals;
they should not silently enlarge this parity backlog.

Evaluation used the actual statement/query/table binders, type and function
registrations, catalog/default representation, optimizer, scheduler, storage and
WAL implementations, Cargo targets, public interfaces, upstream source inventory,
and historical findings. It did not rerun the entire upstream population, external
clients, platform matrix, or performance campaigns. Missing behavior identified
in source is distinguished below from incomplete coverage of implemented behavior.

The retained [source manifest](../test/upstream/duckdb/manifest.json) inventories
15,646 assets, 5,638 SQLLogicTest files, 1,506 native declarations, 561 Python
declarations, 132 Swift declarations, and 1,160 benchmark workloads. These are
source counts, not compiled/parameterized/platform instance counts. The most
recent full-upstream result found in the removed historical summaries was
487 passing files and 20,811 passing records at engine `27d190b`; successful
records include prefixes of failed files. That is about 8.6% of the inventoried
SQL files, **not 8.6% functional completion**, and is not current-source evidence.
Later scoped improvements cannot be added to that number without a fresh run.

The local chunk sweep passed six stages after the runner was added: format,
check, Clippy, ordinary tests, exhaustive recovery, and six maintained Kani
harnesses. Its 311.5-second run is a local regression result, not upstream parity.
The sweep's default Cargo feature selection does not cover every feature,
ignored test, doctest, external harness or platform. G01/G24 own those obligations.

## What already exists

| Area | Implemented foundation | Remaining boundary |
| --- | --- | --- |
| SQL | SELECT/VALUES, ordinary DML, schemas/tables, selected ALTER, transactions, EXPLAIN | Many statement families/modifiers and exact binding behavior |
| Relations | Ordinary outer/semi/anti joins, NATURAL/USING, subqueries, set operations, recursive UNION, grouping sets, core windows/QUALIFY | Lateral/ASOF/positional joins, richer CTEs, PIVOT/UNPIVOT, sampling, remaining frames and syntax |
| Values | Signed/unsigned integers, DECIMAL, floating, BLOB/UUID/ENUM/BIT/BIGNUM, temporal and nested families including VARIANT/TUPLE | Complete coercion/function coverage, named types, GEOMETRY, context-sensitive behavior |
| Expressions | Selected registered casts/operators/scalars/aggregates, scalar/batch evaluators, retained-expression and native-codec prerequisites | End-to-end retained defaults and complete function/overload semantics |
| State | Copy-on-write snapshots, rollback, basic uniqueness/NOT NULL, prepared rebinding, hash/B-tree indexes | DuckDB conflict timing, independent concurrent writers, catalog dependencies and incremental index maintenance |
| Native persistence | Selected storage versions 64–69, many native readers, selected WAL/checkpoint/recovery and crash tests | All objects/types/versions, compressed writing, large/partial I/O, concurrent maintenance and encryption |
| Execution | Pull/eager alternatives, selected joins/grouping/sorting and cancellation | Cost-based planning, byte accounting, buffer management, spilling, parallel/pending execution |
| Integration | Rust library and a small SQL shell | DuckDB C APIs, Arrow/ADBC, language clients, general external scans and extension loading |

Do not re-port unsigned/decimal/nested values, windows or NATURAL/USING from
scratch because an old report called them absent. Build on the existing source.
No broad subsystem in this table has evidence of full parity.

## How to assign this work

Each **Gxx group** is a goal to hand to an owning agent. Each **Gxx.n chunk** is
a smaller implementation outcome, with several concrete tasks. A group can span
many commits; it is not a promise of completion in one agent turn. If a chunk
exceeds one coherent change, split it again while retaining its parent ID.

For every assigned chunk:

1. Resolve the pinned source/tests for its precise behavior. Classify existing
   passing behavior, engine gaps, harness gaps and reference divergences.
2. Implement through the selected public contracts. Include names, types, NULLs,
   errors, effects, ownership and applicable configuration semantics.
3. Carry it through its actual consumers: scalar/batch, prepared use, transaction
   failure/rollback, persistence/reopen and other relevant APIs. A type declaration,
   parser branch or isolated codec is a prerequisite, not the complete outcome.
4. Run affected unchanged upstream cases and adversarial regressions; preserve
   known passing cases. Map non-SQL assertions explicitly to Rust contracts.
5. At the explicit chunk boundary, delegate `python3 scripts/verify_chunk.py` to
   the configured low-cost verifier. Follow [AGENTS.md](../AGENTS.md). Kani findings
   must be investigated/reported under the exploratory policy; proof success is
   not a substitute for ordinary correctness or upstream execution evidence.
6. Update this group's status with the source revision, tested population and
   remaining cases. Keep raw logs/JSON under ignored `target/`; report compactly.

One integration owner coordinates changes to `DataType`/`Value`, catalog identities,
bound expressions/plans, shared registries, `DatabaseBuilder`, transaction/storage
contracts and wire formats. Family owners propose focused changes to those files;
uncoordinated parallel edits to the same shared enums are not independent work.
Use separate worktrees. Do not mutate the pinned references or shared test workers
during another campaign. Performance measurements require a quiet host.

## Goal map and dependencies

Dependencies below name required capabilities, not a requirement to finish every
subtask of a large predecessor before beginning design or a usable increment.

| ID | Agent goal | Main prerequisites |
| --- | --- | --- |
| G01 | Reliable parity inventory and faithful harnesses | Existing source/test inventory |
| G02 | SQL parsing, binding, naming and diagnostics | Existing binder; coordinate G09/G10 |
| G03 | Scalar types, numeric coercion and math | G02 binding contracts |
| G04 | Temporal and timezone completeness | G03 conversion contracts; G10 settings; G23 ICU |
| G05 | Nested values, lambdas and core GEOMETRY | G02/G03; coordinate G12 codecs |
| G06 | Text, collations and utility functions | G02/G03; G23 ICU where required |
| G07 | Remaining relational SQL | G02; G05 for UNNEST/PIVOT values |
| G08 | Aggregate and window completeness | G03–G07 |
| G09 | Retained expressions and default lifecycle | Existing stored-expression prerequisites |
| G10 | Catalog objects, settings and attachments | G02/G09; coordinate G14 |
| G11 | Mutations, constraints and schema evolution | G09/G10/G14/G15 contracts |
| G12 | Native checkpoint/file compatibility | G03–G05/G09–G11 as types/objects land |
| G13 | WAL, recovery and checkpoint lifecycle | G12/G14 |
| G14 | Transaction and connection semantics | Existing snapshots; coordinate G10/G13 |
| G15 | Index DDL and selective/incremental access | G10/G14; coordinate G12 |
| G16 | Statistics, optimizer and physical planning | G07/G08/G15; later G17/G18 |
| G17 | Memory accounting, buffers and spilling | Storage/execution contracts |
| G18 | Parallel and pending execution | G14/G17 and operator state contracts |
| G19 | Table functions and external scan contracts | G02/G05; G17/G21 |
| G20 | CSV, JSON, Parquet and COPY | G19/G21; G05 nested values |
| G21 | Filesystems, secrets and encryption | G10/G12/G13; coordinate G23 |
| G22 | Embedding APIs, Arrow and ADBC | G02/G10/G14/G18/G19 |
| G23 | Extension runtime and required extension capabilities | G10/G19/G21/G22 |
| G24 | Clients, shell, distribution and full acceptance | Integrate all relevant groups |

Practical opening wave: run G01's inventory work alongside G09's retained-default
integration, G03/G04/G05 family completion, and G10's catalog identity prerequisite.
G02 can close independent syntax gaps. Start G17 and G19 contract design early.
Next integrate G07/G08/G11/G12/G14/G15, then resource execution and external data,
then complete public clients/extensions. G24 regression accounting runs throughout;
it is not a final attempt to discover all compatibility requirements at once.

## G01 — Reliable parity inventory and harnesses

**Current:** source assets are retained and a Python runner drives the Rust API;
native/client mappings and complete harness semantics remain open.

- **G01.1 Inventory the acceptance population.** Enumerate both pins' SQL files,
  native registrations, generated/parameterized cases, slow tests, configurations,
  platforms, client suites and configured external extensions. Record immutable
  case IDs and an owner group; distinguish declaration counts from executed cases.
- **G01.2 Finish SQLLogicTest semantics.** Implement remaining directives, extension
  requirements, external expected files, paths, loops, restart/named-connection and
  concurrent controls. Match reference numeric conversion/tolerance, regex, hashes,
  ordering and labels without changing assertions. Wrong-result harness tests must fail.
- **G01.3 Map native and client assertions.** Translate meaningful API/internal
  invariants into Rust contracts with source-case mappings; do not replace native
  lifetime or concurrency assertions with a SQL smoke query.
- **G01.4 Refresh and classify.** Run a complete current campaign with reproducible
  identities and sensible recorded deadlines. Separate engine/harness/setup failures,
  unsupported, skipped, timed out, crashed and unexecuted cases. Preserve regressions
  and partial prefixes without counting them as full-file passes.

**Exit:** every inventoried obligation has a mapped outcome/owner; the harness
cannot count unsupported expected-error records or omitted work as success.
Sources: `scripts/{run_upstream,sqllogic,upstream_suite}.py`, `test/runner/`,
[testing specifications](../specs/testing/README.md).

## G02 — Parsing, binding, namespaces and diagnostics

**Current:** the vendored sqlparser frontend and Rust binder support substantial
SQL, but many AST variants/modifiers end in explicit Unsupported.

- **G02.1 Complete syntax coverage.** Compare upstream statement/expression grammar
  with parser and binder handling; implement DuckDB-specific syntax and aliases.
  Reject unsupported modifiers explicitly instead of accepting and ignoring them.
- **G02.2 Complete scope rules.** Handle catalog/schema/table qualification, aliases,
  star EXCLUDE/REPLACE/RENAME and COLUMNS expressions, alias reuse, correlated names,
  struct-versus-qualified-column ambiguity and named/default function arguments.
- **G02.3 Complete binding context.** Preserve literal versus typed-constant versus
  parameter identity, common types, overload costs, qualified calls and dependencies.
  Add SQL PREPARE/EXECUTE/DEALLOCATE lifecycle where the API already has preparation.
- **G02.4 Match diagnostics.** Preserve error categories, query positions/lengths,
  candidate signatures and reference-required message content; keep missing engine
  capability distinct from a supported operation's legitimate rejection.

**Exit:** selected grammar families pass unchanged parser/binder tests with exact
names, schemas and error behavior, including prepared and nested expressions.
Sources: `src/parser/`, `src/planner/binder/`, upstream `src/parser/`,
`src/planner/binder/` and `test/sql/`.

## G03 — Scalar types, coercion and numeric functions

**Current:** signed/unsigned widths, DECIMAL, FLOAT/DOUBLE, BIT, BIGNUM, BLOB,
UUID and anonymous ENUM have implementations. Checked factorial and signed
BIGINT/HUGEINT GCD/LCM families, including aliases and selected casts, now work
through scalar/batch evaluation, mutations and reopen. A second numeric slice adds
the pinned trigonometric/hyperbolic, angle, exponential, cube-root, `even`, `pi`,
`signbit` and `nextafter` families, including strict-IEEE signed-NaN behavior. The
generated math tail now also includes power/log/square-root/bit-count, `gamma`,
`lgamma`, development-authoritative `binom`, exact FLOAT/DOUBLE `isnan`, and the
callable `**`, `^`, `!__postfix` and `@` aliases. Quoted and `main`-qualified scalar
calls share ordinary binding. SQL operator spelling for exponentiation/postfix
factorial, the conversion matrix and the remaining scalar-family catalog stay open.

- **G03.1 Close the conversion matrix.** Cover source/target types, literals,
  implicit/explicit/assignment/combination casts, overflow, rounding, textual forms,
  typed NULLs, mixed widths and decimal scale across every consumer.
- **G03.2 Complete numeric operations and catalog.** Inventory upstream overloads,
  aliases, constants, arithmetic/bitwise/math/rounding functions and BIGNUM/BIT
  operations. Extend existing registrations; verify return types and boundary errors.
- **G03.3 Complete scalar-family edges.** Finish binary/UUID/ENUM functions, coercion,
  formatting and batch-dependent behavior. Coordinate named types with G10 and
  remaining physical type/version/compression coverage with G12.
- **G03.4 Preserve context and mixed-family semantics.** Match IEEE-setting behavior
  for already prepared versus freshly bound expressions, lazy errors, unsigned keys,
  DECIMAL inside nested values, and scalar/batch results with retained adapters.

**Exit:** mapped family cases pass through SQL, parameters, joins/groups/order,
indexes, mutations and native reopen; a selected arithmetic matrix alone is insufficient.
Sources: `src/common/{types,numeric,scalar,bit,bignum}.rs`, `src/common/cast/`,
`src/function/scalar/numeric/`, upstream `extension/core_functions/`.

## G04 — Temporal and timezone completeness

**Current:** DATE, TIME/TIME_NS/TIMETZ, timestamp precisions/timezones and INTERVAL,
plus selected arithmetic, calendar difference/truncation/bucket/format functions.
`to_timestamp(DOUBLE)` retains ties-even microsecond rounding and its half-open
range; development's interval normalization carries and borrows with Euclidean,
saturating behavior. ISO/BCE-aware `era`, `isoyear`, `week`/`weekofyear`, `weekday`,
`yearweek` and `julian` extraction now works through named and generic aliases, with
pinned NULL-overload and invalid-specifier ordering. Core `timezone`,
`timezone_hour` and `timezone_minute` cover fixed-offset TIMETZ and zero-offset
DATE/TIME/TIMESTAMP behavior; unsafe offsets outside +/-15:59:59 are rejected
instead of constructing invalid physical values. Text parsing, transaction-time,
named-zone and ICU work remains open.

- **G04.1 Finish physical and textual domains.** Cover minima/maxima, infinities,
  fractional rounding, offset limits, precision loss, interval forms, native/API
  payloads and exact cast failures; retain the repaired full-width timestamp minimum.
- **G04.2 Finish calendar functions.** Inventory extraction, construction, formatting,
  parsing, date arithmetic, series and current-date/time functions; match constant
  versus column execution, NULLs, errors and transaction-stable time semantics.
- **G04.3 Add timezone/ICU behavior.** Provide timezone settings, named zones, DST
  gaps/folds, calendar configuration and timezone conversions. Provision matching
  ICU reference builds; the current core-only builds cannot validate that population.
- **G04.4 Integrate every path.** Exercise mixed temporal/nested values, prepared
  settings, casts/defaults, aggregates/windows, indexes and native WAL/checkpoints.

**Exit:** complete mapped temporal and ICU populations pass separately against the
required configurations; exact diagnostic differences stay visible.
Sources: `src/common/temporal/`, `src/function/temporal/`,
[temporal contracts](implementation-notes.md#temporal-and-numeric-behavior), upstream `extension/icu/`.

## G05 — Nested values, lambdas and core GEOMETRY

**Current:** LIST/ARRAY/STRUCT/MAP/UNION/VARIANT/TUPLE have selected value, function
and native paths. LIST/ARRAY slicing now implements pinned 1-based inclusive,
negative, omitted-bound and stride semantics through both evaluators and reopen;
omitted syntax is retained as binder provenance and cannot be forged by a user
empty-list expression. LIST/ARRAY contains/position/select/resize/reverse families
now preserve ARRAY-to-LIST results, pinned NULL demand and bounded allocation.
LIST/ARRAY `where` masks and variadic `zip` additionally preserve long-mask NULL
padding, truncate-to-shortest, heterogeneous tuple children and NULL-list behavior
through scalar/batched execution, Rust-origin retained defaults and materialized
native values from both producer directions. Rust intentionally rejects a sequence
or shallow zipped expansion above 16,777,216 logical children before allocation;
the pinned implementation has no
equivalent fixed ceiling. The shared multi-family `contains` name, higher-order/lambda
functions and the broader nested catalog remain open. Retained native constructors
qualified as `main.list_value` now resolve through the same bounded built-in lookup
as ordinary `main`-qualified calls. Core GEOMETRY is present in the pinned C++ type
enum and absent from Rust's built-in DataType enum; it is not solely a
spatial-extension question.

- **G05.1 Complete nested semantics.** Finish slicing, constructors/accessors,
  UNION promotion, STRUCT field combination, ARRAY shapes, MAP duplicates/lookup,
  VARIANT dynamic semantics, TUPLE behavior and formatting with child NULL/type identity.
- **G05.2 Add higher-order operations.** Implement lambda binding/captures and the
  list/array/map function catalog, including transforms, filters, reductions and
  applicable mathematical operations; preserve selected child adapters and effects.
- **G05.3 Finish relational and mutation consumers.** Cover recursive UNNEST,
  lateral use, aggregates, qualified paths, nested mutations, prepared values and
  full-width literal inference in mixed containers.
- **G05.4 Add missing logical families.** Implement core GEOMETRY/WKB/CRS behavior,
  applicable casts/operations and native metadata. Inventory TYPE and other exposed
  type-constructor/pseudo-type behavior; do not turn internal-only IDs into columns.
- **G05.5 Finish nested persistence.** Cover native child streams, shredded VARIANT,
  update paths, named child types, defaults and version restrictions with independent
  bidirectional files. Preserve nested IEEE bits and NULL-versus-active-NULL tags.

**Exit:** mapped nested and core geometry behavior works end to end; arbitrary
opaque payload storage is not equivalent to the reference type semantics.
Sources: `src/common/{nested,variant}.rs`, `src/function/nested/`,
`src/storage/duckdb/nested/`, upstream `src/include/duckdb/common/types.hpp`.

## G06 — Text, collations and utility functions

**Current:** selected scalar/string operations and LIKE exist; the full upstream
function catalog and collation system do not.

- **G06.1 Finish text functions.** Implement length/substrings/search/replace/split,
  Unicode case and normalization, formatting/padding, encodings and relevant aliases.
  Match character versus byte indexing and invalid-input behavior.
- **G06.2 Add regex and collation contracts.** Match regex options/results/errors;
  support built-in and ICU collations across comparisons, sorting, grouping, joins
  and index keys. A custom ASCII test type is not a production collation system.
- **G06.3 Complete utilities.** Inventory random/seed/UUID generation, hashing,
  conversion/introspection helpers and other core scalar functions. Declare volatility,
  transaction/statement stability and external effects in binding/execution.
- **G06.4 Publish coherent catalog metadata.** Ensure aliases, named parameters,
  overload descriptions and qualified function lookup match G10 introspection and
  G02 binding; test error/effect behavior under folding, CASE and TRY-like consumers.

**Exit:** source-enumerated function families have correct registrations and behavior,
including Unicode, NUL, empty input, NULLs and retained prepared bindings.
Sources: `src/function/`, upstream `extension/core_functions/scalar/`,
`src/function/scalar/`, `extension/icu/`.

## G07 — Remaining relational SQL

**Current:** ordinary joins/NATURAL/USING, basic set operations, scalar/EXISTS/IN
subqueries and recursive UNION are implemented. This is completion work, not a rewrite.

- **G07.1 Finish join and correlation forms.** Add lateral/dependent relations,
  ASOF and positional joins, remaining quantified/correlated subqueries and
  correlation through functions/aggregates, preserving empty/NULL/cardinality rules.
- **G07.2 Finish recursive and materialized CTEs.** Implement materialization controls,
  USING KEY/recurring relations, multiple references, recursive naming/type rules
  and cancellation/termination behavior.
- **G07.3 Complete relational syntax.** Finish DISTINCT ON, GROUP BY ALL, remaining
  SELECT/ORDER/LIMIT modifiers and set-operation alignment such as BY NAME where
  present in the pin; verify interactions with aliases and qualified columns.
- **G07.4 Add reshape and sampling.** Implement PIVOT/UNPIVOT and sampling forms,
  schema discovery, output naming, NULL handling and reproducibility settings.

**Exit:** each query family passes unchanged SQL cases and mixed prepared/nested/
transaction scenarios; neither a parsed AST nor a single join algorithm is enough.
Sources: `src/planner/binder/{query,table,recursive,subquery}.rs`,
`src/execution/operator/`, upstream query-node/table-reference tests.

## G08 — Aggregates and windows

**Current:** nine ordinary aggregate names and eleven dedicated window names are
registered in their built-in modules; grouping sets and core frames already work.

- **G08.1 Complete aggregate families.** Add ordered/list/string aggregates,
  statistical/regression/distribution functions, quantiles, approximate/sketch
  functions, arg-min/max and other source-registered families with exact return types.
- **G08.2 Finish aggregate modifiers.** Support applicable ORDER BY/DISTINCT/FILTER,
  multiargument signatures, empty groups, grouping masks and full grouping-set rules.
- **G08.3 Finish window semantics.** Add value-offset RANGE frames, remaining
  exclusion/dynamic-bound behavior and argument ordering. Inventory additional
  window functions from the pin and verify peers, ties, NULLs and invalid bounds.
- **G08.4 Integrate execution states.** Carry aggregate/window states through
  batching, parallel combination and spill contracts; preserve ownership, effects,
  cancellation and results under different partition/batch boundaries.

**Exit:** unchanged aggregate/window populations pass, including nested/decimal/
temporal inputs and bounded-resource execution where the reference supports it.
Sources: `src/function/{aggregate,window}.rs`, `src/planner/window.rs`,
`src/execution/operator/{aggregate,window}/`, upstream `extension/core_functions/aggregate/`.

## G09 — Retained expressions and default lifecycle

**Status: scoped retained-default lifecycle complete.** Catalog columns and private
snapshots retain optional `StoredExpression` trees, including absence, declared
literal type and syntax provenance. SQL CREATE/SET/ADD captures and binds closed
defaults without executing during capture or binding; INSERT and demanded ADD
backfill evaluate later at their documented demand points. Omitted
INSERT defaults run column-major over each source batch requested with the fixed
2,048-row DuckDB maximum; a short natural child batch remains a boundary, while the
full row set remains staged so failure publishes no partial insert. ADD follows
the pinned development behavior where the references diverge: a constant or one non-TRY
cast around a constant is simple, while v1.5.5 treats only the bare constant as
simple. Non-simple ADD evaluates visible rows only; simple ADD evaluates retained
physical slots, including deleted slots, until checkpoint reclamation.

Physical order follows regular versus relocating updates: indexed-column and
unsupported nested updates retain a deleted old slot and append the replacement.
Successful manual and automatic checkpoints reclaim only the current acknowledged
snapshot; failed checkpoints leave slots, generation and the durable image unchanged,
and older snapshots retain their holes.
Native checkpoint/WAL expression codecs preserve selected function/default metadata,
including absent versus explicit `DEFAULT NULL`, with bidirectional FUNCTION, CASE,
predicate and interval acceptance against the pinned development process. Both release
checkpoint directions pass independently; its two Rust-origin WAL cases containing
FUNCTION nodes retain the known upstream startup failure.

- **G09.1 Retain catalog expressions — implemented.** Optional owned expressions
  now retain declared types, aliases/argument provenance, qualification, operators
  and source spans through catalog alteration and private snapshot round trips.
  Closed CASE, NULL-test, BETWEEN, IN, LIKE and interval defaults share ordinary
  binding; conditional ADD no-ops are resolved before type/default binding.
- **G09.2 Connect DDL and evaluation demand — implemented.** Capture
  and bind CREATE/SET/ADD defaults without executing during those stages. Evaluate
  omitted INSERT values column-major per source vector while interleaving source
  effects and preserving short child-batch boundaries. Resolve demanded ADD backfill
  once in the applicable live-only or retained-physical demand order and reuse it
  across catalog-basis, current-snapshot and WAL paths.
- **G09.3 Connect native serialization — implemented representable scope.** Integrate
  parsed/value codecs, unresolved/named type binding, private format, native
  checkpoints and WAL. Selected FUNCTION, CASE, predicate and interval defaults plus
  absence/explicit-NULL metadata pass independent exchange. Reject unrepresentable
  legacy argument provenance.
- **G09.4 Preserve lifetime/effect semantics — implemented for closed defaults.** Closed
  retained binding permits volatile/external effects. Prepared omitted INSERT uses
  execution-time settings; value versus selection predicate demand matches the pinned
  execution modes. Tests cover source/vector boundaries, failed-statement atomicity,
  CREATE/SET rollback, old catalog snapshots, update relocation, manual/automatic
  reclamation, reopen and deferred error timing. Built-in current-date/time and
  timezone/calendar defaults remain with G04. Real sequences/`nextval`, catalog
  function/object identity, search-path resolution, dependencies and prepared
  invalidation remain with G10. DML-level explicit `DEFAULT`, CHECK constraints and
  generated columns remain with G11. Those are separate consumer scopes and are not
  claimed by this G09 exit.

**Exit achieved for this slice:** the checked-in development fixture covers FUNCTION
checkpoint input, and the independent process gate passes FUNCTION/CASE/predicate
and interval checkpoint exchange in both directions plus Rust-origin WAL recovery. A
one-time evaluated literal or codec-only round trip cannot pass. Release checkpoint
exchange passes separately from its documented upstream FUNCTION-node WAL startup limit.
Sources: `src/catalog/{mod,expression}.rs`, `src/planner/binder/stored.rs`,
`src/storage/duckdb/{parsed,value}/`, [implementation notes](implementation-notes.md#retained-defaults).

## G10 — Catalog objects, settings and attachments

**Current:** the catalog exposes schemas/tables; TableName has schema/name only.
Runtime catalog/object IDs now separate stable object identity from observed catalog
version, legacy adapters fail closed at identity-aware boundaries, a bidirectional
checked dependency graph plans deterministic RESTRICT/CASCADE order, and a pure
search-path model matches pinned parsing and implicit lookup order. These contracts
are now adopted by snapshot schema/table DDL and transaction publication. Runtime
registries rebuild fresh non-wire identities on reopen, enforce table-to-schema
dependencies, preserve identity across rename, reject drop/recreate replacements and
reuse one prepared insertion identity across transaction-current/catalog-basis views.
SQL binding and logical/physical table plans now retain these identities. Query/DML
plans require their exact observed catalog version at logical validation and physical
scan open; stable ALTER/DROP follows rename, rejects replacements, and preserves
`IF EXISTS` without classifying arbitrary catalog failures as absence. Alternative
frontends can resolve the same handles through `Connection::resolve_table`. Prepared
API statements still retain syntax and rebind on each execution, and the pure search
path is not yet connected to session resolution. Three built-in setting definitions
exist; there is no general catalog object model.

- **G10.1 Establish identity and dependencies.** Add catalog/object IDs, search paths,
  dependency tracking, invalidation, temporary object scope and transaction visibility.
  Preserve prepared object lifetimes and reference rename/drop/cascade rules.
- **G10.2 Implement object families.** Add views, scalar/table macros, sequences,
  named ENUM/user-defined types, aliases and applicable newer object families such
  as triggers from the pinned source. Include DDL, binding and persistence lifecycle.
- **G10.3 Add attachments and routing.** Implement ATTACH/DETACH/USE and qualified
  access to multiple catalogs, read-only modes, storage extensions and connection
  shutdown. Match cross-database write restrictions instead of promising atomicity
  merely because two stores can be attached.
- **G10.4 Complete metadata and configuration.** Implement duckdb_* tables/functions,
  information_schema/compatibility catalogs, SHOW/DESCRIBE, settings/PRAGMAs,
  SET/RESET/variables and scope/locking rules. Include comments and object metadata.

**Exit:** objects behave consistently under session scope, DDL rollback, dependencies,
prepared reuse and native reopen; catalog listings reflect the actual implementation.
Sources: `src/catalog/`, `src/main/settings/`, upstream `src/catalog/`,
`src/function/table/system/`, `src/main/settings/` and `test/sql/trigger/`.

## G11 — DML, constraints and schema evolution

**Current:** ordinary INSERT/UPDATE/DELETE, primary/unique/NOT NULL and selected
ALTER exist; conflict/returning/joined DML and many CREATE/ALTER options are rejected.

- **G11.1 Complete mutations.** Add RETURNING, INSERT conflict/replacement forms,
  BY NAME and DEFAULT expressions, UPDATE FROM/tuple assignments, DELETE USING,
  MERGE and other pinned mutation forms. Preserve changed-row and result metadata.
- **G11.2 Complete constraints.** Implement CHECK, foreign keys, generated columns,
  applicable constraint DDL and dependency rules; handle multirow/self-referential
  statements, NULLs and statement-atomic failure according to the source.
- **G11.3 Finish schema changes.** Add ALTER TYPE/USING, nested-field changes and
  remaining object/column modifiers; coordinate indexes, defaults, dependent objects,
  old snapshots and catalog conflict timing.
- **G11.4 Verify durable and concurrent behavior.** Exercise prepared mutation,
  rollback, concurrent changes, native serialization and WAL replay for each new
  form. Do not treat in-memory DDL success as durable compatibility.

**Exit:** unchanged DML/constraint/ALTER cases preserve all rows and metadata on
failure, with matching error timing and visibility.
Sources: `src/planner/binder/{statement,alter}.rs`, `src/catalog/alter.rs`,
`src/storage/table/alter.rs`, upstream `src/planner/binder/statement/`.

## G12 — Native files and checkpoint compatibility

**Current:** substantial native reading and selected versioned writing exist,
including nested/VARIANT/temporal values. Thirteen decoder registrations exist;
the writer does not provide corresponding general compressed-write selection.

- **G12.1 Complete format/version metadata.** Map supported historical/current
  versions, headers, catalog objects, types, constraints, defaults, indexes,
  statistics and layout changes. Track read, create, rewrite and upgrade separately.
- **G12.2 Finish compression and encoding.** Map every pinned codec/version/type
  combination; implement missing readers and required encoder/selection behavior,
  compressed updates and nested validity/string layouts. Compare actual values.
- **G12.3 Add selective and scalable storage.** Connect block/row-group access,
  partial scans, large values/files, compaction/vacuum and free-block reclamation to
  buffer/resource contracts. Remove artificial size ceilings only with bounded I/O.
- **G12.4 Prove interoperability by direction.** Read independently produced files,
  write files read and mutated by C++, then read those mutations in Rust. Include
  mixed types, deleted row IDs, endian/platform cases, corruption and version rejection.

**Exit:** required format populations pass bidirectionally with unchanged-file
guarantees on rejected reads/writes. A private snapshot round trip is not this gate.
Sources: `src/storage/duckdb/`, upstream `src/storage/`,
[storage specifications](../specs/components/storage.md).

## G13 — WAL, recovery and checkpoint lifecycle

**Current:** native WAL v2, selected DML/ALTER/nested recovery, logged commits,
checkpoint policies and process interruption tests exist. WAL v1 is explicitly rejected.

- **G13.1 Complete record and version coverage.** Map all required WAL record/object/
  type families, nested update paths and version transitions; preserve atomic FLUSH
  boundaries, index/default metadata and committed-versus-aborted changes.
- **G13.2 Complete maintenance lifecycle.** Match automatic/manual/forced/concurrent
  checkpoints, sidecar reconciliation, truncation, restart and repeated recovery.
  Extend the selected physical-order reclamation contract to remaining concurrent,
  compaction and vacuum histories without changing default/backfill demand.
- **G13.3 Verify publication failures.** Extend truncation/corruption/process-kill
  tests to new operations, short writes, sync/rename failures and compound faults.
  Distinguish definite failure from unknown commit and safe recovery-required states.
- **G13.4 Exercise reference handoffs.** Alternate readers/writers and checkpoint/
  recovery owners across both pins, including continued writes after recovery,
  encrypted forms once G21 lands and backward-compatibility obligations from G01.

**Exit:** all acknowledged commits survive required failure histories and incomplete
commits are invisible, with compatible files and errors after restart.
Sources: `src/storage/{logged,recovery,log}.rs`, `src/storage/duckdb/wal/`,
`test/component/{recovery,logging,checkpointing}/` and companion test files.

## G14 — Transactions and connection lifecycle

**Current:** optimistic snapshot transactions conflict with every intervening
writer, even disjoint writes. That is observably different from DuckDB concurrency.

- **G14.1 Match visibility and conflict domains.** Implement required row/catalog
  version visibility, concurrent disjoint updates, conflict timing and read/write
  transaction modes; retain old snapshots and safe version reclamation.
- **G14.2 Match failure and statement lifecycle.** Define bind/execution/commit error
  transitions, autocommit over multiple statements, implicit transactions, rollback,
  read-only mode, prepared reuse and cancellation exactly as in the reference.
- **G14.3 Match database-instance/connection ownership.** Handle repeated opens,
  shared instances, locks, active results, close/reopen and multiple catalogs.
  Coordinate with G22 foreign handles and G18 pending execution.
- **G14.4 Validate histories.** Run deterministic overlapping read/write/DDL/index/
  checkpoint histories, resource failure and process tests; check outcomes against
  the source rather than accepting any serializable outcome.

**Exit:** mapped transaction/API histories have the same allowed results and errors;
stronger blanket rejection is not full functional parity.
Sources: `src/transaction/`, `src/main/connection.rs`, `src/storage/table/`,
upstream `src/transaction/`, `test/sql/transactions/`.

## G15 — Index DDL, maintenance and access

**Current:** selectable hash/B-tree equality indexes and native ART output exist.
Mutations rebuild runtime indexes; SQL index DDL and range access remain open.

- **G15.1 Add index catalog/DDL.** Implement create/drop/names, unique/composite/
  expression definitions and persistence/dependency metadata through G10/G12.
- **G15.2 Implement incremental maintenance.** Update index state transactionally
  for insert/delete/update, schema changes and rollback; retain uniqueness semantics
  for NULL, NaN, collated and nested keys.
- **G15.3 Add selective access.** Define range/batched/gather access and planner
  eligibility, residual predicates and statistics. Distinguish selecting fewer rows
  from actually reading fewer file blocks.
- **G15.4 Finish native index compatibility.** Verify reference index visibility/use,
  rebuild/reopen, corruption and mutation behavior; add extension index contracts
  only for the extension scope selected in G01/G23.

**Exit:** SQL-defined indexes work across transactions/native files and eligible
access produces correct rows without changing errors or effects.
Sources: `src/execution/index/`, `src/optimizer/key_lookup.rs`,
`src/storage/duckdb/writer/index.rs`, upstream `src/execution/index/art/`.

## G16 — Statistics, optimization and physical planning

**Current:** the default pipeline has expression simplification, equality lookup
and conservative EXISTS decorrelation. There is no general cost model.

- **G16.1 Establish statistics lifecycle.** Implement ANALYZE and required table/
  column statistics, propagation, invalidation and cardinality estimates.
- **G16.2 Add relational transformations.** Expand safe filter/projection/limit
  pushdown, decorrelation, join ordering, common expression/subplan reuse and set/
  aggregate/window rewrites, preserving error and effect semantics.
- **G16.3 Add cost-based physical selection.** Select scan/index, join, aggregation,
  sorting/Top-N and external algorithms using explicit capabilities, statistics and
  resources. Support relevant optimization controls and comparable plans.
- **G16.4 Complete EXPLAIN/profiling.** Implement required plan forms, EXPLAIN ANALYZE,
  estimates versus actuals, query metrics and settings with reference-facing output
  contracts. Development tracing is separate tooling.

**Exit:** optimizer-on/off results agree for supported cases, observable controls
work, and measured planning/execution regressions are addressed. C++ algorithm
identity is unnecessary; performance remains a separate acceptance dimension.
Sources: `src/optimizer/`, `src/execution/physical_plan.rs`, upstream
`src/optimizer/`, [optimizer spec](../specs/components/optimizer.md).

## G17 — Memory, buffers and spilling

**Current:** QueryContext accounts intermediate rows. There is no byte allocator,
buffer pool, spill policy or complete query memory ownership model.

- **G17.1 Account actual resources.** Add query/database byte budgets, ownership,
  reservations and fallible allocation across values, vectors, operator states,
  catalog work and external scans; wire memory/temp limits into settings.
- **G17.2 Add buffer management.** Implement block pin/unpin, eviction, dirty state,
  cache lifetime and I/O attribution, integrating file format and transaction needs.
- **G17.3 Implement external algorithms.** Spill/reload joins, grouping, sorting,
  windows and intermediates under bounded memory; preserve ordering/equality/type
  metadata and cleanup on error, early consumer stop and process restart.
- **G17.4 Exercise limits.** Test datasets exceeding memory, long/nested values,
  low-budget concurrency, allocation/disk-full failures and encrypted temporary
  files where required. Validate peak bytes and I/O, not only row counts.

**Exit:** reference-supported workloads succeed under comparable limits or fail
with matching resource semantics; raising arbitrary limits is not the solution.
Sources: `src/parallel/mod.rs`, `src/common/vector.rs`, `src/storage/`,
upstream `src/storage/buffer/`, `src/include/duckdb/storage/buffer_manager.hpp`.

## G18 — Parallel scheduling and pending execution

**Current:** InlineScheduler synchronously runs one task; stream adapters do not
establish a parallel/pending query engine.

- **G18.1 Define scheduling state.** Add task pools, pipeline dependencies, local/
  global operator state and exactly-once scheduling with thread settings.
- **G18.2 Parallelize operators.** Partition scans, joins, aggregation, sorting and
  format reads/writes; combine/finalize states while preserving observable ordering,
  mutation atomicity and transaction snapshots.
- **G18.3 Implement asynchronous progress.** Add pending result/readiness/backpressure,
  cancellation/timeouts, query close and safe cleanup. Connect public wait/step APIs
  through G22 rather than spinning synchronously behind an asynchronous name.
- **G18.4 Test scheduling histories.** Exercise one/many threads, blocked tasks,
  resource pressure, cancellation races, multiple connections and configuration
  changes; map upstream stress and platform cases.

**Exit:** concurrent/pending APIs and configured parallel work preserve reference
semantics, with no lost/replayed rows or leaked work after cancellation.
Sources: `src/parallel/mod.rs`, `src/execution/stream.rs`, upstream `src/parallel/`.

## G19 — Table functions and multi-file scan contracts

**Current:** table binding special-cases integer `range`/`generate_series`.
There is no general table-function/external-format lifecycle.

- **G19.1 Register and bind table functions.** Add signatures, named/default options,
  schema discovery, bind data, local/global state and ownership; integrate correlated
  arguments, replacement scans and registration through APIs/extensions.
- **G19.2 Define source capabilities.** Support projection/filter/limit pushdown,
  residual filters, statistics, virtual columns and streaming batches. Preserve
  logical column identities through physical projection and type conversion.
- **G19.3 Implement multi-file behavior.** Expand lists/globs, schema union/by-name,
  partition columns, filename/row identifiers, sampling and reader reopening.
  External file snapshots must not be inferred from native-table MVCC.
- **G19.4 Port core table functions.** Inventory metadata, range variants, UNNEST,
  test_all_types and other built-ins; connect dependencies to their actual owners.

**Exit:** a new source can bind/scan through ordinary contracts and public consumers
with correct schemas, pushdown residuals, lifetime and failure behavior.
Sources: `src/planner/binder/table.rs`, `src/storage/scan.rs`, upstream
`src/function/table/`, `src/common/multi_file/`.

## G20 — CSV, JSON, Parquet and COPY

**Current:** native DuckDB/snapshot formats are implemented; general SQL external
read/write support is absent. Using serde_json internally is not the JSON extension.

- **G20.1 CSV.** Port read/write, sniffing/schema options, quoted/buffer boundaries,
  encodings/compression, NULL/error/reject handling and parallel records.
- **G20.2 JSON.** Port JSON SQL functions plus readers/writers, paths/transforms,
  schema inference and strict/lax behavior. Preserve JSON null, SQL NULL, missing
  fields, numeric domains and mixed/nested records.
- **G20.3 Parquet.** Implement interoperable read/write metadata, pages/encodings,
  compression, decimals/temporal/nested mappings, statistics/pushdown and required
  encryption/logical annotations. Verify files with independent engines.
- **G20.4 COPY/import/export.** Implement options, partitioned/batched/multi-file
  writes, output finalization, cleanup and database import/export/copy lifecycle.
  Include text/blob readers and any additional inventoried core format entry points.

**Exit:** unchanged format suites and cross-engine files pass, including malformed
data, partial outputs, low memory, schema variation and exactly-once parallel scans.
Sources: upstream `src/execution/operator/csv_scanner/`, `extension/{json,parquet}/`,
[format spec](../specs/components/file-formats.md).

## G21 — Filesystems, secrets and encryption

**Current:** native publication has a local filesystem adapter and fault boundaries;
there is no general routed filesystem, secret manager or encrypted database engine.

- **G21.1 Filesystem capabilities.** Add routed filesystems, random/sequential/range
  reads, globs, compression, metadata/cache invalidation, lock/durability behavior,
  cancellation and remote retries without replaying non-idempotent writes.
- **G21.2 Secret and external-access policy.** Implement providers, scope selection,
  temporary/persistent/transaction lifetimes, redaction and configuration controls.
  Connect extension/file access to the selected policy.
- **G21.3 Encryption.** Match required database headers/keys/ciphers, blocks, WAL,
  checkpoint/recovery and temporary/output encryption. Distinguish wrong keys,
  unsupported versions and authenticated corruption.
- **G21.4 Remote integrations.** Implement required HTTP/object-store behavior or
  compatible extension adapters, then test credentials, redirects, ranges, network
  failure, stale objects and actual bytes transferred. External services are explicit
  test prerequisites, never silent omissions.

**Exit:** inventoried local/remote/encrypted workloads preserve data, policy and
failure behavior across the required operating systems.
Sources: `src/storage/filesystem/`, upstream `src/common/file_system.cpp`,
`src/main/secret/`, [security spec](../specs/components/security.md).

## G22 — Embedding APIs, Arrow and ADBC

**Current:** the Rust library is usable, but Cargo does not expose DuckDB's C ABI
or a complete DuckDB-compatible client/interchange surface.

- **G22.1 C API v1.** Implement exported symbols, configuration/open/connect/query,
  prepared values/results, logical types/vectors/chunks, appender and registration
  callbacks. Match ABI layouts, allocation/free rules, errors and ownership.
- **G22.2 C API v2 and wrapper contracts.** Implement the pinned environment/cache,
  handle/result state machines, wait/step/cancel and the consumer C++ wrapper-facing
  contract. Distinguish WAITING/CHUNK/FINISHED/CANCELLED from errors and internal states.
- **G22.3 Relations and bulk interfaces.** Add deferred relation composition,
  appender buffering/flush/close, replacement scans and user registration. Verify
  early destruction, transaction visibility and nested values through foreign callers.
- **G22.4 Arrow.** Implement import/export and stream schemas, buffers, release
  callbacks, nested/dictionary/extension types, offsets/NULLs and cancellation;
  connect Arrow scans through G19 and validate actual ownership lifetimes.
- **G22.5 ADBC.** Implement required driver database/connection/statement metadata,
  parameter binding, transactions, streaming and error/status contracts against the
  selected external ADBC tests and version.

**Exit:** mapped API/ABI/interchange tests run against the Rust-built artifact,
including failures and owner destruction; linking an installed C++ library is not a pass.
Sources: upstream `api_spec/{v1,v2}/`, `src/main/capi/`, `tools/cpp/`,
[API spec](../specs/components/apis.md), [Arrow/ADBC spec](../specs/components/arrow-adbc.md).

## G23 — Extensions and their capabilities

**Current:** Rust function/type registries are implementation seams, not a binary
extension loader or compatible extension distribution system.

- **G23.1 Freeze the compatibility population early.** Inventory configured in-tree
  and external extension pins/builds/tests. Separate stable C tables, unstable C
  tables, the C++ wrapper over C v2 and extensions coupled to internal C++ classes.
- **G23.2 Implement loader lifecycle.** Add INSTALL/LOAD/update, repository metadata,
  version/platform/signature checks, disabled/autoload/autoinstall policies, repeat
  loading and retained callback/library ownership.
- **G23.3 Port built-in extension behavior.** Integrate core_functions, JSON, Parquet,
  ICU, autocomplete and TPC-H/TPC-DS generators through their owning groups; exercise
  static and loadable forms where the target requires them.
- **G23.4 Port configured external capabilities.** Assign concrete child goals for
  cloud filesystems, lake formats/catalogs, scanners, spatial/search/vector and other
  inventoried extensions, each with pinned tests and dependencies. Their full remote
  implementations were not audited here and remain explicitly unassessed obligations.

**Exit:** the selected extension population works through documented compatible
interfaces. A Rust trait cannot load an arbitrary internal-C++ binary. For those
extensions, either reimplement/port the capability or explicitly resolve a different
compatibility target; do not claim existing-binary compatibility by assumption.
Sources: upstream `src/main/extension/`, `.github/config/`, `api_spec/VERSIONING.md`,
[extension spec](../specs/components/extensions.md).

## G24 — Clients, shell, distribution and acceptance

**Current:** a small SQL CLI and local Rust tests exist. Full client/tooling,
platform and performance parity have not been demonstrated.

- **G24.1 Client behavior.** Port/adapt pinned Python, Swift, C/C++ and inventoried
  external clients (for example JDBC/R/Node/Wasm integrations where selected).
  Exercise conversions, relation/dataframe APIs, transactions, threading, errors
  and artifact provenance; account for sources outside this checkout explicitly.
- **G24.2 Shell and user tooling.** Match required arguments/dot commands, interactive
  editing/history/completion, renderers/output modes, import/export, metadata,
  progress/profiling, exit codes and interruption. Treat any separately selected UI
  package as its own pinned population.
- **G24.3 Build and release matrix.** Produce shared/static libraries and clients,
  packaging/install paths and required OS/architecture/configuration combinations;
  verify ABI symbols and that every test loads the newly built Rust implementation.
- **G24.4 Cross-cutting correctness.** Continuously run full mapped upstream/feature/
  configuration/slow/client suites, fuzz/malformed input, allocation and persistence
  faults, concurrency histories, coverage and mutation checks. Explain all ignored
  cases and zero-test runs. Missing dependencies remain incomplete evidence.
- **G24.5 Separate performance acceptance.** Measure planning, execution, cold/warm
  storage, durable commits/recovery, concurrency, APIs, clients, CPU, memory and I/O.
  Use equivalent correct workloads and the faster of the two pinned references;
  the existing <=1.0 ratio rule remains binding. Thirty-four historically passing
  microbenchmarks do not establish general performance parity.

**Exit:** every required population is accounted for and passing under the acceptance
policy. Publish functional, file/API/extension compatibility and performance results
separately. A full Rust test sweep or six passing Kani proofs cannot replace them.
Sources: `tools/shell/main.rs`, upstream `tools/`, `.github/workflows/`,
[client testing](../specs/testing/clients.md), [acceptance](../specs/testing/parity.md).

## Handoff template

```text
Goal: Gxx — <goal title from this backlog>
Baseline: <integrated Rust commit>; pinned development and release from reference-builds.md
Owned chunks: Gxx.1 ... Gxx.n
Dependencies: <specific capabilities; integration owner for shared files>
Deliver: implemented behavior, unchanged upstream cases/source mappings,
         relevant cross-consumer regressions, chunk-sweep result, remaining gaps.
Constraints: selected subsystem interfaces; development wins semantic disagreements;
             no skips relabeled as passes; raw evidence only in ignored target/.
Completion: satisfy this group's exit criteria and the per-chunk contract above.
```

## Documentation maintenance

This backlog replaces the historical parity/progress/checkpoint summaries removed
in the same cleanup. Their last pre-cleanup tracked version is recoverable at
`20c8214`, including `docs/value-expression-progress.md`, `docs/testing-parity.md`
and family reports. Use `git show 20c8214:docs/<old-name>.md` for a specific historical
question; do not automatically reload that history into every agent context.

Keep [architecture](architecture.md) as the concise implementation map,
[implementation notes](implementation-notes.md) for difficult contracts,
[reference builds](reference-builds.md), [tracing](dev-tracing.md), and
[adversarial testing](sqlite-testing-review.md) as runbooks. Normative requirements
and detailed source specifications belong in `specs/`. A source-system specification
is not an assertion that its Rust counterpart is implemented. Update this work map
in place; avoid copying milestone status into multiple documents.
