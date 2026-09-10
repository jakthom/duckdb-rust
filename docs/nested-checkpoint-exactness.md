# Nested checkpoint exactness investigation

On integrated source `4b2a027`, the debug shell (SHA-256
`c1ba9ce075ce078740003f254d448bea1f9b660351735fc70afd61e25b7073fd`)
reproduces a checkpoint-validation failure:

```sql
CREATE TABLE t(v DOUBLE[]);
INSERT INTO t VALUES(['NaN'::DOUBLE]);
CHECKPOINT;
```

Run against a new native file with `--durability wal`, it returns
`Internal Error: checkpoint layout does not preserve snapshot identity and values`.
Both the same Rust shell and pinned development C++ read-only reopen return
`[nan]`. The acknowledged log content remains readable; this witness establishes
failed maintenance, not data loss. Source inspection identifies the cause:
layout validation checks IEEE bits for top-level floats only and uses derived
value equality inside containers. Equal NaN bits then compare unequal; ordinary
equality can conversely hide changes between nested positive and negative zero.
The pinned development executable also checkpoints that same temporary file
successfully, after which Rust read-only reopen still returns `[nan]`.

The repair must recursively preserve exact values across every container without
using selected SQL comparators or equality keys. NaN payloads and signs, scalar
widths, decimal metadata, union tags, member order and child NULLs remain durable
content. This strict validation is separate from the native VARIANT canonical
representation equivalence prerequisite, whose SQL-transparent wrappers need
format-owned normalization. No repair or new proof is claimed by this initial
investigation record.

## Repair

Strict layout validation now traverses every physical nested payload and compares
FLOAT/DOUBLE IEEE bits recursively. It does not call SQL comparators, casts or
keys, and does not materialize a second row/value tree. Column-backed and row-backed
snapshots both use borrowed iterators. Traversal checks cancellation, bounds depth
at 64 and bounds each compared row at 16 million logical values, including repeated
shared children; identical Arc pointers do not bypass accounting.

Tests pass for both float widths in LIST, ARRAY, STRUCT, TUPLE, internal OBJECT,
MAP keys and values, UNION and VARIANT, with exact NaN defaults. Changed NaN bits
and nested zero signs are rejected. Connected native SQL tests exercise manual
and automatic checkpoints, both index factories, prepared updates, rollback,
post-checkpoint row-ID changes, writable WAL recovery and read-only reopen.
Ordinary check, library 57, checkpointing 13, contracts 31, types 17 and all-target
clippy pass. Initial implementation compilation errors concerning borrowed row
views were corrected before these results. The original CLI reproducer now
checkpoints successfully and both Rust and pinned development read `[nan]` from
the resulting checkpoint. The next combined instrumentation/Kani checkpoint
remains required; no new performance acceptance is claimed.

This fixes strict physical equality. Native VARIANT wrapper canonicalization
still requires the separate format-owned equivalence integration before its
recovery-publication gate can open.
