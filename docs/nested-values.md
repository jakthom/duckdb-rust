# Nested values: implementation record

This is an in-progress part of the [value-and-expression milestone](../specs/value-expression-milestone.md), not a completed family or parity claim.

Latest lead integration: ROARING, DICT_FSST and EMPTY_VALIDITY now clear all six
original native nested fixtures. Their exact-value tests no longer skip either
development child codec. Additional independent fixtures cover all DICT_FSST
modes and ROARING forms, with mutations, rollback and reopen. The
[third integrated checkpoint](value-expression-progress.md) records the combined
tests, upstream regression repairs, Kani findings and 34-workload performance
preservation. Earlier codec failures below are retained historical evidence;
nested WAL/defaults and fuller VARIANT/TUPLE behavior still remain open.

## First integration increment

The provisional model shares recursive metadata and scalar payloads behind `Arc` while keeping the outer `DataType` at at most 16 bytes and `Value` at at most 32 bytes. LIST, ARRAY, STRUCT, MAP, UNION and VARIANT have distinct metadata/payload shapes. Shape validation checks child physical types, ARRAY cardinality, map non-NULL keys and UNION tag bounds. The selected type registry additionally validates child adapters, duplicate MAP keys and metadata limits. The registry binds child comparisons and keys once; changing the execution context's registry does not select new child implementations.

Implemented SQL paths include LIST literals and `list_value`, `array_value`, STRUCT dictionary literals, LIST subscripts, STRUCT field/subscript extraction, `list_extract`/`array_extract`, `struct_extract`, `map`, declared LIST/ARRAY/STRUCT/MAP/UNION types, and recursive LIST/ARRAY/STRUCT/MAP casts. STRUCT casts match target fields by name and fill absent target fields with NULL. Nested child NULL compares greater than a non-NULL child and equals another child NULL; container NULL remains the consuming operator's responsibility. MAP equality and order retain entry order. Canonical keys frame every child, including NULL, through its retained adapter.

The first four component tests exercise:

- constructors, decimal children, array shape failures, duplicate/NULL MAP keys, failed field access and nested comparison;
- nested values through joins, grouping, DISTINCT, min/max, sorting, partitioned windows and SQL UNION;
- child/container NULL, canonical keys, selected dictionary vectors and compact outer representations;
- a STRUCT containing DECIMAL, DATE and LIST(UBIGINT) through insert, update/delete rollback, prepared payload extraction, a JSON checkpoint and reopen.

`cargo check`, `cargo test --test nested` (4 passed), and `cargo clippy --workspace --all-targets -- -D warnings` pass for this increment. The full workspace suite passed with two existing ignored external-CLI analytics tests. A subsequent physical-depth guard rejects 66 nested VARIANT containers, and composite coercion ranking now checks availability through the selected child cast registry; focused nested/type/cast/operator tests passed again after those changes (4/12/8/8). Instrumentation coverage reports 220 files, 1,927 functions, 199 interface methods and no missing attributes. The latest trace check passed in 26.44 seconds and deleted temporary telemetry. Integrated Kani evidence is pending the first combined family checkpoint; these commits are internal steps, not completion of the nested assignment. No acceptance benchmarks have been run for these changes.

## Reference observations

Lightweight probes use pinned development `99063af2bd7092aff02e14184a20e24699d34d71`:

- `[1,NULL] > [1,2]` and `[1,NULL] = [1,NULL]` are true.
- MAP entry reordering changes equality, even when the same key/value pairs occur.
- `{'a':1,'b':2}::STRUCT(b BIGINT,c VARCHAR)` produces `{'b': 2, 'c': NULL}`; a STRUCT cast with no common field fails binding.
- `union_value(i:=NULL)` is a non-NULL UNION with a NULL active member. Its tag is the anonymous enum of declared member names.
- Development additionally introduces TUPLE identity; its older-file representation uses an unnamed STRUCT. This remains within the nested work inventory.

These observations guide implementation; they are not a full differential campaign.

## Second integration increment

UNION now has a named `union_value` constructor, field extraction, widening by declared member name, scalar-to-member selection through the selected child cast registry, and ambiguity rejection. A typed NULL cast such as `(NULL::INTEGER)::UNION(i INTEGER,s VARCHAR)` creates the active `i` member with a NULL child; untyped `NULL::UNION(...)` stays a NULL container. This required the lead's opt-in cast NULL-handling contract, with scalar and column/batch SQL regressions. `struct_pack`, bracket/field access, `list`/`array_agg`, aggregate-empty NULL results, and list window frames are exercised by the fifth nested test. All nested families explicitly decline PK/UNIQUE index admissibility while retaining equality, sorting and grouping semantics.

After incorporating the first combined temporal/BLOB/UUID base, ordinary check, nested tests (5), binary-scalar tests (4), temporal tests (2) and clippy passed. After the NULL-cast integration, nested (5), type (13) and cast (8) tests and clippy passed again. This is another integrable increment, not completion of UNION, the function catalog, or the nested milestone.

## Native checkpoint integration increment

LIST, ARRAY, STRUCT, MAP and UNION now serialize native child metadata, statistics,
validity and column streams. Native reads preserve active-NULL UNION members,
NULL containers, empty lists/maps, fixed array cardinality and decimal children.
Primitive codec selection remains through the supplied decoder registry. The
shared reader also consumes bounded development string statistics (fields
204–207) while preserving legacy statistics decoding.

Independent pinned release and development fixtures are retained under
`test/data/duckdb/nested-{release,development}/`, with producer identity, SQL,
hashes and observed codecs. Four of six files pass exact selected-value reads;
the release 125,013-row file also passes Rust publication and exact reopen. The
10,013-row development fixture remains unsupported because its VARCHAR child
uses DICT_FSST codec 15; the development 125,013-row fixture uses unsupported
ROARING codec 13 for child validity. A specifically named negative regression
records these gaps; it is not a compatibility pass. Both original failed fixtures
are retained unchanged for decoder follow-ups.

The first large-file run found an incorrect child coordinate assumption:
C++ applies the enclosing row-group origin recursively to all segment identities,
including LIST/ARRAY children, while list endpoints remain local. Both reader
and writer were repaired, and the multi-row-group fixtures cover the regression.
A selected three-row release-producer file also passed Rust update/insert,
Rust reopen, and independent reopen by both pinned C++ CLIs.

This is an internal storage increment, not completion of the family assignment.
Nested WAL, native non-NULL defaults, dynamic VARIANT/TUPLE persistence, the
remaining string/validity decoders and adversarial/resource expansion remain open. No
acceptance performance measurements were taken. Kani is run by the integration
lead at combined checkpoints; the first combined checkpoint reported five of
five maintained harnesses passing, with bounded coverage rather than full parity.

For this checkpoint increment, ordinary check, nested tests (8, including one
explicit unsupported-codec test), existing compatibility tests (14), the focused
packed-string-statistics unit test and all-target clippy passed. Coverage reports
241 files, 2,088 functions, 201 interface methods and no missing attributes.
The final instrumentation check passed in 28.01 seconds with zero reported errors,
panics or open spans; temporary telemetry was deleted. An earlier overlapping
trace/check attempt returned nonzero despite a completed compiler invocation;
the stable retry is the executed passing instrumentation result.

## Remaining work (continuing assignment)

### ROARING child-validity follow-up

The selected ROARING decoder now supports native codec 13 for validity and
BOOLEAN values. It validates aligned data/metadata boundaries, packed container
metadata, sparse and inverted arrays, short/compressed NULL runs, bitsets,
ordered indices and bounded partial tails. Development may allocate a full
2,048-bit final bitset despite a smaller logical cardinality; that unused tail is
accepted without producing extra rows. C++ short terminal runs can similarly
contain one unobserved invalid bit. Those reference layouts are exercised rather
than rejected as malformed.

The previously failing development 125,013-row fixture now passes every selected
value, Rust publication and exact reopen. Two additional independent 125,013-row
release/development fixtures contain a STRUCT with sparse/dense validity, single
and multiple runs, alternating bitsets and BOOLEAN children. All values pass.
The retained DICT_FSST-15 file remains an explicit unsupported case pending the
scalar worker's decoder, not a native compatibility pass.

Ordinary check, three focused decoder unit tests (including malformed bounds,
truncation, cancellation and row limits), nested tests (9), compression tests
(11), and enumeration tests (3) passed. Final clippy passed after its initial
single-element negative-test loop warning was corrected. Coverage reports 245
files, 2,119 functions, 201 interface methods, no missing attributes; trace check
completed successfully in 64.09 seconds with no reported errors/panics/open spans
and temporary telemetry deleted. No performance acceptance or independent Kani
claim is attached to this internal follow-up; integrated proofs remain the lead's
substantial-checkpoint responsibility.

### Continuing type/expression work

The MAP accessor increment adds `map_extract_value`/subscripts, `map_extract`/
`element_at`, `map_contains`, `map_keys`, `map_values`, `map_entries` and
`cardinality`. Lookup template inference promotes the MAP key and lookup key
together, so searching INTEGER key 1 with DECIMAL 1.1 does not round to a false
match. The lead's literal-identity hook allows a string literal to adopt the key
type while keeping VARCHAR columns subject to ordinary implicit coercion. A
retained selected key adapter performs comparisons even when execution uses an
unrelated registry without that extension family. All nested subscript syntax
now binds through the selected scalar catalog; a replacement-catalog regression
checks that syntax and direct function calls share the selected implementation.

`union_tag` returns the ordered anonymous ENUM of declared member names,
including the tag of an active NULL member. All integrated temporal families
now register scalar-to-UNION injection. Type display follows development
identifier quoting, using its 505-keyword presentation inventory, and preserves
quoted NULL child types without quoting ordinary STRUCT/UNION field names.

New SQL workloads use MAP(INTEGER, STRUCT(DECIMAL, TIMESTAMP, INTEGER[])) through
lookup chains, prepared parameters, joins, DISTINCT, window partitions,
rollback, indexed row mutation and native reopen. Ordinary check, nested tests
(12), contract tests (22), the retained-key unit test and clippy pass. The focused
accessor suite passed again after adding temporal UNION injection. Coverage
reports 251 files, 2,172 functions, 202 interface methods and no missing
attributes. Instrumentation check passed in 40.56 seconds with no reported
errors, panics or open spans and deleted temporary telemetry. This is another
internal increment, not completion of the function catalog or nested milestone.

All six requested families remain in scope. Broader UNION member-coercion coverage; VARIANT dynamic semantics; TUPLE/unnamed rows; ordered list aggregation and remaining nested functions; slicing, lambdas and shape/coercion edge cases; value formatting and mixed-family semantics need expansion. ARRAY/common-type, STRUCT field-union and composite overload ranking rules need broader reference coverage.

The next VARIANT representation prerequisite retains each dynamic child's
declared type alongside its value; inferring a minimal integer width would lose
observable `variant_typeof` metadata. Physical payload validation now shares a
16,777,216-visit budget across recursive Arc subtrees as well as the existing
depth bound, including DAGs with many logical visits. This is not a VARIANT SQL
implementation yet. Ordinary check, nested tests (11 after integration removed
the resolved codec-15 negative), the new metadata/budget unit test and all-target
clippy pass. Coverage reports 254 files, 2,186 functions, 203 interface methods
and no missing attributes; instrumentation check completed in 30.83 seconds
with no errors, panics or open spans and deleted temporary telemetry. Runtime
VARIANT normalization is planned for built-in categories with retained selected
child semantics; no extension-category normalization support is claimed.

Native DuckDB nested WAL and non-NULL default codecs are not implemented yet. The private JSON round trip is not native compatibility evidence. Native default and ART encoding reject nested values explicitly, and native nested indexes must follow reference-supported behavior rather than a blanket assumption that every type is indexable. Further tests must include nested registry replacement, adversarial payload/resource cases, alternate execution/index compositions, mixed temporal/scalar families, independent C++ files, native rollback/recovery/reopen, upstream regressions and isolated faster-reference performance campaigns.

### VARIANT SQL/execution increment

VARIANT now carries declared dynamic children through casts, vectors, comparisons,
canonical keys, joins, grouping/DISTINCT, sorting, aggregates/windows, prepared
values, indexed row mutations, rollback and private JSON reopen. Its retained
registry snapshot supplies dynamic child validation and same-type scalar
comparisons; a retained cast snapshot supplies dynamic conversions. A regression
replaces the source registries after binding and executes with an empty ambient
type registry, including NULL batch casts. Built-in categories have explicit
VARIANT normalization; extension-category normalization remains unsupported.

Integer/unsigned/decimal values share an exact decimal-scientific comparison/key
category, including full signed and unsigned 128-bit boundaries. FLOAT/DOUBLE
share a separate real category. DATE and timestamp precisions compare in the
development nanosecond category, timezone timestamps remain separate, and nested
NULL sorts last. Development raw infinity-sentinel scaling is reproduced; DATE,
TIMESTAMP and TIMESTAMP_NS infinity values are not all equal inside VARIANT.
Object comparisons/keys sort names case-sensitively, independent of insertion
order. MAP is viewed as an ARRAY of key/value OBJECTs; UNION resolves its active
child and ENUM becomes VARCHAR. No second full payload tree is needed for ordinary
comparison or extraction. A NULL active UNION member becomes SQL NULL through
the lead's separate selected output-nullability cast capability.

The selected scalar catalog now provides `variant_typeof`, `variant_extract`
(including subscripts), `variant_type`, `variant_keys`, `variant_array_length`
and `variant_exists`. Optional path functions accept constant VARCHAR paths or
lists of paths; a present NULL differs from a missing field for existence/type
inspection. Array extraction uses positive constant UINTEGER-range 1-based
indexes. Ordinary scalar/list inputs are rejected by VARIANT functions, while
string literals and untyped NULL retain their reference binding behavior.
Combination inference can still adopt VARIANT, such as `[100::VARIANT, 1.2]`.

Pinned development probes exposed an observable presentation distinction:
natively shreddable STRUCT trees are presented with sorted member names, while
STRUCTs containing LIST/ENUM/untyped-NULL children retain source order; extracting
a STRUCT from an unshredded ARRAY also retains source order. The Rust injection
retains that presentation without requiring a shredded physical representation.
VARCHAR materialization preserves development's homogeneous versus heterogeneous
array formatting. Nested scalar quoting now escapes delimiters, empty/NULL-like
strings and quotes/backslashes, without quoting child containers.

Ordinary check, all 14 nested tests, three focused VARIANT units (including the
earlier physical-budget unit), the expanded three VARIANT SQL workloads and
all-target clippy pass. Coverage reports 260 files, 2,253 functions, 204 interface
methods and no missing attributes. Kani remains an integrated-checkpoint task for
the lead, not a claimed worker proof or full subsystem completion.
The instrumentation check completed in 46.51 seconds with zero errors, panics or
open spans; temporary telemetry was deleted.

Still open: native VARIANT checkpoint/WAL/default codecs, TUPLE/unnamed values,
VARIANT contains/extract-string/normalization functions, string-to-nested fallback
casts and broader upstream/performance coverage. Native VARIANT persistence is
not established by these private-format tests. The shared DICT_FSST and
EMPTY_VALIDITY integrations have resolved the earlier native nested codec-15
negative; all retained nested native fixtures now run as positive regressions.

### TUPLE SQL and development-native reader increment

Development TUPLE has positional child metadata and reuses the retained STRUCT
payload/child-adapter machinery, without treating unnamed values as named
STRUCTs. SQL now distinguishes `()`, `(x,)`, `(x)` and `(x,y)`; `row`, positional
access, `struct_extract_at`, `struct_values`, `struct_keys`, recursive positional
casts and TUPLE/STRUCT combination inference are covered. Empty TUPLE and STRUCT
values are non-NULL; UNION still requires at least one named member. The local
DuckDB grammar change preserves singleton commas in parser-rendered SQL and
keeps subquery/lambda handling ahead of the tuple path.

Mixed DECIMAL/TIMESTAMP_NS/LIST/BIT tuples pass comparisons, grouping/DISTINCT,
joins, windows, prepared values, indexed-row mutation, rollback and private JSON
reopen. VARIANT views TUPLE as an ARRAY and converts compatible ARRAY values back
to positional TUPLE children. TUPLE keys explicitly reject ART primary/unique
indexes while ordinary equality keys remain usable.

The independent pinned development `nested_tuple` fixture explicitly selects
storage version v2.0.0 and retains its producer identity, SQL, checksum and
observed codecs. The reader accepts development logical ID110, legacy nonempty
unnamed STRUCT metadata, and omitted default-empty child metadata. It preserves
positional values, nested NULLs and empty TUPLE/STRUCT streams. Publication of
TUPLE-containing native files remains deliberately Unsupported until the lead's
minimum-version writer selection is integrated; the test verifies that a failed
publication leaves the table unchanged. This is not bidirectional compatibility.

This independent file exposed a shared DECIMAL decoder defect: a NULL child
encoded with the physical signed minimum was rejected as an out-of-precision
decimal before its validity mask was applied. Logical reconstruction now maps
that reserved sentinel to NULL at each 2/4/8/16-byte width. Tests cover precision
boundaries, scale zero/full scale, both selected bitpacking adapters, rejection
of other out-of-domain coefficients, and rejection of NULL decoder output under
an explicitly valid child mask.

Ordinary check, all 17 nested tests, all 16 compression tests, the focused three
TUPLE regressions after the final accessor lint repair, and all-target clippy
pass. Coverage reports 265 files, 2,311 functions and 205 interface methods with
no missing attributes. This is an internal integrable increment; Kani remains
the lead's substantial integrated-checkpoint task. Native TUPLE publication,
nested WAL/default codecs, dynamic VARIANT native storage and the broader
nested function/performance inventory remain open.

The instrumentation check completed in 53.11 seconds with zero errors, panics
or open spans; temporary telemetry was deleted. The fixture generator also
rejects an explicit release-target TUPLE request before creating output.

### Nested WAL vector and development metadata increment

Native WAL vectors now carry LIST/ARRAY/STRUCT/MAP/UNION children through
recursive validity and physical streams. TUPLE uses the same positional stream
machinery internally, but public WAL publication remains rejected by the native
type/version gate; VARIANT physical WAL storage is still unsupported. Child
vector dispatch handles flat, constant and dictionary encodings recursively.
LIST entries validate arbitrary offset/length slices instead of assuming
contiguous offsets, including overlapping dictionary slices. A shared 16-million
cell budget bounds immediate child expansion before allocation and charges
owned overlapping-slice copies; depth remains bounded at 64.

Pinned development required two additional compatibility fixes. WAL USE_TABLE
and DROP_TABLE use a field-103 qualified name instead of legacy schema/table
fields 101/102. Both formats are read, with conflicting identities and nested
schema paths rejected. Development string vectors store an optional-index byte
count in field 107 followed by length/data blobs 108/109. This optional index is
an unsigned value with UINT64_MAX sentinel, not a boolean-prefixed pointer. The
decoder checks exact lengths, zero-length NULL entries, bounded offsets and
unused bytes while preserving the legacy field-102 string list.

Source references are pinned `src/common/types/vector.cpp`,
`src/include/duckdb/common/serializer/{serializer,deserializer}.hpp`,
`src/storage/serialization/serialize_wal.cpp`, and
`src/parser/qualified_name.cpp`. Independent interrupted-process fixtures are
retained separately under `test/data/wal-nested-{release,development}`. The
development fixtures include default and explicit v2.0.0 storage. All nine
recorded commit boundaries preserve expected nested values and leave checkpoint
and WAL bytes unchanged on read-only recovery.

The Rust WAL workload covers mixed DECIMAL/TIMESTAMP_NS/BIT/STRUCT/LIST/ARRAY/MAP/
UNION values, prepared insertion, update/delete rollback, committed mutation,
recovery, equality joins, checkpoint and reopen. The separate
`scripts/nested_wal_reference.py` campaign passed all four selected producer
cases across pinned release/development: Rust WAL and independent C++ checkpoint
followed by Rust WAL, then C++ mutation/checkpoint and Rust reopen. Exact producer
identities, Rust/source fingerprints, durable-pair hashes and final rows are in
`docs/nested-wal-reference.json`; this correctness run is not a performance
measurement or full compatibility claim.

Ordinary check, nested tests 19/19, contracts 25/25, logging 7/7, recovery 14/14,
the three focused WAL unit tests and all-target clippy pass. The units exercise
truncation, bad shapes, slice amplification, development string metadata and
ambiguous/deep qualified names. Coverage reports 275 files, 2,421 functions and
207 interface methods with no missing attributes.

Still open: C++ physical nested child-update paths and their parent/child
validity ordering, nested native non-NULL default codecs, version-aware TUPLE
publication and VARIANT native storage. The mixed fixture's UNION update uses
replacement rows, so it does not prove the distinct physical child-update path.
Kani remains the lead's substantial integrated-checkpoint responsibility.

The instrumentation check completed in 61.29 seconds with zero errors, panics
or open spans; temporary telemetry was deleted.

### Atomic physical child-update recovery follow-up

The native recovery reader now distinguishes physical STRUCT/TUPLE/UNION child
paths from their separate validity records. Source inspection of pinned
`src/transaction/wal_write_state.cpp`, `struct_column_data.cpp` and
`standard_column_data.cpp` established that a terminal zero selects validity;
positive child positions select physical members, including UNION's hidden tag.
Malformed indices, descendants below validity and excessive depth are rejected.
LIST/ARRAY/MAP/VARIANT child-update paths remain explicitly unsupported; this does
not prevent their whole-value replacement through the existing WAL vector path.

Recovery stages each affected column in a transaction-private physical tree.
Parent and child validity remain independent while records arrive, and UNION
tags/inactive members are validated only when the transaction is materialized.
Children written below a NULL parent do not make that parent non-NULL. Publication
remains copy-on-write and atomic, including derived-index validation. A shared
16-million-node budget, depth bound and cancellation checks constrain staging.
This temporary tree is not a new persistent representation for hidden values
below committed NULL parents.

Separate pinned release/development fixtures under
`test/data/wal-nested-paths-{release,development}` exercise actual physical child
records, unlike the preceding UNION replacement-row workload. All 12 recorded
commit states pass exact-value recovery and read-only durable-byte preservation.
All 10 nonempty states also pass writable recovery, rollback, checkpoint and
reopen. The fixtures include NULL parents becoming populated, nested NULLs,
restoring previous children and multiple parent changes in one transaction.
The atomicity regression reverses valid child/tag/validity record order and checks
that malformed paths, wrong child types and invalid final UNION states publish
neither changed rows nor earlier schema operations.

The WAL writer's scalar-column traversal now borrows values directly, removing
the temporary column allocation and deep VARCHAR/BLOB clones introduced by the
recursive-vector prerequisite. Nested roots still share outer Arc handles for
physical child expansion. Logging's NUL-string/float-bit/atomic-tail checks and
the existing recursive-vector workloads remain passing; no throughput improvement
is claimed without the lead's coordinated measurements.

Ordinary check, nested 21/21, logging 7/7, recovery 14/14, contracts 25/25, four
focused WAL units and all-target clippy pass. The independently refreshed mixed
WAL campaign passes all four producer cases across both pinned C++ versions with
an unchanged source fingerprint; evidence is retained separately in
`docs/nested-wal-recovery-followup-reference.json`. That script checks the mixed
replacement-row workload, while the new component fixtures establish physical
child-update coverage. Coverage reports 277 files, 2,448 functions and 207
interface methods with no missing instrumentation attributes.

This is an internal integration increment. Kani remains the lead's substantial
combined-checkpoint responsibility; full native parity is not claimed. Native
nested non-NULL defaults, version-aware TUPLE publication, VARIANT physical
storage and the broader function/performance inventory remain open.

The final instrumentation check completed in 31.30 seconds with zero errors,
panics or open spans; temporary telemetry was deleted.

### Exact BIGNUM children in UNION and VARIANT

BIGNUM now participates in scalar-to-UNION injection and bidirectional VARIANT
casts without narrowing its payload. `variant_typeof` retains BIGNUM identity,
including values beyond 128 bits. Exact numeric comparison/key normalization uses
cancellable decimal digit conversion, matching pinned
`src/common/types/variant/variant_comparison.cpp`: integers, decimals and BIGNUM
share NUMBER, while floating values remain a separate REAL category. A
4,097-digit unit witness checks the unbounded exponent/digit path.

Ordinary BIGNUM distinguishes negative zero. VARIANT equates both zero signs in
comparison and keys, while casts, extraction, prepared values and private reopen
retain the original `-0` payload. Same-type zero comparisons still invoke the
retained selected BIGNUM adapter with canonical comparison arguments; a registry
replacement/empty ambient registry test verifies this ownership. Other
same-type comparisons retain their selected implementation. STRUCTs containing
BIGNUM preserve source member order, consistent with the development shredding
eligibility rules, rather than being presented as sorted shredded objects.

The SQL regression carries mixed BIGNUM/DECIMAL/INTEGER/DOUBLE VARIANT values
through nested casts, UNION access, DISTINCT subqueries, equality joins, sorting,
window partitions, prepared comparison, indexed-row mutations, rollback and
private JSON reopen. Native VARIANT storage is still not implemented or claimed.
Ordinary check, nested 22/22, BIGNUM 2/2, three selected VARIANT units and
all-target clippy pass. Coverage reports 281 files, 2,500 functions and 207
interface methods with no missing attributes.

The retained diagnostic campaign is **not passing**:
`docs/nested-bignum-variant-exact-reference.json` has 11 matching cases out of 14
against pinned development, with unchanged source. The earlier 10/13 report is
also preserved. Three remaining witnesses are explicit:

- Stored development VARIANT_NULL reports `IS NULL = false`, although scalar
  `NULL::VARIANT` reports true. Rust currently uses SQL NULL in both contexts.
- Development `count(DISTINCT v)` and `count(v)` return 8 for the retained stored
  workload; Rust returns 4 and 7. The development DISTINCT subquery returns five
  groups, matching Rust, so aggregate behavior is a separate obligation.
- The REAL group's VARCHAR rendering is `1.0` in development and `1` in Rust.
  VARIANT correctly uses its retained FLOAT/DOUBLE-to-VARCHAR cast; the scalar
  owner will investigate that formatter rather than adding a VARIANT bypass.

These observations remain open correctness work, not waived tests or evidence
of full parity. LIST concat overloads, strict VARIANT temporal-string parsing,
native defaults/storage and broader nested function coverage remain tracked.
Kani stays at the lead's substantial integrated checkpoint.

The final instrumentation check completed in approximately 68.28 seconds with
zero errors, panics or open spans; temporary telemetry was deleted.

The follow-up `docs/nested-bignum-variant-optimizer-reference.json` refines the
NULL diagnosis without changing the failing default-development baseline.
Disabling only C++ `statistics_propagation` changes the stored predicate to
`IS NULL = true` and `count(v)` to 7. `count(DISTINCT v)` becomes 7 rather than
4, while the DISTINCT subquery still produces five groups. The discrepancy is
therefore optimizer-sensitive and does not prove that a present-null physical
VARIANT payload is needed. Pinned `variant_comparator.cpp` explicitly describes
root NULL as genuine SQL NULL. All six diagnostic probes are retained; the
ordinary comparison campaign still has three failures (11/14 matches). No
optimizer-specific accident has been copied into generic Rust values.

### Typed sequence concat integration

The existing selected `concat` catalog entry now delegates LIST/ARRAY arguments
to a family-owned binding. It infers child metadata through the selected type
registry, retains the result adapter and requests explicit selected plan casts
to that list type. ARRAY operands produce LIST results. Typed and untyped NULL
lists are skipped, including an empty non-NULL result when all list operands
are NULL; scalar all-NULL concat still produces an empty VARCHAR. Mixing lists
with scalar inputs is rejected before string conversion. Disjoint STRUCT fields
use recursive common metadata with missing NULL fields.

The supporting common-type proposal combines BOOLEAN only with integral types,
in both argument orders, matching pinned `src/common/types.cpp:1019`. It does not
grant a global implicit BOOLEAN numeric cast, and BOOLEAN/DECIMAL/FLOAT/DOUBLE
combinations remain rejected. Recursive LIST/STRUCT metadata and all ten signed
and unsigned integral widths are covered. The lead owns the separate CASE,
UNION and VALUES combination-cast integration.

Concat execution does not stringify children: an unrenderable physical temporal
value can remain in a typed result until an actual text boundary is requested.
The selected-ownership unit replaces registries after binding and executes with
an empty ambient registry. SQL tests carry mixed DECIMAL/TIMESTAMP_NS/BIT lists
through prepared insertion, joins, window partitions, indexed-row mutations,
rollback and native checkpoint/reopen.

The selected development campaign in `docs/nested-concat-reference.json` passes
16/16 SQL/error checks and both independent native producer paths. Each producer
passes exact initial, Rust mutation/rollback and C++ mutation/checkpoint/reopen
states. Source and executable identities are retained with an unchanged source
fingerprint. These are correctness checks, not throughput measurements.

Ordinary check, nested 24/24, types 15/15, binary scalars 5/5, casts 11/11,
contracts 26/26, the selected concat ownership unit and all-target clippy pass.
Coverage reports 284 files, 2,553 functions and 208 interface methods without
missing attributes. The instrumentation check completed in 41.03 seconds with
zero errors, panics or open spans; temporary telemetry was deleted. Kani remains
the lead's substantial combined-checkpoint responsibility.

The provisional scalar LIST guard and negative assertion are removed. No
duplicate catalog name was registered. `list_concat`/`array_concat` aliases and
registry-aware `||` specialization remain separate follow-ups, along with the
previously recorded VARIANT and native-storage gaps.

The later [selected floating-text increment](floating-text-values.md) repairs
the REAL `1.0` VARCHAR witness through the retained scalar cast, including
nested child casting and concat. The historical 11/14 diagnostic above is
unchanged; the stored VARIANT NULL/count observations are not resolved by that
formatter repair.
