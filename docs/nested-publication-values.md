# Versioned native nested publication

The [initial diagnostic](nested-publication-initial.json) preserves all four
native writer failures. The [follow-up](nested-publication-checkpoint8.json)
now passes all four independent producer files through original reads,
rollback, committed updates, mixed-child updates, deletion and reopen. Three
files contain unshredded or shredded VARIANT from the pinned development and
release producers; the fourth contains development TUPLE and empty STRUCT.
Every Rust result and development read of the Rust-written file matches a
separate development-mutated twin. Existing raw header versions and database
identifiers remain unchanged. Both reader outcomes and all source/binary/fixture
identities are retained. This is compatibility evidence, not performance parity.

The writer emits canonical unshredded VARIANT children using the snapshot's
retained type services. A shredded input need not remain shredded. Version
checks run recursively before publication: VARIANT requires storage 68;
TUPLE and empty STRUCT require 69. `DuckDbFormat::with_storage_version` selects
64–69 for new images only. Existing images keep their version even when reopened
with a newer preference. The provisional default remains 64, and fresh modern
database identifiers still have the earlier writer's zero initialization.

Connected Rust tests exercise fresh storage 68/69, both hash and B-tree index
factories, prepared parameters, decimals and nanosecond timestamps inside
containers, joins, window partitioning, failed primary-key writes, rollback,
committed mutation and repeated reopen. Empty incompatible CREATE/ALTER and
WAL requests fail without changing the checkpoint/log. Selected dynamic-child
validation, resource errors and cancellation remain observable in the writer.
Library 53, nested 33, checkpointing 11 and contracts 29 pass, with ordinary
check and all-target clippy. New test harness errors (empty type declaration
syntax and comparison with pre-commit bytes after a successful commit) were
repaired; those unsuccessful runs are not counted as passing engine checks.

WAL publication of VARIANT, TUPLE and empty STRUCT remains explicitly unsupported
until log sessions retain checkpoint capabilities and matching wire support.
VARIANT recovery checkpoint publication also remains closed until exact
canonical layout validation is integrated. SQL numeric equality cannot replace
that validation because it can erase distinct tags, widths and payload bits.
Empty containers can be inferred through CTAS; empty `STRUCT()`/`TUPLE()` type
annotations remain a parser gap. Full workspace, instrumentation and exploratory
Kani verification follow at the combined implementation checkpoint. Fresh modern
files still need independent C++ producer/reader testing beyond Rust round trips.
The earlier checkpoint's proofs and timings do not cover these changes.
