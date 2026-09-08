# Component-to-verification coverage matrix

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

| Component/change surface | Primary correctness evidence | Additional failure dimensions |
| --- | --- | --- |
| PEG grammar/tokenizer/transformer | `test/sql/parser`, `test/sql/peg_parser`, native parse/API tests | Invalid UTF-8, depth, malformed syntax, heap parser, grammar extension, fuzzing |
| Binder/types/casts | `test/sql/binder`, `cast`, `types`, `prepared`, `subquery` | NULL/nested values, unresolved parameters, column bindings, statement round-trip |
| Optimizer/decorrelation/statistics | `test/optimizer`, `test/sql/optimizer`, joins/CTEs/subqueries | Optimizer disabled, verification configs, volatility/effects, plan cost/performance |
| Vectors/expression execution | Native common/API/vector tests and SQL type/function corpus | Dictionary/constant/sequence/shredded vectors, small vector sizes, sanitizers |
| Pipelines/scheduler/cancellation | Native pending/partial/streaming tests, SQL parallelism | TSAN, interquery/intraquery, forced async blocking, delayed filesystem, early consumer stop |
| Joins/aggregation/windows/sort | SQL family suites and benchmark correctness | Spill, skew, NULLs, empty inputs, shared state/finalization, memory limits |
| Catalog/DDL/dependencies | SQL catalog/alter/create/trigger, native catalog/API tests | Transaction rollback, concurrent readers, prepared statements, serialization |
| Native storage/WAL/checkpoint | SQL storage/transactions, native persistence | Forced restart, concurrent checkpoint, crash replay, initialization, old-version compatibility |
| Compression/nested storage | SQL storage/compression and type suites | 16 KiB blocks, partial segments, fetch/scan equivalence, encryption, variant/shredded forms |
| ART/index constraints | SQL index/constraints/update/delete/upsert | Rollback, checkpoint/restart, memory limits, conflict handling |
| Buffer pool/spill/cache | Native buffer/cache/memory tests, SQL out-of-core | Destroy unpinned blocks, allocation tracking, RSS growth, file lifetime |
| CSV/Parquet/JSON/multi-file | SQL COPY/format suites, dedicated Parquet corpus | Parallel boundaries, malformed files, schema reconciliation, external filesystem, fuzzing |
| Arrow/ADBC | Native Arrow and API/driver tests | Release ownership, dictionaries, nested offsets, empty streams, external library loading |
| C v1/C v2/C++ wrapper | Respective API suites and extension fixtures | Invalid handles, borrowed data, moves/destruction, error translation, ABI slots |
| Extensions/grammar/storage plugins | Loadable/static demos and registered extension SQL paths | Autoload/install, incompatible binaries, missing dependencies, initialization failures |
| Settings/secrets/external resources | Native API/secrets and SQL settings/resource cases | Scope, cleanup, attachment lifetime, local/remote state |
| Logging/profiling/I/O | Native/SQL logging/profiler tests | Independent I/O shim, known skipped instrumentation defects, async attribution |
| Shell | Shell pytest | Process arguments, stdout/stderr, render modes, terminal features, platform differences |
| Swift binding | Xcode test suite | Foundation conversions and Apple-platform packaging |
| Native test harness | Stdin/temp-dir/tag tests plus runner self-tests | Post-exit cleanup, missing env, exact file lists, skip reporting |
| Python orchestration/regression tooling | `scripts/ci` and `scripts/regression` unit tests | Timeout, retries, batching, false-green selection, thresholds and artifact isolation |
| Build/generated/API packaging | Format/generation/compile/symbol/ABI/package checks | Compiler/platform variants, unity assumptions, stale generated code |

Rows name applicable evidence, not a claim that all combinations ran or passed. For an actual change, select checks according to which contracts changed and record their outcomes separately.
