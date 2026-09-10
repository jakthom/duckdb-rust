# Fresh native typed files

The shell now exposes the selected format's new-image preference through
`--storage-version 64..69`. It requires a writable native file, rejects invalid,
duplicate and incompatible options before file creation, and never upgrades an
existing file. The default remains 64. Native version/type capability checks
remain owned by the format rather than duplicated in SQL binding.

The [initial six-version campaign](fresh-native-values-checkpoint9.json) creates
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
separate expected version limitation. The [repeat fixture campaign](fresh-native-values-checkpoint9-fixture.json)
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
