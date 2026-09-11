# Independent nested ParsedExpression fixtures

The [initial campaign](nested-parsed-expression-reference.json) and
[explicit-column-name campaign](nested-parsed-expression-names-reference.json)
each retain 203 pinned source/version attempts. Each parsed 200 expressions and
completed 200 stable C++ deserialize/reserialize cycles. The other three attempts
are release parsing `{}` at storage 64, 65 and 68: the pinned release rejects this
syntax, whereas development accepts it. Development governs that disagreement.

The helper calls Parser::ParseExpressionList and ParsedExpression serialization
directly. It never binds an expression, executes it or changes a catalog. A
failing cast inside struct_pack therefore remains a CastExpression child, not an
evaluated error or folded Value. Raw bytes, class/type identifiers, literal
declared metadata, aliases, function qualification/names and argument names are
retained independently of diagnostic ToString output. The second campaign also
records each raw column qualification component. Diagnostic text is not the
comparison oracle. Stable cycles mean the complete decoded wire/inventory output
matches the next decode cycle under the same selected compatibility version.

The helper links the existing pinned libraries. Reports include verified CLI and
loaded library version/source identities, library/executable/helper digests,
compilation commands and unchanged Rust source digests. No shared C++ checkout
was edited or rebuilt. Versions 64/65/68 use both pins; development also covers 69.
These are independent codec fixtures, not a Rust ParsedExpression implementation,
bidirectional Rust/C++ DEFAULT test or general catalog publication result.

## Source-confirmed mappings

| Source syntax | Raw retained expression |
| --- | --- |
| `[a,b]` | FunctionExpression list_value |
| `ARRAY[a,b]` | OperatorExpression ARRAY_CONSTRUCTOR (156) |
| `{field: value}` | FunctionExpression struct_pack, argument name plus child alias |
| `(a,b)` | FunctionExpression row |
| `MAP {key:value}` | FunctionExpression map with ordered key/value list_value children |
| `(constructor).field[index]` | STRUCT_EXTRACT (155), then ARRAY_EXTRACT (153) |
| `t.s.Items[2]` | ARRAY_EXTRACT over one qualified ColumnRefExpression (203) |

The numbers are wire observations only; catalog IR uses typed operator variants,
not native numeric tags. Legacy versions serialize function argument names as
child aliases, which development reopens with legacy positional-call provenance.
Modern version 69 retains named arguments and qualification separately.

Qualified names must not be split blindly into field operators. For example,
`"t.q"."s.x"."Items.y"[2]` retains three exact ColumnRef components, including
embedded dots. Closed stored defaults reject those row-dependent bases; a future
broader retained-expression subsystem needs an explicit qualified reference node.
The family capture helper is not authority to invent a qualifier split or eagerly
evaluate a base expression. Slices, column-dependent defaults, qualified function
resolution and arbitrary native operators remain separate unsupported paths.

Reproduce with `python3 scripts/native_nested_expression_reference.py --output
<new-report-path>`. It refuses to overwrite retained evidence. This helper is
ordinary reference evidence, not performance measurement or a Kani proof.
