# Compiled component, API, and extension tests

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

Compiled suites are assembled by [test/CMakeLists.txt](../../../duckdb/test/CMakeLists.txt) when `ENABLE_UNITTEST_CPP_TESTS` permits them, with additional platform/extension conditions. They share Catch and the helper library rather than each supplying a standalone runner.

| Directory/suite | Main verification responsibility |
| --- | --- |
| [test/api](../../../duckdb/test/api/) | Database/connection lifecycle, preparation, streaming/pending queries, results, relations, callbacks, configuration, profiling, caches, buffers and task behavior |
| [test/api/capi/v1](../../../duckdb/test/api/capi/v1/) | v1 C handles, types/values/chunks, prepared execution, functions, appending, Arrow and API errors |
| [test/api/capi/v2](../../../duckdb/test/api/capi/v2/) | Environment/database/result lifecycles, errors, arenas, identifiers, schemas/catalog, Arrow, vectors, values, function/cast/COPY/replacement-scan registration and static extension behavior |
| [tools/cpp/tests](../../../duckdb/tools/cpp/tests/) | Consumer C++ wrapper RAII/moves/borrows, API calls and registrations over v2 |
| [test/appender](../../../duckdb/test/appender/) | Append and flush semantics, transactions, constraints and data forms |
| [test/arrow](../../../duckdb/test/arrow/) | Arrow round-trips, nested/dictionary offsets, release/child movement, filter pushdown and output-version buffers |
| [test/catalog](../../../duckdb/test/catalog/) | Native metadata behavior beyond SQL expectations |
| [test/common](../../../duckdb/test/common/) | Types, strings, casts, allocators, memory reservations, caches, file systems, checksums and asynchronous I/O helpers |
| [test/optimizer](../../../duckdb/test/optimizer/) | Internal plan/statistics/optimizer behavior requiring C++ access |
| [test/serialize](../../../duckdb/test/serialize/) | Serialization of native objects and round-trip expectations |
| [test/sql](../../../duckdb/test/sql/) compiled cases | Threading, SQL execution and internal tests colocated with the SQL corpus |
| [test/logging](../../../duckdb/test/logging/) | Logging storage/callback contracts |
| [test/secrets](../../../duckdb/test/secrets/) | Secret storage/provider and lifecycle behavior |
| [test/encryption](../../../duckdb/test/encryption/), [test/mbedtls](../../../duckdb/test/mbedtls/) | Encryption and crypto-integration behavior |
| [test/persistence](../../../duckdb/test/persistence/) | Reopening, crashes, sequence durability and process-oriented persistence behavior |
| [test/memoryleak](../../../duckdb/test/memoryleak/) | Repeated operations driven by the separate RSS supervisor |
| [test/ossfuzz](../../../duckdb/test/ossfuzz/) | Replayed crash/internal-error corpus |

## API test boundaries

C++ wrapper test sources remain next to the wrapper for packaging, but [test/api/cpp/CMakeLists.txt](../../../duckdb/test/api/cpp/CMakeLists.txt) compiles them into the main `unittest`. They are not a separately invoked CTest suite in the normal root build.

The v2 static extension fixture is compiled in its own translation unit because its entrypoint macro emits fixed names incompatible with a unity translation unit. API test coverage includes successful behavior and invalid arguments, ownership/lifecycle failures, callback errors and cancellation. Individual tests and build lists define the actual cases; the API's breadth must not be mistaken for proof that every function is exhaustively tested.

## ADBC harness

[test_adbc.cpp](../../../duckdb/test/api/adbc/test_adbc.cpp) reads `DUCKDB_INSTALL_LIB`, supplies it to the driver manager, and uses the `duckdb_adbc_init` entrypoint. The harness creates databases/connections/statements, executes queries and ingestion operations, and validates the resulting status/data. A library-path prerequisite is part of running this suite correctly.

## Extension fixtures

[test/extension/CMakeLists.txt](../../../duckdb/test/extension/CMakeLists.txt) builds generic loadable-extension demos, grammar demos, explicit-schema/alias demos, alternate implementations, the C++-over-v2 `cpp_api_demo` and an initialization-failure variant. It also builds optimizer demos on supported platforms and the debug filesystem both as a loadable extension and a linked test object.

These fixtures test loading as a real binary boundary: symbol/entrypoint choice, registered behavior, failure during initialization, schema routing and ABI dispatch. A statically registered callback test alone cannot cover every loadable-extension failure mode.

`DebugFileSystem` wraps an underlying filesystem and injects configurable open/read/write latency with randomization/seed controls. It supports deterministic or varied I/O scheduling tests while forwarding normal filesystem semantics. Its implementation is under [test/extension/debug_fs](../../../duckdb/test/extension/debug_fs/).

The separately named [run_extension_medata_tests.sh](../../../duckdb/scripts/run_extension_medata_tests.sh) builds local repositories with updated extensions, wrong platform/version metadata, missing or malformed install-info files, and direct-install artifacts. It also populates the configured MinIO test repository and runs [update_extensions_ci.test](../../../duckdb/test/extension/update_extensions_ci.test) with the required environment. The filename's `medata` spelling is literal. It rebuilds/replaces `build/debug` during scenario construction and writes external test-repository state, so it is a dedicated integration-environment harness rather than an ordinary local SQL test command.

## Platform and build exclusions

Native persistence tests are included only on supported non-Windows/non-Sun builds. Some tests require TPCH/TPCDS linkage, enabled threads, or non-TSAN builds. `ENABLE_UNITTEST_CPP_TESTS=OFF` retains SQL infrastructure while omitting the normal compiled-component population. Coverage should always identify these exclusions.

The historical README reference to `test/parallel_csv/test_parallel_csv.cpp` is not a present harness path. CSV coverage in this tree is in the SQL/COPY corpus and its actual compiled test/build registrations.
