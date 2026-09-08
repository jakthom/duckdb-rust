# SQL parser and PEG grammar

[Specification index](../README.md) · [Testing](../testing/README.md)

Source baseline: DuckDB `99063af2bd7092aff02e14184a20e24699d34d71` (2026-09-08). This document describes the C++ implementation; it does not assert that an equivalent Rust implementation exists. Source observations and additional engineering implications are distinguished below. Verification coverage describes available checks, not executed results.

## Parsing

The parser validates input and tokenizes SQL, applies a compiled PEG grammar, and transforms grammar matches into DuckDB syntax objects. The major object families are `SQLStatement`, `QueryNode`, `ParsedExpression`, `TableRef`, and statement-specific parsed-data structures. Parsed expressions are syntax objects: they do not yet carry resolved catalog bindings and executable type semantics.

Grammar files live under [src/parser/peg/grammar](../../../duckdb/src/parser/peg/grammar/); transformer implementations live under [src/parser/peg/transformer](../../../duckdb/src/parser/peg/transformer/). [The PEG README](../../../duckdb/src/parser/peg/README.md) describes grammar development. Tokenization/highlighting and grammar extension facilities also support the shell and extension-defined syntax. The runtime has a parser cache; grammar compilation is not necessarily repeated independently for every query.

Parser errors preserve query-location information. UTF-8 validation is an explicit boundary before tokenizer processing. Expression depth and malformed-input handling are correctness and robustness concerns, covered by SQL parser regressions and fuzz cases.

## Component boundary and source map

The parser decides whether source text has a syntactic interpretation and constructs unbound syntax objects. It does not look up a table's columns or choose a join algorithm. [Planner](planner.md) owns name/type resolution; [physical planning](physical-planner.md) owns algorithm selection. Keeping these boundaries explicit allows syntax inspection and statement iteration without executing a query.

| Internal component | Source and responsibility |
| --- | --- |
| Parser facade | [parser.hpp](../../../duckdb/src/include/duckdb/parser/parser.hpp): whole-query parsing, top-level statement parsing, expression/list helpers, keyword/token APIs |
| SQL normalization/tokenization | [parser.cpp](../../../duckdb/src/parser/parser.cpp), [tokenizers](../../../duckdb/src/parser/peg/tokenizer/): validate bytes, normalize supported spaces, classify tokens with locations |
| Grammar-language parser | [peg_parser.cpp](../../../duckdb/src/parser/peg/peg_parser.cpp): parse grammar definitions; distinct from parsing user SQL |
| Compiled grammar | [compiled_grammar.hpp](../../../duckdb/src/include/duckdb/parser/peg/compiled_grammar.hpp): immutable matcher graph, tokenizer and keyword helper for a grammar selection |
| Matcher and parse results | [matcher interfaces](../../../duckdb/src/include/duckdb/parser/peg/matcher/), [AST results](../../../duckdb/src/include/duckdb/parser/peg/ast/): record successful grammar choices and children |
| Packrat cache | [parser_packrat.hpp](../../../duckdb/src/include/duckdb/parser/peg/parser_packrat.hpp): key by matcher ID/token index, remember success, end/farthest token positions and result reference |
| Semantic transformer | [transformers](../../../duckdb/src/parser/peg/transformer/): construct statements, query nodes, table references and expressions |
| Incremental client interface | [parse iterator](../../../duckdb/src/include/duckdb/main/parse_iterator.hpp): integrate statement-at-a-time parsing with client lifetime |

## Parsing sequence and extension dispatch

1. Normalize the source, including UTF-8 validation. The parser must reject invalid input before tokenizers assume valid character boundaries.
2. Consult configured parser overrides. Strict override errors and fallback/default behavior are policy decisions, separate from grammar matching.
3. Tokenize normalized SQL using the selected compiled grammar's tokenizer.
4. Repeatedly match one TopLevelStatement. Separator-only input can consume tokens without yielding a statement.
5. On a per-statement PEG failure, offer the remaining token stream to parser extensions. A negative consumed-token count reports an extension error; zero declines the input; a positive count claims that many tokens and is bounds-checked.
6. Transform successful matches into owned syntax objects. For the eager whole-query path, populate each statement's query string and rebase its location to that string.

ParseTopLevelStatement advances an existing TokenIterator and intentionally does not fill the returned statement's query string. Callers of that lower-level interface own the source and location interpretation. Mixing eager and lazy paths without that distinction can create incorrect diagnostics or source slices.

## Grammar and ownership contracts

PEG alternatives are ordered. Inserting a broad alternative ahead of a more specific one can change accepted syntax even if both productions still exist. Grammar child order also forms an interface to transformers: adding an optional element changes the structure a transformer must inspect. Generated typed wrappers reduce, but do not remove, the need to implement semantic transformation correctly.

The per-database ParserCache protects publication of a shared compiled base grammar. Extension-selected grammars can have their own keyword/tokenizer/rule state. Matcher and parse-result references must remain valid for transformation; they are not reusable catalog objects. The packrat entry contains a parse-result reference, so cache and parse-result allocation lifetimes must agree.

The PEG README has historical autocomplete-era paths. Use the actual grammar under src/parser/peg/grammar and the current build script as the path authority; do not infer current matcher semantics from every historical note in that README.

## Failure cases and change obligations

An engineering change must account for multi-statement input, empty/separator runs, quoted identifiers, string/dollar quoting, comments, Unicode normalization, deeply nested syntax, and errors at the last token. Extension claims must not skip beyond input or cause a no-progress loop. A syntax addition also needs copy/serialization/rendering support for any new AST representation it introduces.

A useful trace is `SELECT a FROM t; SELECT b FROM u`: parsing should yield two independent syntax statements with appropriate query text; neither statement establishes that a, b, t or u exists. Binding errors belong to the next stage.

## Verification links

Use [SQLLogicTest](../testing/sqllogictest.md) for accepted/rejected syntax, [fuzzer](../testing/fuzzer.md) for malformed-byte and syntax robustness, and [configuration](../testing/configuration.md) for statement-copy/render/serialization checks. Native parse-iterator and grammar-extension tests cover the interface that SQL-only eager parsing misses. Acceptance of one newly valid query is insufficient if it changes parsing of a neighboring production.
