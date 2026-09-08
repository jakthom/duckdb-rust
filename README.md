# duckdb-rust

An experimental port of DuckDB to Rust.

**Highly experimental.** This project is building a working, DuckDB-compatible database with a pluggable Rust architecture. Not for production use.

## Guiding principles

- **Pluggable by construction:** replaceable internals behind explicit contracts, not just extension hooks.
- **DuckDB compatibility:** preserve DuckDB behavior and compatibility with its extension ecosystem as internals evolve.
- **Workload flexibility:** preserve OLAP strengths while exploring OLTP, graph, random access, and AI.
- **Open interoperability:** clean interfaces for file formats, storage engines, extensions, and other databases.
- **Measured experimentation:** interchangeable algorithms, shared tests, and reproducible benchmarks.
- **Evidence-backed design:** decisions grounded in code, community needs, and related systems.
- **Specification first:** define contracts and verification requirements before implementation.
- **Iterative refinement:** sharpen priorities, test assumptions, and remove premature scope.

See the [engineering specs](specs/README.md) and [rewrite principles](specs/rewrite-principles.md).
