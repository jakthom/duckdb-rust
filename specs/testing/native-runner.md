# Native Catch runner and test discovery

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Native runner initialization and discovery

`test/unittest.cpp` supplies a Catch runner with DuckDB-specific option parsing and setup. It installs the test reporter, resolves configuration/environment, validates required passthrough variables, sets the working directory, prepares temporary directories, isolates the test HOME, registers SQL tests and executes the Catch session. It then prints failure and requirement-skip summaries and performs outcome-dependent cleanup.

SQL tests are registered from:

1. `test/`, recursively, using recognized suffixes.
2. `third_party/sqllogictest/test`, if supplied, with explicit compatibility exclusions.
3. Extension test paths reported by generated extension integration.
4. An exact file list passed through `-f`/`--input-file`, when all entries are actual SQLLogicTest paths.
5. Standard input when `--stdin` is selected.

Exact-file-list and stdin modes clear the compiled test registry and register the selected SQL cases. `--skip-compiled` also changes the registry. SQL files are not compiled into the binary; native C++ cases are.

Slow files receive Catch's hidden tag `[.]`; coverage files would receive `[coverage][.]`. Default selection omits hidden cases. Explicit selection such as `"*"` includes hidden cases, subject to other filters and requirements. The exact registry also depends on platform and extension build configuration.

Source: [test_sqllogictest.cpp](../../../duckdb/test/sqlite/test_sqllogictest.cpp).

## Startup and teardown protocol

Startup must finish option/environment validation before executing tests. The runner distinguishes a malformed invocation from a missing capability inside one test. It builds the effective configuration, establishes scratch/environment context, and only then registers the requested SQL population and starts Catch. A startup failure should not be counted as a database test failure or a successful empty run.

Compiled Catch registrations exist before dynamic SQL registration. Exact-file mode deliberately clears that registry when every listed item is an actual SQLLogicTest file. A file list containing native names/patterns follows a different selection route. Test-list consumers therefore need to preserve the distinction between a concrete SQL path and a Catch expression.

At completion, reporter summaries and the process return code describe the session result. Temporary-directory reclamation uses the resulting success state and the configured ownership/disposition policy. Failed state can remain available for diagnosis; destruction of a database object during a test does not necessarily imply deletion of its file.

Sources: [unittest.cpp](../../../duckdb/test/unittest.cpp), [test configuration](../../../duckdb/test/helpers/test_config.cpp), [reporter](../../../duckdb/test/sqlite/catch_test_reporter.cpp).

## Input and output interfaces

Inputs include argv/Catch selection, native test configuration, environment passthrough, source/fixture roots, extension registrations, and optionally a test-file list or stdin. Outputs include assertion results, current-test/progress messages, requirement-skip summaries, diagnostics, and retained scratch artifacts. The Python orchestrator parses parts of this output as a protocol; changing reporter wording can break failure attribution without changing SQL execution.

Test enumeration is not execution. Hidden tags, compiled availability, requirement checks, and platform configuration mean the count of discovered source files differs from the count of tests actually run. Report selected, executed, failed, and requirement-skipped populations separately where the harness exposes them.

## Verification of the runner itself

Use [stdin runner tests](clients.md), temporary-directory lifecycle pytest, tag-selection fixtures, and [orchestrator self-tests](ci.md) when changing discovery/reporting. Include invalid options, absent passthrough variables, empty selections, mixed native/SQL lists, included scripts, hidden cases, and failure cleanup. A one-file SQL success verifies only a small part of runner startup/selection behavior. Source-derived invocation examples are in the [runbook](runbook.md).
