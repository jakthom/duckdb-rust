# Native publication identity and compatibility

The shared publication increment is `12586bf`. The value-and-expression
milestone continues; native TUPLE/VARIANT writing and expression defaults are
not enabled by this metadata repair.

The [initial independent diagnostic](native-version-publication-initial.json)
confirmed that ordinary Rust checkpoint commits reset file metadata. All six
producer cases retained their SQL rows, but every generation reset, four
effective storage versions downgraded to 64, and three database identifiers
changed. Development produced storage 64, 65, 68 and 69; release produced 64
and 68. Those original failures remain recorded.

The [repaired campaign](native-version-publication-retained.json) passes **6/6**
on unchanged production source. Every case preserves its effective storage
version and identifier, advances its generation, and returns the exact expected
rows to Rust and its pinned C++ producer's read-only connection. File creation,
mutations and reopen use the ordinary CLIs in temporary databases. Source,
binary, script and reference identities are retained. This diagnostic is not
a performance measurement or full native-compatibility claim.

`SnapshotFormat` may now return a selected owned checkpoint encoder. Native
bindings retain only identifier, generation, root and the two header version
fields. The file layer does not interpret that metadata or read/cache the old
table image per commit. It prepares successor bytes and their next binding
before publication, installing the binding only after durable success. Definite
failure preserves the old state; an uncertain outcome prevents another write
until reopen. Stateful FileCheckpoint has one transaction-manager owner, as
the existing WAL adapter already requires. Formats without state retain their
selected stateless encoder.

Native successor binding and encoding now reject unknown main/database versions
and header flags/encryption consistently with decoding. Existing raw version
fields are preserved, including legacy serialization values and modern 999/69
headers. No silent version upgrade or new type-layout capability is inferred.
The default fresh-file version remains unchanged in this increment.

Seven native version units, checkpointing 11/11, compatibility 14/14, contracts
27/27, nested 28/28, recovery 14/14 and ordinary check/all-target clippy pass.
An independent replacement format tests prepared successor binding failure,
definite and uncertain I/O failures, one initial read across multiple commits,
typed SQL mutation, rollback and reopen. Native tests drop input bytes before
reusing the bound encoder and check repeated owned generations. The recovery
suite includes interrupted publication and the full tail-truncation sweep.

The follow-up temporal/coverage/trace check is completing. Kani is scheduled at
the next substantial combined checkpoint with the queued DATE, numeric and
dynamic OBJECT increments; the earlier six-harness result at `748dfba` does not
cover this publication-state change. Full upstream and faster-reference timing
acceptance also remain open before the next PR follow-up push.
