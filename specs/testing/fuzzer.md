# Fuzzers, input mutation, and crash replay

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Harness families and build boundary

Fuzzing in this checkout consists of several different systems. Raw-byte targets expose `LLVMFuzzerTestOneInput`; native Catch cases replay stored SQL or run storage operation sequences; reduced SQLSmith/DuckFuzz cases use SQLLogicTest. These families do not share one generator or one failure oracle.

| Harness | Input | Engine lifetime | Primary oracle |
| --- | --- | --- | --- |
| SQL byte target | Entire input interpreted as SQL bytes | New in-memory database/connection per input | Crashes/instrumentation; selected diagnostic check returns 1 |
| CSV/JSON byte target | Selector byte plus bounded payload | New database/connection per input | Crashes/instrumentation; ordinary result errors ignored |
| Parquet byte target | Candidate file bytes | Process-global database/connection reused | Crashes/instrumentation; ordinary result errors ignored |
| Stored OSS-Fuzz replay | Files under `test/ossfuzz/cases` | New database/connection per case | Catch assertion rejects selected internal-error diagnostics |
| Generated SQL regressions | Reduced `.test`/`.test_slow` scripts | SQLLogicTest lifecycle | Scripted expected success/error/results |
| Storage operation fuzzer | Random action sequence plus injected filesystem failure | Reopen database for each action | Expected errors and pre-action checksum after reopen |

[test/ossfuzz/CMakeLists.txt](../../../duckdb/test/ossfuzz/CMakeLists.txt) builds the replay source into the native test target, not the three byte targets as separate local libFuzzer executables. The external OSS-Fuzz project controls active fuzzer builds/instrumentation. No local source inspection establishes its current seeds, deployed target list, campaign coverage, or results.

## SQL byte target

[parse_fuzz_test.cpp](../../../duckdb/test/ossfuzz/parse_fuzz_test.cpp) constructs a length-aware `std::string` from the input and executes `Connection::Query`. Despite its filename, the reachable path is parsing, binding, planning, and execution of whatever SQL the fresh database and enabled capabilities accept; it is not an isolated parser entry point.

The target examines result errors for `Unoptimized Result differs from original result!` and `INTERNAL`, returning 1 on a match. It otherwise returns 0 and catches `std::exception`. A nonzero callback return is not a standard libFuzzer crash assertion, so this code must not be credited with a guaranteed semantic-internal-error finding merely because it returns 1. Actual sanitizer failures, assertions, and crashes are independent signals. The source does not explicitly enable a universal differential verifier for every input.

A fresh engine per input reduces cross-input SQL state retention but adds initialization cost. Arbitrary SQL can perform more work than its input byte length suggests; the target supplies no general process timeout or memory budget. Campaign infrastructure must supply resource containment and record build/extension settings.

## CSV/JSON multiplexed target

[csv_json_fuzz_test.cpp](../../../duckdb/test/ossfuzz/csv_json_fuzz_test.cpp) ignores inputs shorter than two bytes. The first byte selects `selector % 4`; the remaining payload is truncated to 8,192 bytes. It disables the progress bar and sets `max_expression_depth=50`. A comment mentions memory/row limits, but there is no explicit engine memory limit or wall-clock timeout in this target.

| Selector | Executed path | Important restrictions |
| --- | --- | --- |
| 0 | `read_csv_auto`, `sample_size=1`, `LIMIT 10` | Dialect/schema inference and CSV scan |
| 1 | `read_csv` with three VARCHAR columns, `auto_detect=false`, `header=false`, `sample_size=1`, `LIMIT 10` | Explicit-schema scanner path |
| 2 | `json_valid`, `json_type`, `json_extract` at `$` | Single quotes doubled and NUL bytes removed before SQL construction |
| 3 | `read_json_auto`, `maximum_sample_files=1`, `sample_size=1`, `LIMIT 10` | JSON file inference/scan |

File modes write to the fixed path `/tmp/duckdb_fuzz_data`; cleanup removes it after the switch. File-open failure returns without exercising a reader, and write length is not validated here. Ordinary query errors are not inspected; caught C++ exceptions are tolerated. Missing reader/function extensions can therefore reduce coverage without an explicit semantic failure in this callback.

The payload cap, sampling, and LIMIT bound parts of the workload but do not prove a bound on all parser/allocation behavior. Scalar JSON mutation does not preserve every original byte because it removes NULs and escapes SQL quotes. Fixed temporary paths can collide across multiple processes sharing a temporary namespace, even if each individual callback runs serially.

## Parquet byte target

[parquet_fuzz_test.cpp](../../../duckdb/test/ossfuzz/parquet_fuzz_test.cpp) ignores inputs shorter than eight bytes, lazily constructs process-global database/connection pointers, sets one engine thread, and disables progress output. It writes the entire candidate to `/tmp/duckdb_parquet_fuzz.parquet`, checks the write length, and removes the file after execution.

The first query fully materializes `SELECT * FROM read_parquet(...)`, reaching schema/footer binding and eligible page/column decoding. The second query invokes `parquet_schema(...)`. Although a comment also mentions `parquet_metadata`, the callback does not issue that query. It does not set a payload cap, row limit, or query timeout. Compression can expand a small input into substantial work, so campaign-level limits remain necessary.

Result error values are discarded; C++ exceptions are caught independently around each query. Reusing the engine changes the isolation model relative to the SQL/CSV/JSON targets and can retain process-level caches or invalidated state. The source's performance and coverage comments are rationale, not measurements reproduced by this specification. The fixed path also needs process isolation in a multiworker campaign.

## Stored replay and reduced regressions

[test_ossfuzz.cpp](../../../duckdb/test/ossfuzz/test_ossfuzz.cpp) registers one hidden `[ossfuzz][.]` case per stored file. It reads the file, executes SQL in a new engine, and fails a Catch assertion for selected internal-error diagnostics. Normal SQL errors are allowed; there is no expected relational result for arbitrary stored inputs. Replay uses the C-string query call after reading the file, so it should not be assumed byte-for-byte equivalent to the length-aware fuzz target for embedded NULs.

[test/fuzzer](../../../duckdb/test/fuzzer/) contains historical reductions grouped by generator/source, including SQLSmith and DuckFuzz. Once expressed as SQLLogicTest, those cases can have much stronger specific result/error assertions than the original crash-only campaign. Active SQLSmith comes from the pinned [external configuration](../../../duckdb/.github/config/extensions/sqlsmith.cmake), with separate load-tests and linking decisions. Its generator/minimizer implementation is not present here.

The historical `make sqlsmith` recipe points to `build/debug/third_party/sqlsmith/sqlsmith`, but there is no corresponding source subtree in this checkout. It is not a verified active-generation recipe. Use the fetched extension's own supported interface when conducting such a campaign.

## Storage operation and fault-injection fuzzer

[test_storage_fuzz.cpp](../../../duckdb/test/common/test_storage_fuzz.cpp) defines two hidden `[storage][.]` cases, both gated by the `storage_fuzzer` test option. `FaultInjectionFileSystem` extends the local filesystem with one-shot WRITE and FSYNC faults protected by a mutex. Its trim implementation zero-fills ranges. `LazyFlushFileSystem` additionally buffers positioned writes until synchronization and constrains overlapping write ranges; it is used by the simple fault test, not the active randomized loop.

The simple case inserts 1,001,000 rows, injects an FSYNC failure during another large insert, checks the expected commit error, and verifies the old count both in the current connection and after reopening.

The randomized case starts a persistent `pig.db` with one INTEGER column, enables free-block trimming, disables checkpoint-on-shutdown, and initially skips checkpoints on commit. It generates 30 actions, inserts a table reset, then generates 30 more: 61 actions total. Actions toggle checkpoint behavior, insert 100 or 1,000,000 rows, update/delete ranges, replace the table, or attempt a million-row insert with an FSYNC fault. The implementation uses C `rand()`; no dedicated reproducible-seed interface is established by this file alone. The zero-offset delete branch can fall through to the faulted-write case, so the action enum label alone does not fully describe execution.

Before each action, it constructs a fresh fault-injection filesystem and reopens the database. The previous expected `bit_xor(hash(i))` checksum is compared with the reopened table. A mismatch triggers sorted-result diagnostic comparison and an assertion. Normal actions refresh the expected checksum; an expected fault preserves the previous checksum because the failed operation must not become durable. The `validate` helper asserts both whether an error occurred and the expected error substring.

This is a stateful durability/failure test, not malformed-byte mutation. The XOR hash is a probabilistic oracle, not collision-proof equality. The saved full result is used for diagnostics after a mismatch, not as an exhaustive equality check after every action. Failure-sequence logging and retained database/WAL files are critical for reproduction.

## Execution, findings, and coverage limits

From the DuckDB source root, a source-derived native replay selection is:

```bash
build/reldebug/test/unittest '[ossfuzz]'

# Explicitly select and enable the expensive storage cases.
DUCKDB_TEST_STORAGE_FUZZER=1 build/reldebug/test/unittest 'fuzzed storage test'
DUCKDB_TEST_STORAGE_FUZZER=1 build/reldebug/test/unittest 'simple fault injection storage test'
```

These commands were not executed for this documentation task. The [CIFuzz workflow](../../../duckdb/.github/workflows/cifuzz.yml) requests address, undefined, and memory sanitizer variants through external OSS-Fuzz actions, with 3,600 fuzzing seconds and failure artifact upload from `out/artifacts`. Its triggers and repository guard limit when it runs; the existence of a workflow is not evidence of a successful campaign.

For a finding, retain exact input bytes or the full operation sequence, source/extension revisions, instrumentation, resource limits, process isolation, stderr/stack trace, and any persistent files. Minimize while retaining the same failure oracle, then add a deterministic native or SQLLogicTest regression with the strongest appropriate assertion. A crash-free run does not establish SQL semantic correctness, broad storage corruption tolerance, writer coverage, or complete parser/format coverage. See [stress](stress.md), [compatibility](compatibility.md), and [coverage obligations](coverage.md).
