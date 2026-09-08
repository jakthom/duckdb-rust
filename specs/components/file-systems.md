# File systems, HTTP, and external resources

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## File systems, HTTP, secrets, and external resources

The [FileSystem interface](../../../duckdb/src/include/duckdb/common/file_system.hpp) provides opening, positioned/sequential I/O, metadata, synchronization, directory operations, file lifecycle and capability queries. Virtual/opener file systems route operations and attach caller context. `QueryContext` propagation matters for attribution of I/O metrics. Local files, compressed streams and extension-provided remote schemes can use different implementations.

HTTP transport utilities support engine/extension network needs. `SecretManager` handles secret types/providers and configured storage, exposing credentials to integrations through a managed interface. These facilities do not constitute a general user-authentication server or a sandbox for arbitrary SQL.

This revision also has an external-resource type registry and manager, with planner/execution support for resource operations. Resource lifetime and cleanup cross query/attachment boundaries and must be tested independently from ordinary row storage.

Sources: [file systems](../../../duckdb/src/common/virtual_file_system.cpp), [HTTP implementation](../../../duckdb/src/main/http/), [secret manager](../../../duckdb/src/include/duckdb/main/secret/secret_manager.hpp), [external resource registry](../../../duckdb/src/main/external_resource_type_registry.cpp), [external resource manager](../../../duckdb/src/main/external_resources_manager.cpp).

## I/O operation contract

`FileHandle` identifies an open file and delegates operations to its filesystem. Opening specifies access/creation/locking flags and caller/opener context. Positioned reads/writes and sequential reads/writes are distinct interfaces; a consumer must not assume a shared mutable file position is safe for parallel positioned access. Capability queries describe seekability, on-disk behavior, and implementation-specific support.

Short reads, end-of-file, invalid offsets, failed writes, and failed synchronization are separate outcomes. File scanners often need bounded ranges and prefetch; native persistence additionally depends on write/flush/sync and file lifecycle ordering. A remote or compressed stream may support different operations from a local random-access file. Unsupported operations should be explicit rather than silently emulated with weaker durability.

Sources: [file_system.hpp](../../../duckdb/src/include/duckdb/common/file_system.hpp), [local_file_system.hpp](../../../duckdb/src/include/duckdb/common/local_file_system.hpp), [opener_file_system.hpp](../../../duckdb/src/include/duckdb/common/opener_file_system.hpp), [virtual_file_system.hpp](../../../duckdb/src/include/duckdb/common/virtual_file_system.hpp).

## Routing, context, and caching

The virtual filesystem chooses an implementation for a path, while opener wrappers supply session settings, secrets, and caller context. This separation lets a shared filesystem obtain per-query/session information without baking credentials into every global object. `QueryContext` must follow reads and prefetch operations for correct profiling and I/O accounting.

External-file caching wraps I/O and metadata rather than replacing the native database block manager. Cache identity and freshness must reflect the external object contract. Error retries must not replay non-idempotent writes indiscriminately or hide a consistency change. HTTP helpers and extension transports can have their own buffering/retry behavior, which must be checked in the actual configured implementation.

## External resources and lifecycle

External resource types are registered separately from ordinary tables. The resource manager coordinates instances that can be referenced by planner/execution operations. Their cleanup may span query and attachment lifetime; an owned external resource is not automatically a transactional row or a spill file. Failures during creation/use/release need an explicit owner to avoid leaked handles or premature destruction.

Secret selection and persistence are described in [security](security.md). Filesystem path handling is not a general SQL privilege system, and routing through a virtual filesystem does not sandbox native extension code.

## Verification requirements

Test file open flags, seek/positioned I/O, EOF/short reads, directory/path behavior, locking, sync failures, compressed/nonseekable streams, and cleanup on error. Remote integrations require controlled servers or fixtures plus credential/capability configuration. Instrumented filesystem shims check query-level byte/request attribution in [I/O testing](../testing/io-metrics.md). Fault-injection filesystems in [fuzzing](../testing/fuzzer.md) check durability under failed writes/syncs; they do not prove correctness of every external transport.
