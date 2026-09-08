# Embedding APIs, results, and language interfaces

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Surface map

| Surface | Entry points and representation | Ownership/error contract |
| --- | --- | --- |
| SQL | Parser grammar, statements, settings, functions, catalog objects | Statement/transaction semantics; typed errors |
| Internal/native C++ | [duckdb.hpp](../../../duckdb/src/include/duckdb.hpp), `DuckDB`, `Connection`, relation/prepared/result classes | Engine C++ objects and smart pointers; results and exceptions; internal interfaces are version-coupled |
| C API v1 | [duckdb.h](../../../duckdb/src/include/duckdb.h), implementations in [capi/v1](../../../duckdb/src/main/capi/v1/) | Opaque handles, explicit destroy/free functions, status/error accessors |
| C API v2 | [duckdb_v2.h](../../../duckdb/src/include/duckdb_v2.h), implementations in [capi/v2](../../../duckdb/src/main/capi/v2/) | Environment/database/connection/statement/result handles; structured error codes/info and explicit result stepping |
| Consumer C++ wrapper | [duckdb_cpp.hpp](../../../duckdb/tools/cpp/duckdb_cpp.hpp), namespace `duckdb::cxx` | Primarily move-only owning/borrowed handles; RAII; throwing error interface over C v2 |
| C extension APIs | [duckdb_extension.h](../../../duckdb/src/include/duckdb_extension.h), [duckdb_extension_v2.h](../../../duckdb/src/include/duckdb_extension_v2.h) | Function-table dispatch and extension initialization contract |
| Arrow C data/stream | Arrow import/export interfaces | Shared external buffers governed by release callbacks |
| ADBC | [ADBC implementation](../../../duckdb/src/common/adbc/) | Driver/statement/connection interface and Arrow exchange |
| Shell | [shell sources](../../../duckdb/tools/shell/) | Process arguments, stdin/stdout/stderr, terminal behavior and dot commands |
| Swift | [Swift binding](../../../duckdb/tools/swift/duckdb-swift/) | Swift wrappers, type conversion, prepared statements, appending and Foundation integration |

The C v2 header defaults its API target macros to `2.0.0` in this checkout. This is an interface-version declaration, not proof of a publicly released product version. Likewise, the C++ build describes its wrapper as the stable API, but loadable examples explicitly use a pinned unstable extension surface. Consumers must inspect lifecycle/version gates for the exact functions they use.

## C API v1 lifecycle

The familiar v1 lifecycle is configuration/open → connect → query or prepare/bind/execute → fetch/access results → destroy results/statements → disconnect → close. Separate families expose chunks/vectors/types/values, appenders, scalar/aggregate/table/cast functions, replacement scans, Arrow, filesystem integration and task control.

Each function's generated documentation specifies whether the returned pointer/handle is owned or borrowed and which destroy/free operation applies. Result memory, returned strings and vector data have different lifetimes. Bindings must translate both the status code and the associated diagnostic and must not let foreign exceptions escape a C callback boundary.

Sources: [v1 specification](../../../duckdb/api_spec/v1/), [C API header](../../../duckdb/src/include/duckdb.h), [v1 implementations](../../../duckdb/src/main/capi/v1/).

## C API v2 and C++ wrapper lifecycle

The v2 environment contains a database-instance cache and tracks open databases. Destroying an environment with open databases returns `RESOURCE_IN_USE`. Connections produce parsed statements/prepared statements and streaming result handles; result execution is modeled as an explicit state machine.

The implementation distinguishes pending execution, available result data, completion, cancellation and errors. Statement expansion can produce a sequence of internal fragments, and the result wrapper selects the principal row-producing result. Multiple row-producing fragments cannot simply be collapsed into one stream. Consumer-requested cancellation is distinguished from engine-originated interruptions such as a query timeout.

| C v2 stepping outcome | Caller contract |
| --- | --- |
| `WAITING` | Execution has not produced a terminal outcome or chunk; use the wait/readiness interface before further work as appropriate |
| `CHUNK` | An owned chunk is returned; destroy it through the documented chunk API |
| `FINISHED` | Stream exhausted; terminal state remains finished |
| `CANCELLED` | Consumer interrupt has cancelled the query; terminal state remains cancelled |
| Error return and error-info handle | Failure is reported on the error channel, distinct from the four step statuses |

The internal wrapper states are `PENDING`, `STREAMING`, `FINISHED`, `CANCELLED`, and `ERRORED`. These internal states should not be substituted for the public status enumeration in a binding.

The consumer C++ wrapper exposes `Environment`, `Database`, `Connection`, `SqlStatement`, `PreparedStatement`, `QueryResult`, `DataChunk`, `Vector`, `Value`, `LogicalType`, `Arena`, Arrow helpers and registration interfaces. Owning wrappers release resources automatically; borrowed wrappers must not outlive their documented owner. Ordinary value types are copyable, while most resource handles are move-only.

Sources: [v2 environment](../../../duckdb/src/main/capi/v2/capi_v2_environment.cpp), [v2 result implementation](../../../duckdb/src/main/capi/v2/capi_v2_result.cpp), [v2 result specification](../../../duckdb/api_spec/v2/query_result/query_result.yaml), [C++ wrapper](../../../duckdb/tools/cpp/duckdb_cpp.hpp).

## Relational and append interfaces

The native relation API composes relational operations into deferred plans: table/value/read relations, projection/filter/join/aggregate/order/limit, and write/DDL relations. Execution ultimately enters the same binding and execution machinery as SQL. Relation object construction is therefore not equivalent to executing a query immediately.

Appender interfaces provide bulk insertion through typed values or chunks with explicit flush/close/error handling. Their lifetime, target schema, constraints, transaction behavior and buffer ownership must be tested; a successful append call is not by itself evidence of a completed durable transaction.

Sources: [relations](../../../duckdb/src/main/relation/), [relation.hpp](../../../duckdb/src/include/duckdb/main/relation.hpp), [appender.hpp](../../../duckdb/src/include/duckdb/main/appender.hpp), [C appender spec](../../../duckdb/api_spec/v1/appender/appender.yaml).

## Boundary invariants for bindings

Public handles encode ownership and error behavior that differs from the internal C++ classes. A binding should maintain an explicit owner graph: environment/database → connection → statement/result, plus independently owned chunks/values/types where the API grants ownership. Borrowed vector data remains tied to its chunk or other documented parent even if the host language can retain the pointer indefinitely.

Each wrapper operation must translate its complete outcome: status, error-info ownership, returned handle validity, and any terminal state transition. A non-null result handle can represent a failed query; successful handle creation is not a substitute for checking execution. Foreign callbacks must catch/translate host-language failures into the callback's supported error interface without unwinding through C frames.

Prepared execution must distinguish parameter discovery, binding values, execution, and reset/reuse. Named parameters and positional indexes need the API's declared indexing rules; no universal zero- or one-based assumption should be applied across every handle family. Appenders similarly distinguish buffered append, flush, close, and transaction commit.

## Incremental result protocol example

A C v2 consumer creates a result, repeatedly steps it, handles `WAITING` through readiness/wait support, consumes and destroys each `CHUNK`, and terminates on `FINISHED`, `CANCELLED`, or the error channel. The public status and the internal wrapper state have different names and roles. A binding must avoid a busy loop that treats `WAITING` as an empty successful batch.

Cancellation requested by the consumer has a dedicated terminal outcome; an engine timeout can arrive as an error. Repeated calls after terminal state must follow the implementation's sticky-terminal behavior rather than re-execute the statement. Destroying a result before exhaustion must release/cancel the retained query state according to the handle contract.

## Generated interface maintenance

The API YAML/specification, generated headers, extension tables, implementation, and tests jointly define an API addition. Generation/versioning paths for v1 and v2 differ; see [build](../build.md). Check source visibility and binary table compatibility separately. The consumer C++ wrapper adds RAII and exceptions but does not erase the underlying C v2 target-version and ownership constraints.

Swift, shell, Arrow, and ADBC each add adaptation behavior beyond the core C interfaces. Their tests must run against the intended locally built artifact. Importing an unrelated installed package proves neither local ABI compatibility nor behavior of this checkout.

## Verification requirements

Use native API tests for handle and callback behavior, ABI tooling for table/layout compatibility, and each client harness for language/process integration. Include null/invalid arguments as documented, double-use prevention in owning wrappers, early destruction, error allocation/free, prepared reuse, cancellation, and nested result values. The separated [component/API](../testing/component-api.md), [client](../testing/clients.md), and [compatibility](../testing/compatibility.md) specs identify these independent obligations.
