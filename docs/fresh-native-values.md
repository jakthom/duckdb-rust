# Fresh native typed files

The shell now exposes the selected format's new-image preference through
`--storage-version 64..69`. It requires a writable native file, rejects invalid,
duplicate and incompatible options before file creation, and never upgrades an
existing file. The default remains 64. Native version/type capability checks
remain owned by the format rather than duplicated in SQL binding.

The initial six-version campaign creates
equivalent Rust and development files, with decimal/nanosecond structs, nullable
lists, embedded-NUL binary values and full unsigned integers. Storage 68 adds
VARIANT objects/lists; 69 adds TUPLE and empty containers. Each stage checks
Rust/development reads against the independent development twin, unchanged
rollback bytes, version/identifier preservation, Rust mutations and a final
C++ mutation/checkpoint. Both pinned reader outcomes are recorded.

Storage 64–68 pass all four stages, including the release reader. Storage 69
passes creation, rollback and Rust commit. After C++ updates its nested columns,
Rust rejects the checkpoint as `noncontiguous row group identity`; development
still reads exactly the expected rows. Release's inability to read 69 is a
separate expected version limitation. The repeat fixture campaign
reproduces the failure and exports both final images under
`test/data/native-row-identity-initial/`, with their uncompressed digests and
producer/query provenance in the report. The Rust image is a hybrid producer
fixture, not independently C++-created; its twin is independently produced.

Source investigation identifies a development storage-69 obligation: persisted
row-ID gaps and `next_row_id` are separate from physical row cardinality. The
reader currently discards catalog property 105 and assumes contiguous row groups.
Do not simply waive the continuity error: retain exact row identities and the
append high-water mark, validate ranges/overlap/version capability, and carry
them into WAL replay and later mutations. This remains an open repair at this
initial report, not a passing file-compatibility claim.

Ordinary check, nested 41, temporal 29, contracts 31, types 17, checkpointing 13
and all-target clippy pass before the new fixture repair. Python harness tests
also pass 38/38. The next substantial combined Kani checkpoint and full upstream/
faster-reference regression campaign remain required. No new performance result
is inferred from these compatibility checks.

## Row identity and free-tail repair

The reader now restores catalog property 105 as the append watermark and uses
actual row-group starts for row identities and column streams. The later
[deletion compatibility repair](native-deletion-identity.md) addresses masks by
their independent group-relative vector indices.
Physical cardinality is checked separately. Ordered nonoverlapping intervals,
checked endpoints, the append bound, legacy-version continuity and exact total
counts remain mandatory. The retained hybrid image contains live row ID 1 and
next ID 2; the independent development twin contains ID 3 and next ID 4, matching
development's `SELECT rowid,id FROM t` observations.

Testing that independent twin exposed a second compatibility gap: its header
allocates five blocks, but C++ has truncated the last two free blocks, leaving
three. `SingleFileBlockManager::Truncate` runs after checkpoint header publication;
the persisted free list accounts for the removed suffix. The Rust reader now
permits an aligned missing suffix only when a bounded, strictly ordered free
list accounts for every missing block. Referenced blocks must still exist and
pass checksums. Block-offset arithmetic is checked even for forged allocation
watermarks. This does not synthesize data or weaken live-block truncation checks.

Both fixtures pass direct recovery append/update/delete identity checks, plus
prepared SQL updates, hash/B-tree indexes, failed key changes, rollback with
unchanged bytes, mixed typed joins/windows, checkpoint publication and reopen.
Their decimal, nanosecond, binary, unsigned, VARIANT, TUPLE and empty-container
content matches the retained development result. Partial and missing-live-block
files remain rejected without changing their bytes. Range/free-list unit tests
cover overlap, ordering, bounds, overflow, missing coverage and resource limits.
Ordinary check, library 59, compatibility 15 before the final two connected tests,
all three new compatibility tests, clippy and instrumentation coverage pass
(326 files, 3,040 functions, 216 interface methods, no missing attributes).

The fresh-file script now also checks Rust reading every development twin and a
Rust mutation after the C++ checkpoint. The expanded campaign and combined
workspace/tracing/Kani checkpoint are pending; the initial failure reports remain
unchanged. Native VARIANT/TUPLE WAL publication remains separately unsupported.

## Repaired combined checkpoint

On integrated `27d190b`, the production rerun
passes all six versions and all five stages each, including Rust reading every
development twin and mutation after C++ publication. Release agrees for storage
64–68 and rejects 69 as expected. The preceding debug run
also passes and is retained. Production source and binary hashes remain unchanged
through the campaign. The [combined checkpoint](value-expression-progress.md)
passes workspace tests/check/clippy, trace compatibility and all six maintained
Kani harnesses; those proofs do not establish this native wire/state protocol.
The original failures above are historical observations, now repaired in these
selected workflows, not discarded or reclassified as passing runs.
