# Test configuration, verification, and isolation

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Test configuration schema

[TestConfiguration](../../../duckdb/test/helpers/test_config.cpp) centralizes runner options and their JSON/CLI/environment forms. Configuration controls initial databases, initialization/load SQL, storage/restart modes, verification, skips, tags, environment values, result display and temporary-directory lifecycle.

`DUCKDB_TEST_<NAME>` supplies option fallbacks; CLI options override those fallbacks. A config file loaded later can override earlier values. Argument/config order therefore matters. Multiple `--test-config` values passed to the Python wrapper produce independent runs, not one merged configuration.

Representative configuration dimensions:

| Family | Configurations in this checkout | Intended coverage |
| --- | --- | --- |
| Query transformations | `verify_statement_copy`, `verify_statement_to_string`, `verify_statement_explain`, `verify_statement_prepare`, `verify_statement_serialization`, `verify_serializer` | Copy, parse/render, explain, prepare and serialization preserve query behavior |
| Binding/optimizer | `disable_optimizer`, `verify_stats`, `verification_projection`, `verify_column_bindings`, `heap_based_parser`, `delim_join_as_cte` | Alternate compilation and validation paths |
| Vectors/expression execution | `internal_vector_serialization`, `internal_vector_verification`, `variant_vector`, `shredded_vector`, `variant_order_verification`, `vector_size_512` | Physical representation, nested data and vector boundary assumptions |
| Operators/functions | `force_external`, `disable_caching_operators`, `verify_fetch_row`, `verify_aggregate_state_export`, `verify_functions`, `hash_zero` | Spill, caching, point fetch, aggregate states, function and hash assumptions |
| Persistence | `force_storage`, `force_storage_restart`, `force_storage_mmap`, `wal_verification`, `vacuum_rebuild_indexes_force_storage` | Persistent equivalents, repeated reopen, mmap, WAL and index rebuilding |
| Storage formats | `latest_storage`, `storage_compatibility`, `v1_storage`, versioned `serialization_bwc_*` plus `serialization_bwc_base` | Different persisted representations/compatibility targets |
| Block and compression | `block_verification`, `block_verification_latest`, `block_size_16kB`, `latest_storage_block_size_16kB`, `v1_storage_block_size_16kB`, `verify_compression`, `initial_column_segment_size` | Block/segment boundaries and codec verification |
| Memory/prefetch | `block_allocator_100mib`, `compressed_in_memory`, `prefetch_all_storage` | Allocation limits, compressed memory tables and prefetch paths |
| Environment/isolation | `no_local_filesystem`, `one_schema_per_test`, `encryption` | File-system abstraction, namespace assumptions, encryption |
| Specialized selection/build | `threadsan`, `release_assertions`, `stats_dependent_tests` | Compatible subsets and build-dependent checks |

The versioned serialization configs span the historical versions named in [test/configs](../../../duckdb/test/configs/). Not every JSON file is part of `make test_configs`, and not every JSON file there uses the native runner's schema. In particular, `io_metrics.json` belongs exclusively to the independent I/O harness.

The Makefile defines four standard configuration groups: query verification, execution, persistence and storage engine. It explicitly enumerates their members, including skip rules and inherited skip files through the underlying JSON. A skipped path is an exception to that configuration's coverage, not proof that its feature works under the configuration.

[test_config_compare.py](../../../duckdb/scripts/test_config_compare.py) compares skip lists by reason and path; [cleanup_config_skip_tests.py](../../../duckdb/scripts/cleanup_config_skip_tests.py) removes redundant inherited skip entries. These are configuration maintenance tools, not database test runners, and the cleanup tool edits configuration files.

## Runtime verification as a harness

Verification settings cause the engine to use or check alternative representations and execution paths while ordinary SQL tests provide the expected-result oracle. Examples include round-tripping statements/plans, executing prepared forms, forcing vector representations, enabling internal consistency checks and disabling optimizations.

This is differential or metamorphic coverage: it tests the relationship between executions/representations in addition to the script's expected result. It does not provide an independent reference database for every SQL query. [AGENTS.md](../../../duckdb/AGENTS.md) directs new tests away from adding `PRAGMA enable_verification`; the explicit configuration mechanisms are the current test organization.

## Directory and environment contract

| Variable/token | Resolved meaning |
| --- | --- |
| `WORKING_DIR` | Runner working directory; normally the source root |
| `BUILD_DIR` | Build location derived from the native runner |
| `TEST_NAME`, `TEST_NAME__NO_SLASH` | Full registered test name and slash-replaced form |
| `TEST_ID` | Sanitized full test name, including suffix, usable as one path component |
| `TEST_UUID` | Per-test invocation identifier |
| `RUN_ID` | Shared identity for one run, caller-selected or generated |
| `DATA_DIR` / `LOCAL_DATA_DIR` | Fixture location and guaranteed-local fixture location |
| `TEMP_DIR_ROOT` | Root of test scratch storage |
| `{TEST_DIR}` / `TEMP_DIR` | Per-test scratch location; synonymous |
| `TEMP_DIR_ABSOLUTE` | Absolute form for tests that require one |
| `LOCAL_TEMP_DIR` | Guaranteed-local scratch even when the primary root is remote |
| `CATALOG_DIR` | Per-test catalog path; materialized only when needed |

The default layout is `TEMP_DIR_ROOT/[RUN_ID]/[TEST_ID]`, with optional run/test identity levels. `--temp-dir-destroy` is `never`, `on-success`, or `always`, defaulting to `on-success`. Cleanup respects whether the run created or merely adopted a directory. Database files have a separate `--database-destroy` disposition. Remote roots are not recursively created/deleted by the native runner; their destroy mode is clamped to `never`.

The runner establishes an isolated HOME/USERPROFILE once per invocation unless `--keep-home` is selected. This isolates extension and secret state during tests. That is behavior of the test executable, not a build-time environment requirement for an embedding application.

Reserved variables cannot be overwritten through arbitrary `test_env`/passthrough entries. Missing `--env-passthrough` values fail startup; a missing `require-env` ordinarily skips the individual test. Relative data paths follow a test's working directory; explicit absolute/remote data roots remain anchored. Out-of-tree extension tests change directory to their source and receive adjusted absolute scratch paths.

Authoritative contract: [test/README.md](../../../duckdb/test/README.md), [test_helpers.cpp](../../../duckdb/test/helpers/test_helpers.cpp), [test_config.cpp](../../../duckdb/test/helpers/test_config.cpp).

## Effective-configuration review procedure

For a reproducible run, resolve the build first, then option defaults/environment, native argv/config loading order, SQL initialization settings, and per-script directives. These layers are related but not one interchangeable JSON map. A wrapper configuration sweep launches independent native runs; a script's SQL SET changes engine state within its execution.

The JSON files can inherit skip lists and carry reasons. Review both effective options and effective omissions when adding a configuration to CI. An expected-error skip can mask a real regression if its pattern is too broad. A no-local-filesystem or forced-restart run is valuable only for the tests that actually reach the intended capability after requirements/skips are applied.

## Isolation invariants

Scratch paths are derived from invocation and test identity to avoid collisions among workers and repeated runs. Explicitly adopting an existing path is different from creating it, and cleanup must respect that ownership. Remote paths have different lifecycle capabilities from local directories. Tests must not use the runner's isolated HOME as a generic shared artifact store or assume a user extension/secret installation remains visible.

Environment passthrough is explicit so remote credentials and external test prerequisites can be supplied without overwriting reserved test variables. Failure artifacts may contain substituted values or SQL, so retained logs/databases need appropriate handling when tests consume sensitive configuration.

## Coverage and acceptance requirements

When changing the configuration implementation, test precedence, type conversion, invalid keys/values, includes/skip inheritance, missing environment values, source/build path derivation, and directory disposition on both success and failure. The temporary-directory pytest suite is a direct lifecycle oracle; ordinary SQL output is not.

When changing an engine component, select configurations based on the mechanism affected: vector layout for expressions, serialization for plans, storage restart for durability, forced external for memory-intensive operators, and thread instrumentation for concurrency. Do not run unrelated matrices merely to create a larger pass count. [Coverage](coverage.md) maps components to these checks, and [runbook](runbook.md) provides invocation examples.
