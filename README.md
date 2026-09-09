# duckdb-rust

A port of DuckDB to Rust.

**This is highly experimental!!!**

I'm experimenting. Playing. Having fun with databases, code, and science. Harnessing (human) brilliance with (ai) brilliance to see what I can learn and do.

**You probably shouldn't use this for production use.** Use or follow along at your own risk. You might get burned. I'm sure I will. But it will be a fun ride.

## Guiding principles

- **Zero performance regressions:** 'nuff said. DuckDB is ridiculously fast and good. Why spend time making something worse in another language? Making things faster is a different story...
- **DuckDB compatibility:** preserve DuckDB behavior and compatibility with its extension ecosystem as internals evolve.
- **Spec-first:** design, arhitecture, contrats, and verification requirements were generated and thoroughly reviewed prior to any code being written.
- **Pluggability:** Certain internals will shuffle behind explicit contracts, not just extension hooks. So the guts of the machine can be hot-swapped - not just the interfaces (via extensions).
- **Measure. Experiment. Learn. Improve. Measure again...** because it's the way humanity advances.
- **Open interop** with clean interfaces to file formats, storage engines, extensions, and other databases.
- **Flexible workloads? Maybe??:** preserving OLAP strengths while exploring a consistent interface to OLTP, graph, and ai-centric workloads would be cool.
- **Iterative refinement:** because improvement never happens at once.

See the [engineering specs](specs/README.md) and [rewrite principles](specs/rewrite-principles.md).
