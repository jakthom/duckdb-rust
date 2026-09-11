# Local grammar corrections

This is the source and manifest from crates.io `sqlparser` 0.62.0, with its
original Apache-2.0 license. Only the dependency is vendored; it remains outside
the engine workspace and instrumentation coverage. No C++ runtime is involved.

The local changes enable parenthesized MAP and TUPLE type syntax for DuckDbDialect
using the existing recursive type productions. Empty ENUM syntax reaches the
binder, where DuckDB rejects an empty declaration. The manifest disables its
absent package readme. Other dialect behavior is unchanged.

DuckDB parenthesized expressions retain empty and singleton tuple syntax (`()`
and `(x,)`) instead of collapsing those forms into grouping. Bare STRUCT and
`struct_pack()` carry empty STRUCT metadata. TUPLE expression display preserves
the singleton trailing comma. Ordinary grouping, subquery and lambda dispatch
remain in their existing grammar paths.

DuckDB BIT and BIT VARYING parse and discard their optional expression modifiers,
matching pinned development `BitType` and `TransformBitType`; those expressions
never reach binding or execution. DuckDB custom type modifiers retain constant
identity, including quoted strings, grouping and directly negated numeric
constants at the full negative i64 boundary. Nonconstant modifiers (including
unary plus) fail parsing, matching `TransformTypeModifiers`; BITSTRING then
reaches ordinary modifier validation, unlike BIT's ignored expression list.
Precision and modifier parsing in other dialects is unchanged.

The dependency has no type-parser dialect hook in this version. A small tracked
grammar patch preserves nested STRUCT/ARRAY/MAP combinations without changing the
whole engine to GenericDialect or rewriting SQL text. Remove these corrections
when an adopted upstream release provides them. Engine tests exercise the
grammar through its normal parser and binder. Report source identities include
vendored Rust sources and manifests, not merely Cargo.lock.
