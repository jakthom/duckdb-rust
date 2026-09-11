# Nested membership and position

This family slice is in progress; the interface prerequisite below does not
register the SQL functions or establish membership parity by itself.

## Selected equivalence prerequisite

Both pinned engines use ordinary primitive equality in `list_contains` and
`list_position`, but use `CreateSortKey` equality for nested children. The
`list_has_any` and `list_has_all` families use sort-key equality for every child
type. Source: `src/include/duckdb/function/scalar/list/contains_or_position.hpp`,
`extension/core_functions/scalar/list/list_has_any_or_all.cpp`,
`src/function/scalar/create_sort_key.cpp`, and `src/include/duckdb/common/radix.hpp`
in each pinned checkout.

For example, both engines return true for a scalar search of INTERVAL '30 days'
in `[INTERVAL '1 month']`, but false for `list_has_any` over those lists and for
a search of `[INTERVAL '30 days']` in `[[INTERVAL '1 month']]`. VARIANT search
also distinguishes INTEGER and BIGINT tags holding 1, unlike SQL VARIANT equality.
These are distinct contextual relations, not permission to change GROUP BY,
join keys, SQL equality, or ordinary sorting.

`TypeAdapter::write_key_with_context` and
`BoundType::append_key_with_context` select `KeyContext::Equality` or
`KeyContext::SortEquivalence`. Neither promises byte-sort order. The default
delegates to the selected adapter's existing key method; ordinary key APIs
retain Equality. Binding retention, metadata/physical/logical validation, NULL
framing, append-only 16 MiB limits, cancellation and failed-append rollback
remain unchanged. Family consumers must retain and forward selected child
adapters, not create a private registry or compare raw Value objects.

Prerequisite checks: `cargo check`, all 25 `types` tests, workspace/all-target
clippy with warnings denied, instrumentation coverage (no missing entries),
and workspace/all-target trace check pass. The context test covers replacement
retention, an unrelated empty execution registry, old-adapter byte equivalence,
NULL framing, malformed values and fatal selected validation. Existing partial
write/cancellation/oversized-key tests run under both contexts. No performance
claim or independent Kani run applies to this internal prerequisite; the next
substantial combined checkpoint must report the maintained exploratory suite.

## Source metadata prerequisite

`ScalarBindArguments::full_integer_literal` preserves unsigned as well as signed
literal identity; the default adapts the existing signed hook. SQL delegates to
its existing source-identity classifier, so casts, parameters, columns and
evaluated expressions do not become literals. `combination_cast_mode` requests
the frontend's selected implicit-first/explicit-fallback policy after a family
has inferred its target. Unsupported frontends reject that request explicitly.
Both hooks validate the argument index, perform no evaluation and grant no
unregistered cast capability: ordinary selected binding still checks the actual
source/target/mode signature. Consumers retain the result and disable only the
additional literal-mode rewrite when its selected policy already accounts for
the source.

All 77 contract tests and workspace/all-target clippy pass for this prerequisite.
The new tests retain full unsigned endpoints, directly negated literals, typed
casts/columns/parameters, unevaluated failing/effectful children, selected
implicit-mode precedence, and default-frontends' signed adaptation/bounds.
