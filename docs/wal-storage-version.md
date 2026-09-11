# Checkpoint compatibility in log sessions

The lead's next integration prerequisite retains actual existing-file storage
compatibility in the native transaction-log session. This does not change a
file version, infer it from a fresh-file preference, or cache/reread table bytes
per commit.

`CheckpointEncoder::storage_version` defaults to no metadata. Native encoders
return the normalized version already validated from the header namespaces.
`FileWal` hands that compact format/version pair to a defaulted selected
`TransactionLog::start_at`. Existing adapters keep their ordinary callback;
native sessions validate format and known version, then retain it. Missing
metadata preserves the conservative legacy type gates. Rebase validates the
actual successor version and rejects compatibility changes before session/I/O
publication. This state is compatibility only, not an identity/generation token.

TUPLE and empty STRUCT now use their existing WAL vector codecs only for actual
storage 69. Recursive CREATE/ALTER checks also cover empty tables and nested
children. VARIANT wire/publication enablement remains a separate step. Non-NULL
nested defaults and general parsed DEFAULT expressions are still unsupported.

The connected test deliberately crosses actual versions 64/68/69 with fresh-file
preferences 64/69 under both index implementations. Older images reject new
types without changing their checkpoint or acknowledged log, then remain usable.
Version 69 carries decimal/timestamp/list-bearing TUPLE, empty positional/named
values, typed parameters, indexed updates, rollback, joins, groups, windows,
ALTER, two manual checkpoints, read-only replay, writable recovery and reopen.
Every path preserves the original version. Unit cases cover unknown/foreign
metadata and a rejected compatibility-changing rebase followed by a valid retry.

This is not yet an independent C++ WAL-reader or performance acceptance claim.
Combined workspace, tracing and exploratory Kani results follow integration of
the corresponding native VARIANT wire slice and family increments.
