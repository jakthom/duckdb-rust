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

## Integrated native WAL path

The integrated four-child VARIANT wire codec now enables actual storage 68+
sessions and exact recovery/checkpoint publication. Version 69's TUPLE and empty
STRUCT path remains independently gated. Missing compatibility metadata still
rejects these newer types. The version-crossing test now exercises both manual
and automatic checkpoint policies, VARIANT parameters with decimal/timestamp/
array content, relational comparisons/groups/windows, rollback, rebasing and
exact live-versus-reopened display/type results.

The initial bidirectional WAL campaign passes
all six versions and seven stages each using a debug Rust shell: creation,
initial checkpoint, rollback, Rust commit, C++ commit, a subsequent Rust commit,
and final checkpoint. Both C++ pins read storage 64–68; development also reads
69, which release rejects as expected. Rust reads every independent development
twin. Checkpoint identity/version and expected WAL presence/retirement are
verified; the source fingerprint remains unchanged. These are correctness
observations, not production timing evidence.

An old test deliberately expected all newer types to be unsupported even in a
storage-69 WAL. That assertion was updated to test real storage-64 rejection
without byte changes; positive 68/69 paths now have connected coverage. A new
test initially expected generic `OBJECT` and SQL NULL from `variant_typeof`;
the established contract is ordered `OBJECT(d, ts, a)` and `VARIANT_NULL`.
The test now compares complete live and reopened type/text rows, while the
independent campaign supplies the development oracle. Neither required an
engine-semantic workaround or an upstream assertion change.
