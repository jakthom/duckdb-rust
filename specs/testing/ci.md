# Build gates and continuous integration

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Non-runtime gates

| Check | Entry point | Property checked |
| --- | --- | --- |
| Formatting and generated drift | `make format-check`, generator workflows | Committed sources match format and generated definitions |
| Parser grammar generation | `make parser-grammar`, [parser tooling](../../../duckdb/scripts/parser/) | Grammar and generated transformer support build consistently |
| C API generation/versioning | capigen scripts and [api_spec](../../../duckdb/api_spec/) | Spec validity and generated interface consistency |
| Enum integrity | `make enum-integrity-check` | C enum definition integrity |
| Extension patch configuration | `make extension-patch-check` | Configured patch/source consistency |
| Exported/banned symbols | `make symbol-checks` | Avoid accidental exports and prohibited linked symbols |
| Individual compilation | `make test_compile`, [test_compile.py](../../../duckdb/scripts/test_compile.py) | Source files compile without relying on incidental unity-build context |
| clang-tidy | `make tidy-check` and related targets | Static C++ diagnostics |
| Coverage collection | Instrumented build plus wrapper `--coverage-report` | Executed source/branch evidence; not a correctness oracle on its own |
| Packaging/version logic | Python self-tests and release artifact checks | Artifact names, layouts, versions and exported surfaces |
| Architecture/reconfigure checks | [test_architecture_flags.cmake](../../../duckdb/scripts/ci/test_architecture_flags.cmake), nightly reconfigure job | Build-option/architecture selection and repeat configuration behavior |
| CodeQL/Coverity | [NightlyTests.yml](../../../duckdb/.github/workflows/NightlyTests.yml), [coverity.yml](../../../duckdb/.github/workflows/coverity.yml) | Static/security analysis in configured external services |

Parser corpus utilities use imported `duckdb_sqllogictest` or `sqllogictest` Python packages. `test_peg_parser.py` extracts SQL from selected scripts and invokes `check_peg_parser` through the CLI; that function is registered by the [autocomplete extension](../../../duckdb/extension/autocomplete/autocomplete_extension.cpp). It supports file/list/all-corpus selection and optional external-extension corpus retrieval. `parser_test.py` instead checks parsing of a SQLLogicTest file by its imported Python parser. These are optional developer tools and cannot be assumed to work from a fresh checkout without their dependencies. Some historical utilities refer to older repository layouts; source presence alone does not certify a maintained CI entry point.

An additional [coverage_check.sh](../../../duckdb/scripts/coverage_check.sh) path builds with compiler `--coverage`, runs selected native and shell tests, captures LCOV and generates HTML. This is separate from the wrapper's LLVM profiling path. The script's call to [check_coverage.py](../../../duckdb/scripts/check_coverage.py) is commented out at this commit, so generating its coverage report does not establish enforcement of that script's numeric coverage threshold.

## CI workflow responsibilities

| Workflow | Database/test responsibility |
| --- | --- |
| [Main.yml](../../../duckdb/.github/workflows/Main.yml) | Main build/test orchestration, changed-test selection, release/assertion builds, configuration tests, TSAN, vector sizes, shell tests and selected coverage |
| [OSX.yml](../../../duckdb/.github/workflows/OSX.yml) | macOS builds plus native/shell/stdin-runner verification |
| [Windows.yml](../../../duckdb/.github/workflows/Windows.yml) | Windows builds/artifacts and shell/native verification according to job configuration |
| [Swift.yml](../../../duckdb/.github/workflows/Swift.yml) | Generated Swift package and selected Apple-platform tests |
| [NightlyTests.yml](../../../duckdb/.github/workflows/NightlyTests.yml) | Memory growth, Valgrind, imported SQLite suite, initialization, alternate builds/sizes, BWC, Wasm-related and reconfigure checks |
| [ExtendedTests.yml](../../../duckdb/.github/workflows/ExtendedTests.yml) | Broad configuration runs and compiler/LTO performance comparisons |
| [Regression.yml](../../../duckdb/.github/workflows/Regression.yml) | Baseline/current timing, storage/binary size and plan-cost jobs |
| [ExtraTests.yml](../../../duckdb/.github/workflows/ExtraTests.yml) | Explicit additional regression execution |
| [Extensions.yml](../../../duckdb/.github/workflows/Extensions.yml) | Extension build/integration, installation/autoloading and repository testing |
| [DockerTests.yml](../../../duckdb/.github/workflows/DockerTests.yml) | Containerized Linux test/build variants |
| [Android.yml](../../../duckdb/.github/workflows/Android.yml) | Android platform build/integration path; do not infer desktop-suite equivalence |
| [cifuzz.yml](../../../duckdb/.github/workflows/cifuzz.yml) | External OSS-Fuzz build/run integration |
| [_extension_client_tests.yml](../../../duckdb/.github/workflows/_extension_client_tests.yml) | Reusable workflow in extension-template context; calls that project's Python build/test targets |
| [OnTag.yml](../../../duckdb/.github/workflows/OnTag.yml), [StagedUpload.yml](../../../duckdb/.github/workflows/StagedUpload.yml), [SwiftRelease.yml](../../../duckdb/.github/workflows/SwiftRelease.yml) | Release/package staging, with their configured checks; not standalone SQL correctness suites |

Issue mirroring, label/approval automation, stale-issue handling and draft/documentation checks are repository administration rather than database test harnesses. They are intentionally not counted as additional engine verification mechanisms.

[scripts/ci/job_stages.py](../../../duckdb/scripts/ci/job_stages.py) defines distinct PR, main/nightly, merge-group and release selections, with explicit overrides and skip-test behavior. A workflow file's name does not prove that it runs on every change or on a timer: inspect its `on`, job conditions and selected stage. Some actions/workflows are supplied by other repositories, so their internals and actual outcomes are outside this checkout's evidence.
