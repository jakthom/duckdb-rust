# Encryption and secrets

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Encryption boundary

Encryption spans database storage options, key management, block/header representation, WAL and temporary/output paths where supported. [Encryption helpers](../../../duckdb/src/common/encryption_functions.cpp), [key manager](../../../duckdb/src/common/encryption_key_manager.cpp), [encryption state](../../../duckdb/src/common/encryption_state.cpp), and storage/crypto integrations supply the implementation. The selected cipher, encryption version and storage-compatibility settings must agree at creation and reopening.

Correctness includes wrong/missing-key errors, encrypted checkpoint/WAL recovery, temporary-file behavior, authenticated data handling, and library-specific implementations. [Encryption configurations](../../../duckdb/test/configs/encryption.json), [native encryption tests](../../../duckdb/test/encryption/) and [SQL storage encryption tests](../../../duckdb/test/sql/storage/encryption/) cover these boundaries. A secret-provider configuration and an encrypted database key are distinct interfaces even when both involve sensitive values.

## Key ownership and storage integration

`EncryptionKeyManager` is an object-cache entry accessible from a database or client context. It maintains named derived keys under a mutex and exposes add, lookup, clear/delete/erase operations. `EncryptionKey` owns fixed-length key bytes, disallows copying, and supplies memory lock/unlock helpers. Key derivation and encoding helpers are separate from the block encryption state that consumes the key.

The header explicitly describes `GenerateRandomKeyID` as non-cryptographically secure. That identifier is not a cryptographic key generator and must not be repurposed as one. The existence of memory-lock helpers also does not by itself prove that every platform prevents every copy of a user-supplied key from reaching swap, logs, or foreign-library memory.

Sources: [encryption_key_manager.hpp](../../../duckdb/src/include/duckdb/common/encryption_key_manager.hpp), [encryption_state.hpp](../../../duckdb/src/include/duckdb/common/encryption_state.hpp), [encryption types](../../../duckdb/src/include/duckdb/common/encryption_types.hpp).

## Secret type/provider/storage interfaces

Secrets have a type, provider, name, scope, options, and persistence policy. The declared lifetime choices are default, temporary, persistent, and transaction. A provider is a creation mechanism for a secret type; secret storage controls retention. `SecretManager` registers types/providers, creates or registers secrets, looks up a secret by path/type, and resolves a named secret in storage.

`BaseSecret::MatchScore` uses the longest matching prefix length by default. Scope matching is therefore an explicit credential-selection contract. It should not be confused with an operating-system permission check or a guarantee that SQL cannot access other paths. `SecretDisplayType` distinguishes redacted and unredacted rendering; the default `ToString` request is redacted. Persistence requires a supported serialization/deserialization path and is distinct from redaction in display output.

Sources: [secret.hpp](../../../duckdb/src/include/duckdb/main/secret/secret.hpp), [secret_manager.hpp](../../../duckdb/src/include/duckdb/main/secret/secret_manager.hpp), [secret storage](../../../duckdb/src/include/duckdb/main/secret/secret_storage.hpp).

## Trust boundaries and engineering requirements

DuckDB is embedded in its host process. SQL, extension loading, filesystem access, remote credentials, and native callbacks cross different trust boundaries; encryption at rest does not isolate an executing query from host-process privileges. Build/runtime settings controlling external access and extension installation/loading need to be configured at the hosting boundary. This document describes mechanisms in the source, not a completed security audit or a sandbox guarantee.

Sensitive values must not leak through exception formatting, profiler output, test artifacts, or unredacted secret introspection. Review ownership and cleanup when copying options into bind data or external-library calls. Recovery behavior must distinguish a wrong key, incompatible encryption metadata, and corrupted authenticated data instead of silently accepting undecipherable content.

## Verification requirements

Exercise correct/wrong/missing keys, checkpoint and WAL reopen, cipher/version compatibility, temporary/output encryption where implemented, and provider/storage lifetimes. Secret tests need overlapping scopes, missing providers, transaction end, persistence reopen, and redaction checks. Use [encryption configurations](../testing/configuration.md) and [component tests](../testing/component-api.md); format and failure cases additionally belong to [compatibility](../testing/compatibility.md) and [fuzzing](../testing/fuzzer.md).
