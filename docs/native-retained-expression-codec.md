# Retained native expression codec in progress

The lead's codec maps the retained expression tree to the pinned native
ParsedExpression fields. It remains test-only until the coherent catalog-default
migration is connected. The old eager native DEFAULT path is unchanged; none of
these internal tests establishes DEFAULT or file compatibility parity.

Supported nodes are typed constants, resolved CAST/TRY_CAST, function calls and
LIST constructor/index/field operators. Declared NULL types, literal payload bits,
aliases, named arguments, legacy child-alias provenance, operator flags and full
function qualification are retained without SQL parsing or evaluation. Diagnostic
source spans are accepted with width checks and omitted on writing. FILTER,
nonempty function ORDER BY, DISTINCT/export state, row references, unbound parsed
type expressions and other absent IR variants are explicitly unsupported.

One root owns the typed-Value codec session, sharing its 64 MiB byte and logical
visit budgets across every literal and CAST target. Resolved CAST metadata does
not inherit value-materialization version gates: independent C++ VARIANT CAST
targets are valid at storage 64/65 even though actual VARIANT Value payloads are
not. No new literal version support is inferred from that distinction.

Expression depth is bounded at 64, node count at 16,384 and serialized identifier
bytes at 16 MiB. Unvisited siblings reserve their node allowance before allocation;
descendants cannot repeatedly reuse the same remaining-node budget. Reads check
cancellation throughout; writes preflight the tree and stage the entire root
before appending. Unknown ordered fields cannot be skipped without a wire schema.

Legacy argument names must equal child aliases to be representable. Modern calls
with arguments cannot silently downgrade to legacy binding provenance below 69.
The writer can retain nonempty legacy calls even in 69 using the accepted legacy
field, and modern named calls use FunctionArgument objects. Contradictory duplicate
qualification fields are rejected. For nested schema paths the native catalog is
the first component, not the third component from the end. The language/catalog
owner must decide any safe conversions; the codec does not guess.

Five focused tests cover independent fixtures, truncation, invalid pointers,
unrepresented function state, cancellation, node/identifier limits, atomic writes,
NaN payloads/signed zero and legacy/modern metadata. Of 200 independently parsed
fixtures, 129 decode and Rust round-trip, while 71 remain unsupported (57 unresolved
type-expression cases and 14 row-reference cases). The initial pass supported
121; fixing the CAST-versus-Value version distinction restored eight VARIANT
target cases. A first clippy pass found one collapsible conditional, repaired
without changing its error policy. All 59 native library tests, ordinary workspace
check and warnings-denied all-target clippy pass; coverage reports 382 files,
3,693 functions and 239 methods, none missing. Reverse C++ reading and the next
integrated tracing/Kani checkpoint are still pending for this source.

The remaining connected work is retained UNBOUND type syntax and selected type
binding, catalog/default ownership and bit-exact identity, SQL capture, omitted
INSERT values, ADD backfill demand, native WAL/checkpoint recovery, rollback and
reopen. The version conversion restrictions above remain real gaps in that work,
not accepted end-to-end behavior.
