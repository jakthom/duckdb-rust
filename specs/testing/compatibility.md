# Storage, plan, API, and artifact compatibility

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

Compatibility is split into distinct contracts: database storage, serialized plans, C/API/extension ABI, and clients/packages. Passing one does not prove the others.

## Persistent storage compatibility

[test_storage_compatibility.py](../../../duckdb/scripts/test_storage_compatibility.py) accepts an old CLI or selected versions and a new `unittest` binary, with a compatibility target and test configuration. It uses current tests and persisted artifacts to compare behavior across versions. It can obtain older binaries and run work concurrently; results depend on the versions, selected SQL cases, format target and available extensions.

`make test_storage` selects the historical versions explicitly named in the Makefile. That list is a snapshot of CI coverage, not a universal compatibility guarantee for all DuckDB releases and all new features.

## Current serialized-plan BWC framework

[test/bwc/runner.py](../../../duckdb/test/bwc/runner.py) drives two CLI versions with the external `test_utils` extension. Its utility modules install/find clients/extensions, parse test inputs, manage runtime directories and generate per-step reports.

```text
Old CLI + matching test_utils
    serialize queries/plans and baseline results
                     |
             persisted plan/result files
                     |
New CLI + matching test_utils
    execute old serialized plans and write new results
                     |
           compare with baseline and report
```

Cached serialized plans/results can replace the old serialization step. Extension-version consistency is checked between the clients. The runner supports one old version or a supported-version set, selected tests/patterns, configurable parallelism and report generation.

[export_cache.py](../../../duckdb/test/bwc/export_cache.py), [update_cache.py](../../../duckdb/test/bwc/update_cache.py), [list_versions.py](../../../duckdb/test/bwc/list_versions.py), and [utils](../../../duckdb/test/bwc/utils/) support historical cache management. [NightlyTests.yml](../../../duckdb/.github/workflows/NightlyTests.yml) supplies cache downloads and external `test_utils`/Python prerequisites. This is not a self-contained offline suite in the checkout.

## Earlier serialized-plan path

[scripts/test_serialization_bwc.py](../../../duckdb/scripts/test_serialization_bwc.py) compares two source checkouts, can build their debug binaries, extracts SQL from test files, invokes the plan serialization utility and runs the native deserialization case. Native fixtures also live under [test/api/serialized_plans](../../../duckdb/test/api/serialized_plans/).

This path coexists with the newer `test/bwc` framework. Its existence does not imply that every modern statement or extension uses it, or that its external Python/parser prerequisites are installed. The exact current CI use must be read from workflow calls.

## API, extension, and package compatibility

[check_extension_abi.py](../../../duckdb/scripts/check_extension_abi.py) checks extension function-table layout against historical release interfaces. [verify_enum_integrity.py](../../../duckdb/scripts/verify_enum_integrity.py) checks generated C enum integrity. Exported/banned-symbol checks constrain the library boundary; [package tests](../../../duckdb/scripts/ci/test_package_release_artifact.py) verify packaging logic. These are separate from runtime extension demo tests.

[verify-extension-signing.sh](../../../duckdb/scripts/verify-extension-signing.sh) enumerates signed native/Wasm extension artifacts, separates the 256-byte signature footer, computes the extension hash and verifies it using OpenSSL and the supplied public key. It rejects an empty artifact set, undersized files and failed verification. This checks artifact authenticity/integrity; the metadata scenario harness separately checks install/update compatibility behavior.

The C++ wrapper has a standalone [package definition](../../../duckdb/tools/cpp/package/CMakeLists.txt) and [consumer example](../../../duckdb/tools/cpp/example/README.md). Swift package preparation/release workflows and the external extension client workflow provide additional integration checks. A generated header that compiles is not proof of binary slot compatibility or runtime resource semantics.

## Compatibility test data flow and artifacts

Each campaign needs explicit producer and consumer identities. For storage, record the writer binary, compatibility target, test configuration, generated database/WAL state, reader binary, and comparison query/output. For plans, record serialization version, matching `test_utils` builds, serialized plan and baseline-result files, execution output, and per-step reports. A stale artifact from a different writer can turn a valid test into a misleading pass or failure.

Cached plan/result artifacts reduce historical build costs but also freeze a particular producer environment and workload population. Updating a cache changes the test inputs and should not be treated as a harmless way to erase a failure. Compare the new artifact provenance and expected results before replacing a historical baseline.

## Failure classification

Distinguish infrastructure acquisition/build failures, missing extensions, unsupported features under the selected compatibility target, invalid artifacts, deserialization/open failures, execution failures, and result mismatches. A skipped unsupported feature does not prove compatibility for that feature. A successful open is weaker than correct execution over restored tables, indexes, and catalog objects.

C/extension ABI checks use a different artifact: interface declarations/function-table layouts rather than a database file. Signature verification establishes that the checked artifact matches a supplied key/signature contract; it does not establish SQL correctness or binary compatibility with every host. Package tests validate packaging behavior, while runtime consumer tests validate using the package.

## Engineering change requirements

For a serialization or storage change, identify the exact compatibility domain, update the schema/version gates, run same-build round trips, and run the relevant historical producer/consumer pairs. For a C API addition, regenerate headers/tables, verify enum/slot integrity, and execute lifecycle/callback tests. For extension metadata/loading changes, use the scenario builder and signing checks where applicable.

These are source-derived requirements, not a claim that historical binaries or external utilities were installed during this documentation task. See [serialization](../components/serialization.md), [durability](../components/durability.md), [extensions](../components/extensions.md), and [APIs](../components/apis.md) for the contracts being preserved.
