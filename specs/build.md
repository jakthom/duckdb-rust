# Build system and generated artifacts

[Specification index](README.md) · [Testing](testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

The [root CMake definition](../../duckdb/CMakeLists.txt) requires C++17 or newer. The [Makefile](../../duckdb/Makefile) selects build directories, compiler/configuration options, extensions, and test workflows; CMake defines the actual targets. Unity builds combine source files by default and can be disabled for development or build diagnostics.

## Important outputs

| Target or artifact | Definition | Engineering role |
| --- | --- | --- |
| `duckdb` shared library | [src/CMakeLists.txt](../../duckdb/src/CMakeLists.txt) | Embedding and dynamic linking |
| `duckdb_static` | [src/CMakeLists.txt](../../duckdb/src/CMakeLists.txt) | Static embedding and selected test/platform builds |
| Shell executable, normally `build/<configuration>/duckdb` | [tools/shell/CMakeLists.txt](../../duckdb/tools/shell/CMakeLists.txt) | Interactive and batch SQL client |
| `unittest` and generated `test/run` wrappers | [test/CMakeLists.txt](../../duckdb/test/CMakeLists.txt) | Native test engine and process orchestrator entry points |
| `benchmark_runner` | [benchmark/CMakeLists.txt](../../duckdb/benchmark/CMakeLists.txt) | Optional compiled and interpreted benchmark harness |
| `plan_serializer` | [tools/CMakeLists.txt](../../duckdb/tools/CMakeLists.txt), [utility](../../duckdb/tools/utils/plan_serializer.cpp) | Logical-plan serialization utility; built with the shell option |
| `duckdb_cpp_api` | [tools/cpp/CMakeLists.txt](../../duckdb/tools/cpp/CMakeLists.txt) | C++ convenience layer over C API v2, compiled into consumers |
| `duckdb_cpp_api_loadable` | [tools/cpp/CMakeLists.txt](../../duckdb/tools/cpp/CMakeLists.txt) | Wrapper variant dispatching through a loadable extension's vtable |
| `*.duckdb_extension` | [extension build helpers](../../duckdb/extension/extension_build_tools.cmake) | Loadable binary plus compatibility/signature metadata |

The test target links the shared engine on supported non-Windows builds and uses the static engine where internal symbol visibility requires it. It explicitly links supporting libraries needed by tests. The existence of a source file alone does not mean it is in the test binary: the CMake object lists and feature/platform conditions determine that.

## Configuration axes

Build modes include `debug`, `release`, `reldebug`, `relassert`, and `codecov`. Relevant CMake options include `BUILD_UNITTESTS`, `ENABLE_UNITTEST_CPP_TESTS`, `BUILD_BENCHMARKS`, `BUILD_SHELL`, `DISABLE_THREADS`, `DISABLE_EXTENSION_LOAD`, `ENABLE_SANITIZER`, `ENABLE_UBSAN`, `ENABLE_THREAD_SANITIZER`, `FORCE_ASSERT`, and `COVERAGE`.

Correctness stress builds additionally alter vector size, string inlining, hashing, pointer salt, unpinned-buffer destruction, allocation instrumentation, and asynchronous source/sink behavior. These are separate coverage dimensions: an ordinary release run does not exercise all of them.

The Make variable `BUILD_BENCHMARK=1` maps to CMake's plural `BUILD_BENCHMARKS`. The base [extension configuration](../../duckdb/extension/extension_config.cmake) loads `core_functions` and `parquet`; expanded CI configurations add other extensions. Linking and inclusion can still be changed by build options. [In-tree](../../duckdb/.github/config/in_tree_extensions.cmake) and [out-of-tree](../../duckdb/.github/config/out_of_tree_extensions.cmake) configurations compose domain-specific configuration files.

## Generated-source contracts

| Authoritative input | Generated surface | Generator or workflow |
| --- | --- | --- |
| [api_spec/v1](../../duckdb/api_spec/v1/) | `duckdb.h`, v1 extension header and function-pointer table | [capi_v1_regen.sh](../../duckdb/scripts/capi_v1_regen.sh), capigen |
| [api_spec/v2](../../duckdb/api_spec/v2/) | `duckdb_v2.h`, v2 extension/bridge surfaces | [capi_v2_regen.sh](../../duckdb/scripts/capi_v2_regen.sh), capigen |
| [PEG grammar](../../duckdb/src/parser/peg/grammar/) | Compiled grammar and transformer support | [parser scripts](../../duckdb/scripts/parser/), Make parser-grammar targets |
| [settings.json](../../duckdb/src/common/settings.json) | Setting declarations, scopes, and implementation support | [generate_settings.py](../../duckdb/scripts/generate_settings.py) |
| [metrics.json](../../duckdb/src/common/metrics.json) | Metric identifiers and metadata | [generate_metrics.py](../../duckdb/scripts/generate_metrics.py) |
| [serialization schemas](../../duckdb/src/include/duckdb/storage/serialization/) | Serialization/deserialization implementations | [generate_serialization.py](../../duckdb/scripts/generate_serialization.py) |
| [version_map.json](../../duckdb/src/storage/version_map.json) | Storage-version information | [generate_storage_info.py](../../duckdb/scripts/generate_storage_info.py) |
| Function declaration/registration inputs | Built-in registration lists | [generate_functions.py](../../duckdb/scripts/generate_functions.py) |

Generated files are a derived interface, not an independent place to change semantics. `make generate-files` performs the generator sequence shown in the Makefile, including the v1 API regeneration and formatting. Inspect the separate v2 regeneration script when changing the v2 specification; do not assume the aggregate target regenerates every API generation path.

## Third-party linkage boundary

The core link list in [src/CMakeLists.txt](../../duckdb/src/CMakeLists.txt) includes FSST, fmt, RE2, miniz, utf8proc, HyperLogLog, FastPFor, a skip-list library, mbedTLS, yyjson and Zstandard, with jemalloc where enabled. They supply compression, formatting, regular expressions, Unicode, probabilistic/statistical structures, cryptography, JSON and allocation services. Other extension-specific dependencies are selected by their own build configurations.

Catch is the native test framework. SQLite-origin SQLLogicTest data and its DuckDB interpreter integration are separate from replacing the engine with SQLite. Vendored-library files and their upstream tests are not automatically additional DuckDB test executables; integration is determined by the root and extension CMake definitions.
