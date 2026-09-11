# Qualified nested paths

The combined engine at `748dfba` includes the family increments and shared
qualification repair `e04c8ee`. The broader value-and-expression milestone
continues; this is a usable integration increment, not full type/SQL parity.

`table.column[1]` now resolves the column namespace before extracting a child.
The parser's compound-access representation no longer mistakes the table name
for a column. Dotted prefixes use the longest available table/schema-qualified
column within each scope, then selected nested accessors. Quoted dots,
parenthesized value roots, grouped whole values, grouped paths and correlated
references retain their distinct meanings. Ambiguous names, illegal grouping
and selected accessor failures are not swallowed by recursive prefix retries.

Two mixed-schema execution compositions exercise DECIMAL, TIMESTAMP_NS, LIST
and nested STRUCT children through prepared parameters, joins, aggregation,
sorting, windows, indexed-table mutations, rollback and native reopen. The
replacement-catalog test checks qualified subscripts with prepared parameters.
Direct qualified-subscript joins are also added to all three independent native
VARIANT fixture tests.

The native follow-up passes
**49/49** checks: development unshredded 17/17, development shredded 16/16, and
release-produced shredded 16/16 under the development semantic oracle. All
three formerly failing direct joins now pass. Source, binary and fixture
identities are retained and files remain unchanged. The preceding 46/49 report
remains intact. This is read-side compatibility: native VARIANT publication and
WAL writing remain unsupported.

The initial qualification campaign matches
20/22 cases against each pin. Both remaining mismatches are retained:

- The shell emits a LIST as a JSON string (`"[42]"`) rather than a JSON array
  (`[42]`). The public typed-query test for the QUALIFY expression passes; its
  shell representation is a separate real transport gap.
- Reusing a prior SELECT-list alias in `SELECT [42] xs,xs[1]` fails binding,
  although both pins accept it. Same-list alias reuse is separate from existing
  QUALIFY alias support and remains unimplemented.

The projection follow-up keeps
all original cases and adds a scalar-only outer projection of the same QUALIFY
query. It matches 21/23 cases on both pins; the original two mismatches remain,
without result normalization or suppressed errors. Error checks compare
categories, not complete message text. These campaigns do not verify complete
metadata, performance or arbitrary attached-catalog/nested-schema names.

Ordinary check and all-target clippy pass. Nested 28/28, contracts 27/27,
grouping 9/9 and subqueries 15/15 pass. An initial test edit used a nonexistent
builder method and failed compilation; selecting FileCheckpoint explicitly
repaired the test. A new same-SELECT-alias expectation then exposed the existing
gap above; it was retained in differential evidence, while the component test
now verifies the supported QUALIFY alias path. The combined workspace,
instrumentation and exploratory Kani checkpoint are recorded in the main
[integration report](value-expression-progress.md) as they complete.
