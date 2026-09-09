# duckdb-rust

An experimental database engine implemented in Rust, with an embedded API and SQL command-line tool. It runs independently of the C++ DuckDB engine. The directory structure follows the original system: `src/common`, `catalog`, `parser`, `planner`, `optimizer`, `execution`, `storage`, `transaction`, `function`, `parallel`, and `main`, with `tools`, `test`, `benchmark`, and `scripts` alongside them.

**The full DuckDB rewrite is unfinished.** This implementation executes a relational SQL subset and natively reads and writes a subset of DuckDB checkpoints. It does not yet offer general DuckDB feature, file, extension, or performance compatibility. See [implemented contracts and remaining work](docs/architecture.md).

Acceptance requires [all upstream tests and zero performance regressions against
C++](docs/testing-parity.md). The complete pinned source/test inputs are retained;
passing test parity remains incomplete. [SQLite-inspired checks](docs/sqlite-testing-review.md)
add independent generated SQL, persistent faults, file mutations and test-oracle checks.

## Build and run

Requires Rust 1.96 or later. Cargo builds the engine and SQL parser; no DuckDB installation or C++ compiler is needed.

```sh
cargo build --release --bin duckdb-rust
./target/release/duckdb-rust :memory: -c 'SELECT sum(range) FROM range(1000)'
./target/release/duckdb-rust example.duckdb -c "CREATE TABLE items(id INTEGER, name VARCHAR); INSERT INTO items VALUES (1,'duck'); SELECT * FROM items"
./target/release/duckdb-rust example.duckdb --read-only --json -c 'SELECT * FROM items'
./target/release/duckdb-rust example.duckdb --read-only -c 'SELECT name FROM items WHERE id IN (SELECT max(id) FROM items)'
./target/release/duckdb-rust example.duckdb --durability wal -c "INSERT INTO items VALUES (2,'logged')"
./target/release/duckdb-rust example.duckdb --durability wal --checkpoint-commits 100 -c "INSERT INTO items VALUES (3,'checkpointed'); CHECKPOINT"
printf 'SELECT count(*) FROM items;\n' | ./target/release/duckdb-rust example.duckdb
```

Files use the DuckDB checkpoint format by default. `--format snapshot` selects the private Rust snapshot format, with the same supported catalog and value model. Without `-c`, the shell reads SQL from standard input through EOF. JSON output is an array of objects; column names must be distinct, and dates and nonfinite floating-point values are strings. Numbered parameters (`$1`, `$2`) are available through the embedded API.

`--durability wal` and `Database::open_logged` select native transaction logging.
Each commit appends and syncs its changes. Automatic checkpoints run before an
incoming append would take the log above 16 MiB, provided an earlier commit is
available to checkpoint. `--checkpoint-bytes N` changes that threshold;
`--checkpoint-commits N` instead checkpoints after N nonempty commits, before
the next append. `CHECKPOINT` and `Connection::checkpoint()` explicitly publish
committed work and retire the log. Existing readers retain their snapshots.
Reopening also recovers and checkpoints the log before starting a new session.
The default full-checkpoint adapter remains independently selectable through
the same transaction interfaces.

```rust
use duckdb_rust::{Database, Result, Value};

fn main() -> Result<()> {
    let database = Database::memory()?;
    let mut connection = database.connect();
    let statement = connection.prepare("SELECT $1 + 1 AS answer")?;
    let result = connection.execute_prepared(&statement, &[Value::Integer(41)])?;
    assert_eq!(result.rows, vec![vec![Value::Integer(42)]]);
    Ok(())
}
```

`Connection::query_batches` and `execute_prepared_batches` consume owned chunks through a callback and return schema/completion metadata. Return `StreamControl::Stop` to finish early. The normal query API still collects an owned result. See [examples/streaming.rs](examples/streaming.rs).

`DatabaseBuilder` selects transaction management, indexes, durability, parsing, binding, optimization, physical planning, expression evaluation, execution, scheduling, casts, operators, types, and function registration. The selected transaction bundle owns type semantics. Bound cast expressions retain the selected conversion adapter; vector and table boundaries require explicitly typed values. Checkpoint formats, segment decoders and file publication have separate contracts. `--adapters` prints the selected implementations. See [examples/embedding.rs](examples/embedding.rs), [operator replacement](examples/operators.rs), and the [architecture](docs/architecture.md).

## Current capabilities

- Schemas, tables, literal defaults, `NOT NULL`, primary and unique keys; transactional `CREATE`, `DROP`, `INSERT`, `UPDATE`, and `DELETE`.
- Projection, filtering, inner/outer/semi/anti joins with `ON`, aggregation, `GROUP BY`, `HAVING`, sorting, limits, distinct, unions, nonrecursive CTEs, and `range`/`generate_series`.
- Scalar, `EXISTS` and `IN` subqueries, including lexical correlations, nested CTE references and transactional mutations. Scalar cardinality and NULL membership follow explicit checked contracts; [subquery limits](docs/architecture.md#subqueries) remain.
- Booleans, signed 8/16/32/64/128-bit integers, 32-bit FLOAT and 64-bit DOUBLE, UTF-8 strings, Gregorian `DATE` values (including BCE and infinities), and SQL NULLs.
- Registered operator overloads with explicit coercions, checked arithmetic, date offsets/differences, integer division, and interchangeable LIKE algorithms.
- Registered logical types with parameterized metadata, selected comparison/key behavior, explicit casts and private-format persistence; [an opt-in ASCII example](examples/registered_type.rs) demonstrates replacement.
- Incremental scans, filters, projections, limits, distinct and unions; batch aggregation; selectable pull and eager executors.
- Explicit commit/rollback, isolated snapshots, optimistic writer conflicts, prepared statements, cancellation, deadlines, and intermediate row limits.
- Hash and B-tree equality indexes with composite and nullable keys; independently selected optimizer passes use eligible indexes for SELECT queries.
- Read-only and writable recovery of supported DuckDB v2 WAL records: committed schema/table changes, inserts, updates, deletes, and incomplete transaction tails. Read-only opens preserve both files; writable opens publish the recovered checkpoint and retire the log, including after interrupted recovery.
- Selectable native WAL durability with ordered transaction journals, row-ID translation, atomic log initialization, durable append rollback, and explicit uncertain outcomes.
- Automatic and explicit checkpoints through independently selected size/count policies; validated physical row mappings preserve live transaction identities across compaction.
- Native DuckDB checkpoint decoding for uncompressed, constant, RLE, bitpacking, dictionary, FSST, ALP, ALP-RD, Chimp, and Patas data, including overflow strings and committed deletion masks. Native checkpoint writing uses uncompressed columns, validity masks, literal defaults, and ART primary/unique indexes.

Unsupported SQL and file features return errors. Reading rejects unsupported catalog objects, nonconstant default expressions, types, codecs, encryption, and concurrent checkpoint WAL transitions. Runtime indexes currently rebuild on table mutation; SQL index DDL, range seeks, and indexed UPDATE/DELETE planning remain unfinished. Opening a file still loads its complete snapshot into memory; local checkpoint input is limited to 512 MiB and strings to 16 MiB. Checkpoints pause commit publication; background/concurrent checkpointing and group commit remain unfinished. Log scheduling thresholds are not hard memory or transaction-size limits. This is not a production storage engine.

## Verify

```sh
cargo test --all-targets
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo run --bin sqllogictest -- test/sql/relational.test
python3 -m unittest discover -s scripts -p 'test_*.py' -v
python3 scripts/upstream_suite.py
```

The full upstream and C++ performance campaigns are separate, currently failing
acceptance gates. Their [runbook and recorded gaps](docs/testing-parity.md) retain
failures and unsupported cases; ordinary Cargo success does not imply full parity.

Checked-in fixtures include databases generated by DuckDB v1.3.0 and historical Chimp/Patas databases from the source tree. Normal Cargo tests need no DuckDB executable. To verify values and file mutations in both engines, install an independent DuckDB CLI and run:

```sh
cargo build --release --bin duckdb-rust
python3 scripts/verify_reference.py
```

[Fixture provenance](test/data/duckdb/manifest.json), [verification scope](docs/verification.md), [engineering specifications](specs/README.md), and [rewrite principles](specs/rewrite-principles.md).
