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
