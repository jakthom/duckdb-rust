# Pinned upstream test source

`source.tar.gz` retains **every tracked file**, including tests, fixtures, native
harnesses, clients, configurations, generators and build sources, from DuckDB
`99063af2bd7092aff02e14184a20e24699d34d71`. The upstream license is included in the
archive. No assertions or expected outputs were rewritten.

`manifest.json` records every path, file mode, byte count and SHA-256 digest,
the archive digest, and a source-level test inventory. Symlink fixture targets
are preserved. The native/client declaration inventory is a porting backlog,
not the expanded compiled test registry or a report of passing tests.

Run `python3 scripts/upstream_suite.py` from the repository root to verify the
archive and complete inventory. `--extract NEW_DIRECTORY` validates and extracts
into a new directory. `scripts/run_upstream.py` consumes this exact archive and
reports SQL outcomes and unported obligations. Full test parity remains open.
