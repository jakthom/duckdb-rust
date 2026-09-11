# Native VARIANT WAL codec increment

This is an internal integration step in the value-and-expression assignment,
not native transaction, full VARIANT, performance, or database parity.

The codec uses the pinned native four-child STRUCT vector layout for VARIANT:
keys, child references, value descriptors, and payload bytes. It shares canonical
encoding/decoding with checkpoint columns, composes existing recursive WAL
vectors, and preserves root validity separately from nested NULL tags. Existing
constant/dictionary framing and development v2 string vectors remain selected by
the wire reader. No format/version handoff or publication gate is changed.

Logical traversal shares the remaining WAL cell budget and inherits enclosing
depth. Payload reconstruction checks cancellation inside key/reference/descriptor
loops and BIT/BIGNUM native conversion. Ordinary checkpoint decoding retains its
existing default budget. Selected VARIANT validation retains child adapters;
tests run the canonical bridge with an unrelated empty ambient registry and
verify selected failures propagate. SQL comparison and grouping keys are not
used as storage equivalence.

Four new unit tests cover all supported scalar wire tags, all decimal physical
widths, raw signed-zero/NaN payload bits, negative-zero BIGNUM, nested temporal and
decimal values, ENUM normalization, LIST/ARRAY/TUPLE/MAP/UNION normalization,
ordered exact OBJECT names (empty, case-distinct and embedded NUL), malformed
references/cycles/duplicates/root NULL, every truncated prefix of a small valid
vector, limits, cancellation, and retained service behavior. Individual encoded
row payloads and keys are also bounded by the WAL reader's 16 MiB byte-array
limit, independently of the 64 MiB total materialization limit. GEOMETRY tag 33
remains explicitly unsupported.

Independent acknowledged C++ fixtures are retained with producer identities,
SQL, committed boundaries, expected rows and checksums:

- [Release storage v1.5](../test/data/wal-variant-release/manifest.json).
- [Development storage v1.5 and v2](../test/data/wal-variant-development/manifest.json).

The generator is `scripts/generate_variant_wal_fixtures.py`. Each case includes
typed scalar values, VARIANT inside STRUCT/LIST, NULLs, an UPDATE/delete commit,
rollback, and 257 repeated inserted values. A separate read-only C++ connection
records each boundary without changing the file pair. Rust's connected SQL
regression passes all 15 boundaries, including exact `variant_typeof` and text
values. This proves these independent producer paths, not all WAL records.

Initial ordinary verification: all 65 library tests pass, nested 42, recovery 14,
logging 7, casts 12, types 18, contracts 32; workspace/all-target check and clippy
pass. The independent VARIANT component test covers the 15 boundaries above.
Instrumentation coverage reports 331 files, 3,075 functions and 216 interface
methods, with no missing attributes. The all-target trace check passes in 58.29
seconds with zero error returns, panics or open spans; temporary telemetry was
deleted. This instrumented duration is not a performance measurement.
The substantial integrated Kani checkpoint belongs to the integration lead;
there is no new proof or acceptance timing claim in this worker increment.
Manual Rust-vector/C++-reader evidence and full integrated publication remain
separate follow-ups.
