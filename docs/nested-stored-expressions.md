# Nested stored-expression binding

This is a provisional binding increment toward retained defaults, not native
DEFAULT persistence or ordinary INSERT/ALTER demand completeness. Catalog default
migration and native ParsedExpression publication remain integration-lead work.

## Selected construction and access

`list_value`, `array_value`, `row`, `struct_pack` and `union_value` now bind through
the selected FunctionRegistry. Bracket/ARRAY list syntax and struct literals use
the same adapters. The old direct builtin Constructor bypass is removed. A custom
function catalog must explicitly provide the constructors it wants; an accessor
or MAP adapter alone does not implicitly install list construction.

Constructors retain a selected BoundType containing child bindings, argument
types and exact selected combination-cast modes. Binding reads declared metadata,
names and aliases without evaluating children. Typed NULLs retain their declared
child type; an active NULL UNION member remains a non-NULL UNION. LIST/ARRAY
construction requests the collection-template inference policy, not the Ordered
policy used by other contextual functions. No private type/cast registry appears.

Explicit argument names and legacy child aliases are separate frontend metadata.
Only a selected adapter opting into named arguments accepts explicit names.
LegacyAliases stored calls are positional and expose the retained name as an
alias; modern Named calls expose it as an explicit name. Constructor adapters
interpret these metadata; neither catalog IR nor a wire codec dispatches by a
special SQL name. Child and argument order are retained, and explicit names take
precedence over child aliases as in development's function binder.

The stored binder now lowers ListConstructor, Index and Field operators through
selected constructor/accessor functions. These operators remain distinct from
FunctionExpression's `is_operator` flag, which is still explicitly unsupported.
MAP is a retained call with key/value list expressions; VARIANT and fixed ARRAY
targets retain selected explicit casts. Native operator numeric tags belong only
to the format codec, not the catalog enum.

## Source observations and limits

Pinned development transforms `[a,b]` into FunctionExpression(list_value),
`ARRAY[a,b]` into OperatorExpression(ARRAY_CONSTRUCTOR), tuples into row calls,
and struct literals into named struct_pack calls. Its operator binder maps
ARRAY_CONSTRUCTOR to list_value: ARRAY syntax is LIST, whereas array_value returns
fixed ARRAY. Source: `src/parser/peg/transformer/transform_expression.cpp` and
`src/planner/binder/expression/bind_operator_expression.cpp` in the pinned tree.

`src/planner/binder/expression/bind_function_expression.cpp` preserves legacy
argument names as aliases but binds them positionally. `struct_pack.cpp` and
`extension/core_functions/scalar/union/union_value.cpp` derive field/tag metadata
from argument aliases and declared return types. Direct probes retain missing-name
errors: `struct_pack(a:=1,2)` rejects, but `struct_pack(a:=1,b)` accepts a real
column alias b. The frontend retains source order; selected constructor metadata
decides whether an alias supplies the missing field name. Empty STRUCT/TUPLE type strings are STRUCT and
TUPLE, without parentheses.

This is not general named-overload resolution, qualified stored function binding,
scalar operator completeness, slices, JSON-extension access or VARIANT path
completeness. CompoundFieldAccess capture still requires independent raw parsed
fixtures to establish base/qualification and exact operator identity; do not infer
a complete native mapping from an isolated stored-tree test. SQL source aliases
are currently captured for explicit names and direct identifier expressions.

## Validation status

The focused consumer checks pass: contracts 66/66, nested 42/42. They include
both scalar/batch evaluators, typed NULLs across all nested families, replacement
functions accepting/declining names, legacy positional aliases, selected child
validation with an unrelated empty ambient registry, fatal Resource propagation,
cancellation, collection literal ordering and prepared-parameter identity. The
existing mixed-type mutation/rollback/checkpoint/WAL/reopen tests remain passing.

Two old replacement-catalog tests initially failed because they depended on the
removed private list constructor. Their composition now explicitly registers
selected list_value; separate tests require a missing constructor to remain a
Catalog error and prove literals honor a replacement constructor.

Full workspace and instrumentation verification passed before the final narrow
alias-order correction; its follow-up checks are recorded with the delivered
commit. This internal increment has not independently run Kani or acceptance
timing. The lead runs maintained Kani at the next substantial combined checkpoint;
earlier checkpoint results do not prove these new adapters or stored mappings.
