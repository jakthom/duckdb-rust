# SQLLogicTest interpreter and assertion language

[Specification index](../README.md) · [Testing index](README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Interpreter components

| Component | Responsibility |
| --- | --- |
| [SQLLogicParser](../../../duckdb/test/sqlite/sqllogic_parser.cpp) | Read records, tokenize directives, preserve locations, process included files |
| [SQLLogicTestRunner](../../../duckdb/test/sqlite/sqllogic_test_runner.cpp) | Database/connection lifecycle, substitutions, options, requirements, tags and command execution |
| [Command classes](../../../duckdb/test/sqlite/sqllogic_command.cpp) | Statements, queries, loops, restarts and other executable commands |
| [TestResultHelper](../../../duckdb/test/sqlite/result_helper.cpp) | Query/error verification, conversion, ordering, comparison and hashing |
| [SQLLogicTestLogger](../../../duckdb/test/sqlite/sqllogic_test_logger.cpp) | SQL-oriented diagnostics and test-event output |
| [CatchTestReporter](../../../duckdb/test/sqlite/catch_test_reporter.cpp) | Map interpreter outcomes into the Catch session |
| [TestReporter](../../../duckdb/test/helpers/test_reporter.cpp) | Reporter abstraction used by test helpers/interpreter |

## Record and directive interface

| Directive | Meaning |
| --- | --- |
| `statement ok` | SQL must succeed |
| `statement error` | SQL must fail; following expected-error section can constrain the diagnostic |
| `query <types> [sort-mode] [label]` | Execute SQL; the accepted type-marker string specifies column count, followed by values/hash/label checks |
| `----` | Separate SQL from expected output or expected error |
| `hash-threshold` | Switch sufficiently large results to hash-based expected output |
| `skipif`, `onlyif` | Conditional execution by supported engine/condition |
| `require` | Extension, resource, platform/build or other runner capability requirement |
| `require-env` | Require an environment value, optionally with a specified expected value |
| `test-env` | Establish test environment values/defaults; the executable spelling is hyphenated |
| `tags` | Declare custom selection/skip tags |
| `load` | Open/create the database used by the script |
| `restart`, `reconnect` | Reopen database or connection state at the declared boundary |
| `loop`, `foreach`, `endloop` | Repeated commands with substitutions |
| `concurrentloop`, `concurrentforeach` | Concurrent loop execution through the harness |
| `continue` | Advance within a loop |
| `mode` | Runner output/skip/debug modes supported by the interpreter |
| `set`, `reset` | Harness options, distinct from SQL `SET` within a statement |
| `sleep` | Explicit timing delay with a unit |
| `unzip` | Prepare compressed fixtures |
| `include` | Read another script file |
| `halt` | Stop script processing |

Named connections on statement/query records enable interconnection transaction tests. Loop bodies can exercise configuration combinations and concurrent operations without creating a separate executable.

The parser and command implementation define accepted argument forms. Descriptive documentation sometimes uses `test_env` for the concept/configuration key; source parsing accepts `test-env` as the script token.

Example:

```text
# name: test/sql/example.test
# description: Aggregate values and check a binding error
# group: [example]

statement ok
CREATE TABLE values_to_sum(i INTEGER)

statement ok
INSERT INTO values_to_sum VALUES (1), (2), (NULL)

query I
SELECT sum(i) FROM values_to_sum
----
3

statement error
SELECT missing_column FROM values_to_sum
----
<REGEX>:.*missing_column.*
```

This is an illustrative script, not an additional committed test or an executed result from this inspection.

## Result and error oracle

The harness compares converted values and supports ordered/no-sort, row-sort and value-sort behavior. Ordering mode is part of the assertion: a query without guaranteed SQL ordering should not accidentally assert a particular physical row order.

Conversion handles NULL as `NULL`, booleans as `1`/`0`, empty strings as `(empty)`, and embedded NUL escaping. Imported SQLite tests have a special numeric conversion path for compatibility with their hashes. Error/value expectations can use `<REGEX>:` and `<!REGEX>:`. Hashes and labels provide compact checks for larger or repeated results; they depend on the harness's exact conversion/order rules.

The query signature accepts `I`, `R` and `T`, but this runner uses it to establish expected column count rather than asserting an exact DuckDB logical type per column. Type-sensitive tests should explicitly query the type or use the native API. Value comparison first checks text/substituted text and regex expectations, then uses type-aware numeric or boolean comparison where applicable, including the engine's value-equality behavior for numeric precision.

Sources: [result_helper.cpp](../../../duckdb/test/sqlite/result_helper.cpp), [command implementation](../../../duckdb/test/sqlite/sqllogic_command.cpp).

## Pass, fail, skip, and crash

Missing requirements can skip a test. Configuration can skip paths/tags or selected expected error conditions. Explicitly required capabilities can instead cause a failure. The runner emits summaries and optional test events so callers can distinguish successful execution from omitted work. Catch assertion failures, interpreter failures, uncaught errors, fatal signals and process timeouts are different failure channels.

A useful run report records the selected files, effective configuration, skipped requirements and compiled-test availability. “Exit code zero” alone does not establish that every requested subsystem was exercised.

## Script execution state and lifecycle

The interpreter retains the current database, default and named connections, variables/substitutions, loop context, mode/options, expected-result labels, and reporter state. `load`, `restart`, and `reconnect` intentionally alter different parts of that state. A test using a second named connection must not assume it inherits the first connection's transaction or every session setting.

A record is parsed with source location, converted into a command, and executed through the runner. Included files and repeated bodies preserve enough location/context for actionable diagnostics. Concurrent loops introduce actual concurrent engine activity; a textual script is not necessarily a serial workload. Shared fixtures and expected-result labels require the interpreter's prescribed concurrency behavior.

## Result normalization and oracle strength

A value-based assertion validates the harness's conversion/comparison semantics, not necessarily byte-identical client output. A hash-based assertion additionally depends on sorting, normalization, and hash construction. Labels compare repeated results under that same representation. Expected-error regexes need to be narrow enough to reject an unrelated failure, while avoiding unstable details such as transient paths where those are not the feature under test.

Order-sensitive tests should use SQL ORDER BY and an order-sensitive expected result. Tests of unordered relational contents can choose rowsort; valuesort loses row association and therefore cannot establish that columns stayed paired correctly. Type-sensitive tests must inspect `typeof` or native metadata because the `I/R/T` signature does not enforce exact logical types in this runner.

## Test authoring and reproduction requirements

Keep fixtures and setup deterministic, declare extension/environment requirements explicitly, and use runner scratch placeholders for writable files. Add the smallest case that distinguishes the intended behavior, then add boundary variants for NULLs, empty input, multiple chunks, transactions, and parallelism when they are part of the component contract. For bugs sensitive to optimizer or storage mode, record the needed configuration instead of silently relying on a developer's default settings.

When a test fails, retain the exact record location, substituted SQL, actual/expected values or error, configuration, and connection/loop context. A result mismatch and a missing requirement require different actions. See [configuration](configuration.md), [orchestration](orchestration.md), and the [coverage map](coverage.md) for cross-component obligations.
