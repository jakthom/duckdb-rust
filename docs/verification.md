# Verification scope

Raw JSON and JSONL records cited by this historical narrative are intentionally
excluded from the active documentation tree. Previous copies remain recoverable
from Git history; new validation output belongs under `target/`.

Current reference selection is documented in the [two-reference runbook](reference-builds.md).
The v1.5.5 source build is the default compatibility oracle; the pinned development
checkout has a separate campaign. The
[latest performance comparisons](settings/README.md) pass all sixteen
measured cases against both targets, including ORDER BY ALL, grouped SUM, ROLLUP and CUBE.
Grouping SQL still has independent reference differences and unsupported
upstream files. The earlier combined report
retains unresolved file/ALTER compatibility failures and its original failed
measurements. Historical v1.3.0 results below retain their original scope and
cannot establish current release compatibility.

**Current acceptance is full C++ test parity and zero performance regressions
against C++ DuckDB. Neither is achieved.** See [the parity report](testing-parity.md)
and [the accepted requirement](../specs/testing/parity.md). The 1.25 budgets and
previously accepted measurements below are historical; they do not satisfy the
current requirement. Their original results and provenance remain unchanged.

The checks here concern this Rust implementation. The source-system testing specifications describe a much larger suite and are not a report of tests passed by the rewrite.

## Automated checks

`python3 scripts/verify_kani.py` runs the maintained Kani proof suite at each
substantial implementation-stage boundary. The [Kani policy and runbook](../specs/testing/kani.md)
define cadence, installation, proof scope, and how to report findings and limits.
During exploration, a checkpoint requires a run and a short report; proof success
is not a stage-completion condition. The command retains nonzero exit statuses
for unsuccessful or incomplete verification. Ordinary checks/tests remain the
per-edit feedback loop.

`cargo test --all-targets` runs the contract suite, native file compatibility cases, and the supported SQLLogicTest corpus. It requires no installed DuckDB engine. Interface tests vary adapters through `DatabaseBuilder` and the ordinary public contracts.

Configuration contracts run the settings, named-session and `ORDER BY ALL`
corpora across two providers, two optimizers, two expression evaluators, two
executors and three batch sizes. They also verify typed custom registrations,
retained statement views, prepared rebinding, cancellation and failed scheduler
publication. The [persistent reference campaign](settings/README.md) preserves
connection lifetimes and records exact source, worker and adapter identities:

```sh
python3 scripts/session_reference.py \
  --corpus test/sql/ordering.test --corpus test/sql/settings.test \
  --corpus test/sql/settings_sessions.test --corpus test/sql/grouping.test \
  --report target/session-reference-report.json
```

Sorting contracts select comparison merge sorting and integer radix sorting
through `NativePhysicalPlanner::with_sorting`, then run the same ordering corpus
across 48 optimizer/evaluator/executor/batch configurations. Independent checks
cover all signed integer widths, full-range offsets, NULL placement, stable ties,
flat/dictionary/constant encodings, empty and singleton inputs, custom comparison
and logical validation, malformed evaluator results, effects, first errors,
cancellation and row limits. Fallback checks preserve string/Boolean ordering
and exact floating bits. Both algorithms check the complete order of the
50,000-row measurement query; checksum validation alone is not an ordering oracle.
See [settings and sorting evidence](settings/README.md).

Choose a new report path for each run. The combined campaign remains failing
because of recorded reference differences; passing ordering records do not
establish complete settings or grouping compatibility.

The batch regression tests compare scalar and column evaluation across optimizer
selections and batch sizes 1, 3 and 2048. They cover all signed integer widths,
minimum/-1 overflow, division by zero, NULLs, constant/dictionary/sliced vectors,
lazy branches, first-error order and function effects. Replacement operators
control their own totality proofs; invalid operator, type and expression batches
must fail before filtering can hide them. Storage checks retain batches across
writes and snapshot destruction, preserve row-ID holes, and round-trip the
logical snapshot format. The corresponding C++ performance reports retain every
failed trial under the unchanged 1.0 limit.

EXISTS decorrelation tests compare identity and pass pipelines, scalar and batch
evaluation, hash and nested-loop joins, both subquery consumers, and batch sizes
1, 3 and 2048. An independent row model checks signed remainders, duplicates,
NULLs, ordinal remapping, ordering, outer limits and empty inputs. Plan checks
require conservative fallback for throwing expressions, deeper correlation,
inner limits and small row budgets. Prepared queries check own writes, rollback
and older readers. Volatile calls retain their evaluation counts. Floating keys
retain NaN and signed-zero equality; both registered ASCII adapters exercise
case-insensitive semi/anti joins. Instrumented join cursors check one build per
cursor, bounded demand, retained output, empty-outer laziness, invalid schemas,
resource errors and cancellation. Key writers preserve existing components on
partial errors, cancellation and swallowed size failures.
Column key visitation matches scalar canonical keys across NULLs, slices and
selections, validates the entire logical input before callbacks, and stops on
consumer failure.

The cast suite runs the standard-library and checked-digit parsers through the same registry contracts for all five signed integer types and explicit/assignment modes. It covers extrema, overflow, invalid text and 2,048 deterministic integer samples, retained adapter ownership, concurrent callers, NULL propagation, missing/duplicate registrations, malformed results, and cancellation even when an adapter returns a conversion error. The SQL matrix crosses both parsers, both index adapters and both checkpoint formats, checking defaults, INSERT/UPDATE, prepared parameters, indexed predicates, rollback and reopen. Typed plan, vector and storage boundaries reject hidden conversions. These cast tests establish the supported integer-text subset, not full DuckDB cast syntax; registered-type coverage is described separately below.

The type suite compares two implementations of the same registered ASCII type across equality/order, canonical/composite keys, parameter identity, metadata limits, invalid payloads, retained index behavior and private-format restart. The SQL matrix varies both type adapters, both index adapters and hash/nested-loop joins; it covers grouping, DISTINCT/UNION, defaults, casts, prepared values, rollback and uniqueness. Missing registrations and unsupported native writes preserve file bytes. Malformed scalar-function and physical-operator payloads must return errors, including under TRY_CAST. On 64-bit hosts, representation tests limit primitive Value size to 32 bytes and logical metadata handles to 16 bytes.

The optimizer suite compares identity, expression simplification alone and the default pass pipeline. It checks constant casts and pure operators, retained adapters and registered metadata, exact floating-point bits, nesting, cancellation, relational results and lazy conversion/resource errors. Validation rejects malformed inputs and rewrites, context transfer and cancellation; an invalid pass must fail before the next pass or execution. Column-projection contracts check shared payload ownership, reordered/duplicate/zero columns, empty input and invalid ordinals. Execution contracts also verify demand and error behavior through these projections. Registered payload validation is checked at different ordinals in mixed schemas, and oversized canonical keys leave their caller's output unchanged.

The operator suite compares dynamic-programming and greedy LIKE through the
same checked registry and SQL callers. A separate recursive oracle checks
22,015 Unicode/NUL/wildcard input pairs for both LIKE and NOT LIKE with both
adapters, plus shared UTF-8 prefix and retry cases. It checks signed-width overflow (including minimum/-1 division and
remainder), integer literal overloads, NULLs, date boundaries/infinities,
prepared date offsets, both snapshot formats, and both optimizer choices.
Missing/ambiguous signatures and incompatible replacements fail explicitly;
retained bindings remain stable across replacement and concurrent callers.
Malformed physical/logical results remain errors, volatile operators are not
folded, lazy failures retain their timing, and long LIKE work is cancellable.
An extension type registers arithmetic through the same SQL operator callers.

The index contracts run with hash and B-tree adapters, including duplicate/NULL/NaN/signed-zero keys, ownership, concurrent readers, invalid inputs and cancellation/resource errors. The integration matrix crosses both index adapters, both checkpoint formats, identity optimization and three pass sequences. It checks rollback, restart, uniqueness, reader visibility, and actual index calls. A 5,000-row table with a one-row query budget demonstrates that eligible SELECT lookups avoid full-table materialization.

The execution suite compares pull and eager adapters with batch sizes 1, 3 and 2048. It exercises filters with empty intermediate batches, unions and distinct across boundaries, grouping, joins, demand reduction, early stop, independent cursors, owned chunk lifetime, concurrent snapshot publication during consumption, callback failure, cancellation, and malformed adapter output. Instrumented scalar calls assert actual upstream evaluation counts. A 10,000-row stream and aggregate complete with a seven-row state limit. Scheduler omission/replay tests verify that unexecuted or repeated work cannot commit.

The compression suite runs word and scalar bitpacking through the same registry, covering all supported integer widths and DATE, constant/delta/frame modes, 32-value padding and partial tails. Both replace the decoder in a real file-backed database without changing query callers. Concurrent requests, duplicate registration, unsupported types, cancellation/resource rejection and malformed adapter outputs are checked. ALP tests cover every exponent/factor pair for both floating-point widths, packed widths through 64 bits, and exact NaN/negative-zero/infinity exception bits. ALP-RD tests cover every supported cut width and dictionary size, including exception indexes outside the stored dictionary. Chimp and Patas tests cross group boundaries, ring wrap, byte/bit alignment and prior-value references. Truncation and bounded payload mutations exercise all four floating-point readers. These bounded mutations are not a general fuzzing result.

FLOAT contracts check physical result types, aliases and precision declarations, rounding at arithmetic boundaries, narrowing-cast errors, nonfinite values and aggregate promotion. Both checkpoint formats preserve extrema and 4,096 sampled bit patterns, including NaN payloads. Both runtime index adapters enforce FLOAT uniqueness across rollback and reopen. These samples do not enumerate all 32-bit patterns.

DATE contracts enumerate 292,194 successive calendar days across two complete
Gregorian cycles spanning year zero, plus known epochs, both finite extrema,
infinities and 10,000 sampled day counts. They check invalid calendar casts,
reserved physical values and a checksummed corrupt DATE slot with its validity bit still set, selected cast failures under typed literals/defaults
and TRY_CAST, lazy errors, grouping, joins and distinct values. Both index
adapters and both snapshot formats preserve date defaults and transactional
mutations across restart. Three independent DATE fixtures cover uncompressed,
constant, RLE and bitpacking columns, NULLs and a 125,013-row table. Both
bitpacking adapters read every stored date. These checks cover the documented
calendar grammar; timestamp suffix conversion remains open; date arithmetic is covered by the operator suite.

The shared tests also cover hash and nested-loop joins at batch sizes 1, 3 and 2048; NULL and duplicate-key semantics; outer/semi/anti joins; grouping and aggregates; short-circuit expressions and overflow; prepared rebinding; transactional DDL and constraints; rollback and abandoned transactions; optimistic write conflicts; both durable formats and memory persistence; failure injection before publication and at an uncertain outcome; positional fetch identity/order; vector ownership/cardinality; invalid logical plans; cancellation and resource rejection.

Format tests cover every implemented primitive type, integer extrema, NaNs, infinities, signed zero, UTF-8, embedded NULs, large overflow strings, empty schemas, schema/table names containing dots, and persisted literal defaults with metadata-chain boundaries. The private snapshot payload has a checksum. Native-file tests cover truncation, block corruption, an invalid redundant header, locking across processes, unsupported publication without file changes, read-only and writable WAL recovery, interrupted recovery publication, and rejection of unsupported checkpoint transitions.

`cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings` check the complete Rust target set, including the shell, harness and embedded example.

## Independent file oracle

[`scripts/generate_compatibility_fixtures.py`](../scripts/generate_compatibility_fixtures.py) creates gzip fixtures with an independent DuckDB CLI and copies historical Chimp/Patas artifacts from the source tree. [`manifest.json`](../test/data/duckdb/manifest.json) records generating SQL and writer revision for generated files, or source path/revision for copied artifacts, plus uncompressed size and SHA-256. The original writer revisions of the two historical artifacts are unrecorded. Gzip timestamps are fixed. Database bytes themselves need not be deterministic across independent generation runs.

The fixtures exercise uncompressed/constant data, RLE, bitpacking, all four floating-point codecs, dictionary strings, FSST strings, overflow strings, multiple row groups, schemas, primary/composite/unique keys, literal defaults and casts, and sparse/dense/whole-vector committed deletion masks. Generated fixtures use DuckDB v1.3.0 (`71c5c07cdd`). The copied Chimp and Patas artifacts each contain 245,000 DOUBLE measurements and their FLOAT counterparts; the independent oracle compares every value in both tables before and after publication in both engines. Tests use the files directly without invoking C++. The ALP and ALP-RD fixtures contain 125,013 rows each, multiple segments and row groups, NULLs, decimal-scaled values, finite extrema, subnormals, infinities and NaNs. Their manifest entries also record the reference engine’s observed compression choices; the verification script checks that the expected codecs are actually present. Finite results are compared by IEEE bits before and after Rust publication.

[`scripts/verify_reference.py`](../scripts/verify_reference.py) separately:

1. Verifies fixture hashes and compares every logical value between engines.
2. Writes each existing file with Rust and checks its original rows with DuckDB.
3. Writes the result with DuckDB and checks it again with Rust.
4. Creates 125,000 rows with Rust, crossing row-group and metadata-chain boundaries, and verifies mutations in both engines.
5. Checks signed integer extrema, NULL and Unicode in Rust-written files.
6. Asserts that DuckDB actually traverses a Rust-written ART using EXPLAIN ANALYZE, independently rejects duplicate/NULL constrained keys, and mutates the resulting indexes.
7. Checks empty ART roots, floating-point unique keys, 301 nested string prefixes and a 20 KiB key.
8. Inserts default rows in both engines after rewriting a checkpoint with primitive literal defaults.
9. Checks FLOAT schema, defaults, extrema, arithmetic and aggregation; traverses a Rust-written FLOAT ART and mutates its keys with DuckDB.
10. Checks DATE schema and calendar casts, traverses Rust-written DATE ART keys across BCE/infinities/finite extrema, enforces uniqueness, and verifies date defaults and mutations in both engines.
11. Checks numeric overload result schemas, integer division/overflow, date arithmetic, Unicode LIKE, constant arithmetic defaults, and mutations in both engines.
12. Compares four interrupted-process WAL fixtures at their recorded commit boundaries, repeats read-only recovery without changing either file, completes writable recovery and log retirement, and checks subsequent mutations in both engines.
13. Interrupts Rust recovery at 12 replacement and four retirement boundaries, compares the surviving files with both readers, and retries recovery. A Rust-origin file with a same-shaped native update tests root relocation independently.
14. Checks native checkpoints interrupted before header publication and before WAL truncation, using both readers before and after writable recovery.
15. Reads Rust-written transaction WALs with both engines, verifies continued writes across checkpoint/row-ID transitions, independently checks restored unique constraints, and interrupts logging at nine I/O boundaries and after acknowledged commit.
16. Runs eight online checkpoint workloads across log-size and commit-count policies, verifies explicit maintenance and continued writes in both engines, and checks 26 manual/automatic process-interruption outcomes.
17. Runs the shared subquery corpus with both subquery adapters and checkpoint/WAL durability, checks native values and errors for every record, and continues mutations in both engines.

Reference rows are serialized to JSON inside DuckDB before reaching its shell: the v1.3 shell otherwise truncates string fields at embedded NUL bytes. Successful empty results emit no bytes and are normalized to an empty row list. DuckDB emits nonstandard JSON tokens for nonfinite floating-point values; the oracle normalizes those numeric tokens to the Rust shell’s string representation without rewriting VARCHAR values. Queries widen FLOAT values exactly to DOUBLE where needed for comparison, avoiding differences between shortest decimal spellings while preserving every finite FLOAT value.

This independent check found incorrect legacy segment offsets that a Rust-only round trip did not detect. Both the writer and reader now enforce the corrected identities. The large independent fixture keeps that boundary in the ordinary Cargo suite.

To reproduce:

```sh
cargo build --release --bin duckdb-rust
python3 scripts/verify_reference.py --report reference-report.json
```

The recorded run passed on 2026-09-09 UTC against DuckDB v1.3.0. The JSON report records the reference revision, Rust toolchain, host platform, source and executable hashes, selected adapters, 37 top-level check outcomes and elapsed verification time. Recovery publication, transaction logging and online checkpointing also record their test workers' source/executable hashes and each interruption boundary. The subquery group records its corpus hash and all 144 record/configuration outcomes. That elapsed time is not a database benchmark.

## Recovery conformance

[`generate_wal_fixtures.py`](../scripts/generate_wal_fixtures.py) starts an
independent DuckDB v1.3 process, checkpoints setup data, waits for commit
acknowledgments, then kills the process without shutdown. A separate read-only
DuckDB process records recovered rows and physical row IDs. The
[WAL manifest](../test/data/wal/manifest.json) records the writer revision, setup,
transactions, checkpoint/WAL hashes and expected states. Four fixtures cover
native versions 64 and 65, schema/table create/drop, constants/defaults and
constraints, all supported primitive types, extrema/nonfinite values, embedded
NUL/Unicode strings, deletion holes, append identities, NULL transitions and
multi-vector changes. Regeneration requires the independent executable; ordinary
Cargo tests use the saved files without C++ calls.

Recovery tests vary both index adapters and both bitpacking decoders through
ordinary composition. They compare every recovered value and physical row ID,
repeat read-only opens, reject writes, and check original file bytes. A complete
two-transaction log is truncated at every byte boundary after its version header;
only complete transactions may become visible. Corrupted record checksums,
unsupported committed records, malformed checkpoint markers,
malformed commit markers, missing adapter selection, incompatible formats,
orphan logs and checkpoint/recovery sidecars must fail explicitly.

Target contracts verify deferred unique-key checks, index rebuilds, retained
snapshot isolation, and atomic failure of mixed catalog/data changes with both
index adapters. Source-derived serialized vector tests cover constant,
dictionary and sequence encodings, invalid selections, NULL strings, nesting
limits and numeric overflow. Cancellation and row limits are checked at recovery
boundaries. The newer database-identity WAL header is constructed for contract
tests; the independent v1.3 writer does not emit that header. Tagged bridge
tests preserve the database identity and check the allowed generation transition.

[`generate_checkpoint_fixtures.py`](../scripts/generate_checkpoint_fixtures.py)
captures native checkpoint failures before header publication and before WAL
truncation. It kills the reference with stdin still open after the reported
fatal checkpoint error, preventing shutdown from retrying the checkpoint.
The [checkpoint manifest](../test/data/wal/checkpoints/manifest.json) records
provenance, SQL, hashes, expected rows and whether the checkpoint was published.
Ordinary Rust tests check replay versus retirement and every truncation boundary
between the complete checkpoint marker and its flush. These are actual native
root pointers, including the metadata chain-link offset.

Publication contracts pair bridge logs with both old and successor checkpoints,
check logical results and future mutations, reject stale plans, and prevent
direct replacement from discarding active logs. Root-collision tests include
long metadata chains. File publication errors are injected before each of 12
replacement and four retirement operations, checking definite versus uncertain
errors, temporary-file cleanup, retained commits and successful retry. Separate
child processes exit at all 16 boundaries without running Rust destructors.
[`wal_reference.py`](../scripts/wal_reference.py) uses the same process worker
to check surviving states with an independent DuckDB reader and repeat Rust
recovery. These checks cover the stated operation boundaries, not arbitrary
partial system calls, reordered persistence, power loss or concurrent checkpoint
WAL merging. Fault injectors are not a second filesystem or recovery adapter.

## Transaction logging conformance

The same transaction matrix selects full checkpoint or WAL durability with both
hash and B-tree indexes. It checks own writes, old-reader visibility, failed
constraints, rollback, no-op catalog changes, transient schemas/tables, repeated
indexed updates, deletes of newly inserted rows and further writes after reopen.
A point-write test checks that the checkpoint bytes and prior log prefix remain
unchanged and that individual commits append small records. This is a byte-I/O
property, not a latency measurement. A generated transaction is truncated at
every byte after the preceding commit, exposing only complete transactions;
exact FLOAT/DOUBLE NaN bits and embedded NUL/Unicode strings survive replay.
Incompatible logging/recovery/file compositions fail before mutation.

Errors at five header-initialization, two append and two rollback boundaries
verify rollback and failure classification. Header publication uncertainty is
`RecoveryRequired` with a definitely uncommitted incoming transaction; failed
append rollback is `CommitUnknown`. Append/rollback cases include
an earlier acknowledged transaction in the WAL; successful rollback preserves
that exact prefix. Uncertain outcomes prevent subsequent transaction starts.
Separate workers exit without destructors at all nine boundaries and immediately
after acknowledged commit. Reopening checks retained commits and future writes.

[`logging_reference.py`](../scripts/logging_reference.py) checks the surviving
states with native DuckDB and Rust and retries recovery. Its four SQL workloads
cover a native checkpoint with deletion holes, row-ID remapping, primitive
types/defaults and multi-batch changes. Both engines read each Rust log without
changing either file. Native mutation checks enforce WAL-restored keys;
checkpointing and continued writes in both engines exercise physical identity
transitions. These results cover the supported tuple/catalog records and named
fault boundaries. Partial system calls, power loss, group
commit, concurrent histories and a second log encoder remain separate work.

## Online checkpoint conformance

The checkpoint suite selects manual maintenance, log-size scheduling and
commit-count scheduling through `FileWal`, with hash and B-tree indexes. Actual
checkpoint bytes distinguish the scheduling decisions. Tests retain old readers
and pending writers across physical compaction, assert stable live row IDs and
append high-water marks after deleting every row, and continue writes before and
after reopen. SQL, prepared statements, the typed statement API and direct
`Connection::checkpoint()` share maintenance behavior. Comments, quoted strings,
delimiters, explicit transactions, read-only rejection and cancellation are
covered.

Layout validation checks duplicate row values and exact NaN payloads/defaults.
Missing source mappings, aliased/wrong destinations, changed values and invalid
high-water marks fail. A malformed format adapter fails before publication;
both files remain unchanged and the live writer remains usable. This fake is a
contract fault injector, not a second successor format. A rejected first append
leaves a header-only log that explicit maintenance retires without rewriting the
checkpoint, after which the same session can append again.

Injected I/O errors cover 12 checkpoint publication boundaries in both manual
and automatic modes. Each reports `RecoveryRequired`, blocks new transactions,
and recovers all acknowledged work without the incoming transaction. Process
workers exit without destructors before each boundary and after acknowledgment,
covering 26 surviving states. Reopen checks contents and future writes.
[`checkpoint_reference.py`](../scripts/checkpoint_reference.py) independently
reads those states in DuckDB and Rust and retries recovery. Its eight additional
workloads combine both policies with native deletion holes, row remapping,
primitive types/defaults and multi-batch changes. Reading the checkpoint alone
confirms that the final incoming append belongs only to the WAL. Continued writes
in both engines check the resulting catalog, indexes and physical identities.
These checks do not simulate torn system calls, power loss or concurrent
checkpoint merging, and do not measure checkpoint pause time.

## Subquery conformance

The [shared SQL corpus](../test/sql/subqueries.test) runs across both subquery
adapters, both outer executors, identity/default optimization, both index
adapters, separate/fused scan filtering and batch sizes 1, 3 and 2048:
96 configurations. It checks empty/multiple
scalar results, NULL-aware IN/NOT IN, EXISTS projections/aggregates, nested lexical
scopes, CTE rebasing and shadowing, grouped captures, joins, ordering and unions.
INSERT/UPDATE/DELETE and CREATE TABLE AS use the same dependencies; failed scalar
cardinality checks leave mutation state unchanged.

Component contracts check prepared parameter/type rebinding, own writes and old
reader snapshots, rollback, restart with checkpoint/WAL durability, concurrent
queries and retained results. Primitive and registered ASCII type adapters
exercise comparison and casts. Invalid depth/ordinals/types and combined nesting
fail validation; aggregates that belong wholly to an outer scope fail explicitly.
Instrumented planners verify one compilation per subplan per statement, and
counter functions distinguish once-per-statement scalar initialization from fresh
correlated evaluation. Streaming and eager consumers demonstrate different
actual input demand and row-budget behavior through the same contracts.

Cancellation during nested scans stops both adapters at the checked boundary
and leaves an implicit connection usable. Invalid adapter output, NULL EXISTS
results and cancellation after an adapter returns cannot become successful
TRY_CAST results. Relational dependencies precede scalar COALESCE, while a
statically false CASE branch is removed after binding/type checks. These two
behaviors are independently checked in DuckDB.

The scan/filter strategies also share targeted checks for actual predicate call
counts, early LIMIT termination before a later error, NULL filtering and retained
chunks after mutations. Invalid scan widths, physical values and registered
logical payloads fail even when the predicate would reject their rows. Nested
range and fused table scans both stop on cancellation during evaluation.

[`subquery_reference.py`](../scripts/subquery_reference.py) runs all 36 corpus
records with both consumers and checkpoint/WAL durability. Both engines must
match expected values or reject the specified erroneous query; the script does
not skip unsupported statements. Explicit output aliases remove shell-specific
expression names from comparisons. Each configuration also reads the other
engine's resulting file and continues subquery mutations in both engines.
These checks do not establish general decorrelation, LATERAL, ANY/ALL, compound
grouped capture or volatile/external-effect history compatibility.

## Limits of the evidence

These tests establish correctness only for their covered subset. They do not establish general DuckDB file/version compatibility, crash consistency under power loss, arbitrary concurrent-history conformance, exhaustive parser semantics, fuzz safety, memory/spill correctness, extension interoperability, or performance parity. The SQLLogicTest runner supports a deliberately small directive subset and rejects unknown directives instead of silently counting them as passes.

The implementation has not met the broader [rewrite workload acceptance requirements](../specs/testing/rewrite-workloads.md). Workload benchmarks and promotion budgets remain open; no benchmark or production-readiness claim follows from these checks.

## Execution measurements

`python3 scripts/run_benchmarks.py --report execution-benchmark.json` builds the release benchmark driver and compares pull and eager result delivery for scans, filtering, aggregation, indexed point queries, and first-batch termination. Configuration, selected adapters, source/executable hashes, toolchain, platform and every sample are recorded. Setup and SQL parsing precede timing; prepared rebinding, execution and correctness checks are timed, with one warmup per case. The recorded run is an in-memory experiment, not a DuckDB performance comparison or promotion budget. It does not measure peak memory, disk I/O, parallel scheduling or spill.

## Compression measurements

`python3 scripts/run_benchmarks.py --suite compression --rows 500000 --iterations 9 --report compression-benchmark.json` compares scalar and word bitpacking through checked registry dispatch. Ten cases cover constant, arithmetic delta, delta/frame packing, widths 0–127, and BIGINT/HUGEINT. Every decoded value is checked; dispatch, allocation and physical type validation are timed. Input generation is outside the timed region. Three warmups precede measurement, and adapter order alternates each iteration to reduce drift. Configuration, samples, adapter selections and source/executable provenance appear in the recorded run.

The selection budget is set before measurement: the word implementation's median may be at most 1.25 times the scalar median in each covered case. The recorded run passes this budget. This narrow budget concerns decoder selection only. It does not measure the extraction boundary against the previous engine, full file opening, peak memory, I/O, encoding, or DuckDB performance. The complete rewrite's workload and structural overhead budgets remain open.

## Cast adapter and structural measurements

The casting change uses the saved execution baseline, recorded before implementation, with a per-workload median elapsed-time limit of 1.25 times baseline. The baseline contains five samples per workload, 50,000 input rows and batch size 256. This gate concerns the current serial in-memory workloads only; it is separate from the broader rewrite acceptance criteria. The comparison report passes all ten cases; the largest median ratio is 1.086, below the predeclared 1.25 limit. It records each ratio and the source/executable provenance of both runs. Reproduce the check with `python3 scripts/compare_execution_benchmarks.py docs/cast-seam-baseline.json docs/type-seam-baseline.json --report docs/cast-seam-comparison.json`. Five samples per case do not establish statistical confidence or a general performance claim.

`python3 scripts/run_benchmarks.py --suite casts --rows 500000 --iterations 9 --report docs/cast-benchmark.json` compares both integer parsers through checked bound casts and prepared SQL aggregation. Three warmups precede measurements and adapter order alternates per iteration. Input construction and database setup precede timing; conversion, physical checks, allocation, checksums and SQL rebinding/execution are included as applicable. The recorded cast comparison passes both workloads for both adapters. This algorithm comparison has no promotion threshold and is not a DuckDB performance comparison.

## Type registry measurements

The type change uses the preserved execution baseline from the accepted cast change, with the same predeclared 1.25 per-workload median ratio limit, sample count and configuration. Early measurements exceeded the limit. Shared extension metadata restored the primitive Value footprint from 80 to 32 bytes; binary/IN expressions retain selected semantics, column projections share vectors, and one expression-simplification pass combines constant-cast folding with true-filter removal. Streams retain only necessary logical validators, keys reserve their complete component space, and the owned validated-plan contract removes duplicate runtime/optimizer validation. A trial validation cache supplied no measurable improvement and was removed. The final comparison passes all ten cases, with a largest median ratio of 1.100. Five samples establish neither confidence bounds nor broader workload acceptance. Reproduce the saved comparison with `python3 scripts/compare_execution_benchmarks.py docs/type-seam-baseline.json docs/date-seam-baseline.json --report docs/type-seam-comparison.json`.

`python3 scripts/run_benchmarks.py --suite types --rows 500000 --iterations 9 --report docs/type-benchmark.json` compares both ASCII implementations on equal text, early/late comparison differences, canonical keys and SQL grouping. Three warmups precede alternating adapter measurements. Validation, allocations and correctness checks are included; input construction and database setup precede timing. This adapter experiment has no promotion threshold and makes no DuckDB performance claim.

## DATE integration measurements

The DATE increment preserves the previous accepted execution run in
date-seam-baseline.json. The acceptance limit is a
predeclared per-case median ratio of 1.25, with the same 50,000 rows, five samples and batch
size 256. This gate covers existing workloads; it does not establish temporal
workload performance or complete DuckDB compatibility.

The DATE comparison passes all ten cases; the
largest median ratio is 1.183. Reproduce the gate with
`python3 scripts/compare_execution_benchmarks.py docs/date-seam-baseline.json docs/operator-seam-baseline.json --report docs/date-seam-comparison.json`.
Historical cast/type comparisons retain their original accepted runs. The
current execution, compression, cast and type reports record their measured build's
source and executable provenance. Five samples do not establish statistical
confidence or broader workload acceptance.

## Operator integration measurements

The operator change preserves the accepted DATE execution run in
operator-seam-baseline.json. Each existing
workload's median must remain at or below 1.25 times this baseline, using
50,000 rows, five samples, one warmup and batch size 256. This structural
gate does not establish complete workload or DuckDB performance acceptance.

The LIKE selection budget is also fixed before measurement: each greedy
adapter median must be at most 1.25 times its dynamic-programming counterpart.
Five workloads cover exact Unicode matching, wildcards, early mismatches,
repeated suffix trials, and prepared SQL filtering. The planned run uses
50,000 rows, nine samples, three warmups and alternating adapter order.
This tests the selected LIKE implementation, separately from structural
overhead and the broader rewrite workload requirements.

The operator structural comparison passes all
ten cases with a largest median ratio of 1.150. Reproduce it with
`python3 scripts/compare_execution_benchmarks.py docs/operator-seam-baseline.json docs/wal-seam-baseline.json --report docs/operator-seam-comparison.json`.
The historical DATE comparison uses the accepted run now preserved as the
operator baseline. The accepted operator run is now preserved in the WAL baseline.

`python3 scripts/run_benchmarks.py --suite operators --rows 50000 --iterations 9 --report docs/operator-benchmark.json` reproduces the
LIKE comparison. The initial implementation failed
the repeated-suffix case at 1.468. Literal UTF-8 prefixes now compare bytes
directly; wildcards and retries still advance by Unicode scalar boundaries.
The unchanged workloads pass with a largest greedy/dynamic ratio of 0.595.
The measurements include checked dispatch, result validation and correctness;
SQL includes prepared rebinding and filtering. They do not establish general
LIKE complexity, statistical confidence or DuckDB performance parity.


## WAL recovery acceptance budget

Before changing recovery, the accepted operator execution report is frozen in
wal-seam-baseline.json. Each of the ten existing
execution cases must remain within 1.25 times its baseline median, using
50,000 rows, five samples, one warmup and batch size 256. This protects the
existing execution path; it does not measure recovery throughput, I/O, peak
memory, crash-safe publication or full workload acceptance.


The WAL structural comparison passes all ten cases
with a largest median ratio of 1.063. Reproduce it with
`python3 scripts/compare_execution_benchmarks.py docs/wal-seam-baseline.json docs/writable-recovery-baseline.json --report docs/wal-seam-comparison.json`.
That accepted read-only recovery run is now preserved as the writable recovery
baseline; historical structural comparisons retain their frozen accepted inputs.


## Writable recovery acceptance budget

Before changing publication, the accepted read-only recovery execution run is
frozen in writable-recovery-baseline.json.
Each of the ten existing execution cases must remain within 1.25 times its
baseline median, using 50,000 rows, five samples, one warmup and batch size 256.
This protects the existing execution path; recovery I/O, peak memory and
full-workload performance need separate measurements.

The writable recovery comparison passes all
ten cases with a largest median ratio of 1.122, below the predeclared 1.25 limit.
That accepted run is now frozen as the logging baseline. Reproduce the saved
comparison with
`python3 scripts/compare_execution_benchmarks.py docs/writable-recovery-baseline.json docs/logging-seam-baseline.json --report docs/writable-recovery-comparison.json`.
Five samples do not establish statistical confidence or broader workload acceptance.

Historical comparisons retain their original accepted inputs.

## Transaction logging acceptance budget

Before changing the commit path, the accepted writable recovery execution run
is frozen in logging-seam-baseline.json. Each of
the ten existing execution cases must remain within 1.25 times its baseline
median, using 50,000 rows, five samples, one warmup and batch size 256. Logging
I/O, commit latency, peak memory and full-workload performance require separate
measurements; this gate protects the existing in-memory execution path.

The logging structural comparison passes all
ten cases with a largest median ratio of 1.235, below the predeclared 1.25 limit.
That accepted run is now frozen as the checkpoint baseline. Reproduce the saved
comparison with
`python3 scripts/compare_execution_benchmarks.py docs/logging-seam-baseline.json docs/checkpoint-seam-baseline.json --report docs/logging-seam-comparison.json`.
Five samples do not establish statistical confidence or general WAL performance.

## Online checkpoint acceptance budget

Before changing checkpoint scheduling and row-ID rebasing, the accepted logging
execution run is frozen in checkpoint-seam-baseline.json.
Each of the ten execution cases must remain within 1.25 times its baseline
median, using 50,000 rows, five samples, one warmup and batch size 256. This
protects the existing in-memory path; checkpoint I/O, pause time, peak memory
and full workload performance require separate measurement.

The checkpoint structural comparison passes
all ten cases with a largest median ratio of 1.039, below the predeclared 1.25
limit. Its accepted run is frozen as the subquery baseline. Reproduce the saved
comparison with
`python3 scripts/compare_execution_benchmarks.py docs/checkpoint-seam-baseline.json docs/subquery-seam-baseline.json --report docs/checkpoint-seam-comparison.json`.
Five samples do not establish statistical confidence or checkpoint performance.

## Subquery acceptance budget

Before changing expression execution and nested query binding, the accepted
checkpoint execution run is frozen in
subquery-seam-baseline.json. Each of the ten
existing execution cases must remain within 1.25 times its baseline median,
using 50,000 rows, five samples, one warmup and batch size 256. This gate protects
the existing in-memory path; subquery workloads, planning costs and memory use
need separate measurements. It does not establish full workload acceptance.

Before measuring the new subquery adapter suite, its streaming/materializing
median ratio is limited to 1.25 for each of six deterministic workloads, with
2,000 outer rows, 65 inner rows, nine samples, three warmups and batch size 256.
Initial syntax preparation and setup precede timing; binding, physical planning,
execution and result checks are included. This is a small serial adapter-selection gate, not a decorrelation,
concurrency or DuckDB performance comparison.

The initial unfused subquery run failed this
selection gate: correlated EXISTS had a streaming/materializing median ratio of
1.555. Single-row demand constructed and validated a chunk for every rejected
scan row. A selectable fused scan/filter operator now validates storage rows
directly and constructs only selected output chunks, retaining the same demand
and error contracts. The unchanged subquery workloads
pass all six gates, with a largest ratio of 1.000; correlated EXISTS is 0.774.
The benchmark runner records the report and returns a failing exit status for
any failed correctness or selection budget. Reproduce this run with
`python3 scripts/run_benchmarks.py --suite subqueries --rows 2000 --iterations 9 --batch-size 256 --report docs/subquery-benchmark.json`.

The subquery structural comparison passes all
ten existing execution cases, with a largest median ratio of 1.104 against the
frozen checkpoint run, below the predeclared 1.25 limit. Reproduce it with
`python3 scripts/run_benchmarks.py --suite execution --rows 50000 --iterations 5 --batch-size 256 --report docs/execution-benchmark.json`
followed by
`python3 scripts/compare_execution_benchmarks.py docs/subquery-seam-baseline.json docs/execution-benchmark.json --report docs/subquery-seam-comparison.json`.
These small serial measurements do not establish statistical confidence,
decorrelation performance or broader workload acceptance.

Final verification of this implementation passed all 137 Cargo tests with
`cargo test --offline --all-targets`, formatting with `cargo fmt --all --check`,
and Clippy with `cargo clippy --offline --all-targets -- -D warnings`. The release
shell passed all 37 independent reference checks, including all 144 subquery
record/configuration outcomes and continued file mutations in both engines.
The six current benchmark reports match the final benchmark source and binary;
their correctness checks and declared selection gates pass. The reference
report, its three process-test workers and the subquery corpus also match their
source/executable hashes. All nine structural comparisons retain matching
report and source/executable hashes for their recorded inputs. The full rewrite
and the broader workload acceptance requirements remain unfinished.
