# Extension registration, loading, and compatibility

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Registration and loading

Extensions can register functions, types, casts, catalogs/storage, file systems, secrets, parsers/grammar, optimizers and other callbacks. Statically linked extensions and loadable extensions use related registration machinery, but their deployment and ABI contracts differ.

The extension manager/helper/loader handles discovery, installation repositories, metadata, initialization, compatibility and load state. Autoload/autoinstall behavior depends on the build and runtime settings. A query that references an extension-provided capability can therefore have installation/loading prerequisites.

| Binary interface | Compatibility model in this tree |
| --- | --- |
| Native internal C++ extension | Coupled to DuckDB's internal C++ interface and compatible build/version/platform |
| Versioned C extension (`C_STRUCT`) | Version-gated prefix of a stable function-pointer table |
| Unstable C extension (`C_STRUCT_UNSTABLE`) | Full selected interface pinned to the matching DuckDB version/build |
| C++ wrapper loadable extension | C++ convenience code routed through the C v2 extension table; demo targets use the pinned unstable path |

The v1 API lifecycle records `unstable`, `stable`, `deprecated`, and `removed` states. Stable extension slots are ordered by stabilization bands. Hiding deprecated names must not shift existing slots. API visibility, source compatibility and binary compatibility are related but distinct requirements.

Sources: [extension loading](../../../duckdb/src/main/extension/), [extension manager interface](../../../duckdb/src/include/duckdb/main/extension_manager.hpp), [distribution description](../../../duckdb/extension/ExtensionDistribution.md), [API versioning](../../../duckdb/api_spec/VERSIONING.md), [ABI checker](../../../duckdb/scripts/check_extension_abi.py).

## In-tree and configured external extensions

| In-tree code | Main purpose |
| --- | --- |
| [core_functions](../../../duckdb/extension/core_functions/) | Standard scalar and aggregate function implementations |
| [parquet](../../../duckdb/extension/parquet/) | Parquet read/write, metadata, encodings and format integration |
| [json](../../../duckdb/extension/json/) | JSON functions, readers and writers |
| [icu](../../../duckdb/extension/icu/) | ICU-based collation and temporal/internationalization functionality |
| [autocomplete](../../../duckdb/extension/autocomplete/) | SQL completion support |
| [tpch](../../../duckdb/extension/tpch/) / [tpcds](../../../duckdb/extension/tpcds/) | Benchmark data generation and associated query support |
| [demo_capi](../../../duckdb/extension/demo_capi/) | C extension API demonstration/testing |

The `extension/delta` directory is documentation, not a full in-tree Delta engine implementation. External extension configurations cover cloud access/lake formats, search/spatial/vector functionality and database scanners. Examples include HTTPFS, AWS, Azure, DuckLake, Iceberg, FTS, Spatial, VSS, PostgreSQL/MySQL/SQLite/ODBC scanners, and SQLSmith. Pinned sources and patches are defined under [.github/config/extensions](../../../duckdb/.github/config/extensions/) and [.github/patches/extensions](../../../duckdb/.github/patches/extensions/).

A local build's extension test population depends on these configurations and the sources actually fetched. This specification describes their integration boundary, not unseen implementations in remote repositories.

## Load lifecycle and registration ownership

An extension request resolves an artifact and its metadata, checks compatibility and configured trust/loading rules, initializes the module, and registers capabilities with the engine. Installation and loading are different operations: placing a binary in an extension directory does not mean its callbacks have been registered in a database instance. Autoloading adds a dependency from capability lookup to this lifecycle, subject to configuration.

Registration can retain function descriptors, bind-data hooks, type implementations, filesystem objects, parser overrides, and catalog/transaction callbacks. These objects must remain valid for every query or prepared statement that can call them. A dynamic library cannot be treated as unloadable merely because its initialization function has returned. Failure partway through initialization requires the loader/registration design's cleanup and error behavior; extensions should not assume an arbitrary partial registration is automatically reversible.

## Extension boundaries are not interchangeable

A scalar-function extension participates primarily in binding and vector execution. A storage/catalog extension participates additionally in name resolution, transaction management, physical DML planning, and attachment shutdown. A filesystem extension handles path routing, I/O capabilities, credential lookup, and query attribution. A parser extension can affect the front end before the native AST is built. These capabilities have different verification requirements even if all are delivered as one binary.

The generated C extension table is a binary contract. Function slot ordering, target-version gates, deprecated entries, and the distinction between stable and unstable surfaces must be maintained independently of source-level convenience wrappers. An ABI-compatible table does not guarantee that a function's SQL semantics or ownership behavior can change freely.

## External source and build contract

External extension configuration pins source revisions and can apply local patches. Loading their tests during a build makes those fetched tests part of that build's verification population, but does not make unseen upstream implementation details part of this repository's specification. `DONT_LINK` and `LOAD_TESTS` are separate decisions: an extension can be built/tested as a loadable artifact rather than linked into the engine.

Version/platform checks and available crypto/network dependencies influence which artifacts can be installed or loaded. Network-backed installation is an environmental prerequisite, not an implicit requirement for all local unit tests. Reproducible reports should record the extension revision, patches, build flags, and loaded binary along with the DuckDB commit.

## Verification requirements

Verify successful and failed loading, repeated requests, initialization diagnostics, version/platform mismatch, disabled external/autoload behavior, and registered capability use. Run function/callback lifecycle tests and storage/filesystem/parser-specific suites according to the extension's actual surface. Use [ABI and compatibility testing](../testing/compatibility.md), [configuration](../testing/configuration.md), and [CI extension matrices](../testing/ci.md). External SQLSmith coverage is detailed separately in [fuzzing](../testing/fuzzer.md); its pinned integration does not expose the generator implementation locally.
