# Native deletion identity compatibility

The [initial independent campaign](native-deletion-identity-initial.json) retains
four C++-produced typed files: release storage 64/68 and development storage
64/69. `max_vacuum_tasks=0` preserves complete and partial deletion vectors in
a nonzero row group. Storage 69 additionally drops its empty leading group.
Before the repair all four fail Rust's absolute deletion-identity check. The
first three contain 6,142 live rows, ID sum 23,059,438; storage 69 contains 2,047,
ID sum 14,672,879. Both decimal amounts and nanosecond/list values are retained.
Generation SQL, reference/binary/source identities and fixture digests are in
the report; fixtures are not replaced by Rust-created equivalents.

Source review found that both current pins construct group-relative vector
starts, but that is not a safe validation rule for historical files. An
intermediate relative-only edit passed the four new fixtures and broke the
existing v1.3 deletion test (18/19 compatibility tests passed). Both current C++
readers still read that historical file: 124,879 rows, ID sum 8,423,605,671,
with deleted ID 125000 absent. The intermediate edit was not pushed, and the
historical test and assertions remain unchanged.

C++ history explains the compatibility obligation. At v1.3 commit `71c5c07cdd`,
RowVersionManager stores absolute starts and adjusts them on row-group moves.
Commit `f386370785` removes that base start and normalization but retains the
wire field. Current ChunkVectorInfo reads and rewrites the stored field without
using it for visibility lookup. Vector indices provide the actual mask address.
Accepting only relative starts, guessing from storage version, or accepting
only the two current origins would reject legitimately reused historical masks.

The repair consumes the fixed-width compatibility field and keeps bounds on
vector count/index, duplicate indices, offset arithmetic, mask encoding and
entries, and truncated data. It also implements development's rejection of a
partial-delete mask that marks the entire serialized vector alive or deleted.
Unit tests exercise obsolete start values independently of actual addresses,
invalid/overflowing/duplicate indices, malformed masks and every byte truncation.

The row-ID-gap review also found that the new interval checker allowed an
unexplained append watermark beyond the final physical group. It now requires
the last group end to equal `next_row_id` (zero when there are no groups), while
retaining valid interior gaps. This intermediate validation weakness was fixed
before pushing; its permissive unit case is now a rejection test.

Connected tests compare every surviving row ID, decimal coefficient, timestamp
and nested list. Both index implementations exercise prepared updates, failed
key changes, rollback, append/delete, native WAL replay, writable recovery,
checkpoint/rebase, another mutation and read-only reopen. The repaired focused
run passes ordinary check, library 61, all 19 compatibility tests and all-target
clippy with warnings denied. Coverage reports 328 files, 3,053 functions and
216 interface methods, no missing attributes. Python script compilation passes.
The substantial combined/tracing/Kani checkpoint and production file campaigns
follow; no performance acceptance is inferred from these file tests.
