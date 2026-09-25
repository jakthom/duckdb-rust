# SQLite testing review

Reviewed 2026-09-09 against SQLite's own descriptions. SQLite combines independent
harnesses with generated SQL, malformed databases, boundary checks, disabled
optimizations, resource accounting and coverage. Its failure sweeps advance the
failing allocation or I/O operation, testing both one failure and continuing
failures. Crash simulation includes unsynchronized writes, and compound tests
fail recovery itself. Its database/SQL fuzzer mutates both inputs together.
[How SQLite is tested](https://www.sqlite.org/testing.html).

TH3 also uses mutation testing to check whether assertions detect changed
behavior. TH3 requires a separate license; its private tests were not imported.
The additions here implement applicable methods independently.
[TH3 testing and license](https://www.sqlite.org/th3.html).

## Additions to this rewrite

| Check | Implementation and actual scope |
| --- | --- |
| Independent SQL oracle | [`test_generated_sql.py`](../scripts/test_generated_sql.py) runs 16 reproducible transaction histories against Python's SQLite library and the Rust engine. Compares ordered values, NULLs, rollback and aggregate results in their common integer SQL subset. |
| Predicate partitions | [`generated.rs`](../test/component/adversarial/generated.rs) compares predicates with an independent row model and recombines true/false/unknown partitions. Varies both optimizers, both executors and both scan/filter strategies over eight seeds and boundary predicates. |
| SQL plus file mutations | The same component mutates 128 native checkpoints and selects SQL to execute. Half the cases repair the modified block checksum to reach deeper readers. Panics and internal errors fail; read-only input bytes must remain unchanged. Failure artifacts preserve the file, SQL and seed. |
| Persistent I/O failures | [`faults.rs`](../test/component/adversarial/faults.rs) advances failure through every observed operation until an uninjected completion, with one-shot and persistent failures in checkpoint and WAL modes. Reopen checks acknowledged work, definite/uncertain outcomes and subsequent writes. |
| Ownership after errors | [`ownership.rs`](../test/component/adversarial/ownership.rs) repeats 64 adapter/prepared-statement/result lifetimes, including conversion errors. Weak references verify adapter release while owned result values survive. |
| Test sensitivity | [`run_mutations.py`](../scripts/run_mutations.py) verifies the unchanged subquery suite, then separately changes scalar cardinality, NULL membership and EXISTS negation in isolated copies. Compile errors, timeouts and broken baselines cannot count as detected faults. |
| Harness sensitivity | [`test_verification_harnesses.py`](../scripts/test_verification_harnesses.py) supplies wrong values, hashes, cardinalities and errors; it also rejects missing/duplicate cases and even a one-nanosecond median slowdown. |

These extend the existing native-file, truncation, process-exit, type-boundary,
adapter-conformance and independent DuckDB compatibility checks. They do not
establish the breadth or coverage achieved by SQLite's harnesses.

## Work still required

The engine needs a fallible allocation interface and allocation-failure sweeps.
Its current row limit does not simulate allocator failure. The file interface
also needs a persistence simulator for torn and reordered writes, plus faults
combined with recovery from those states. Current operation-boundary process
interruptions do not simulate power loss.

Weak-reference checks cover selected ownership chains; general heap, descriptor
and thread accounting remains absent. Add generated concurrent transaction
histories with an independent visibility/commit oracle, guided SQL/file fuzzing
with corpus replay, broader semantic mutations and branch/condition coverage.
Run the resulting suites across delivery builds, configurations and platforms.
None of those unimplemented scopes is counted as a passed test.

```sh
python3 -m unittest discover -s scripts -p 'test_*.py' -v
cargo test --offline --test adversarial
cargo test --offline --release --test adversarial
python3 scripts/run_mutations.py --report target/mutations-new.json
```

Each mutation campaign builds inside its temporary workspace. Verify the
unchanged baseline before interpreting a mutant result; a build-cache isolation
failure is not a detected engine mutation. Retain diagnostic output under the
ignored `target/` directory and record outstanding coverage in the
[parity backlog](parity-backlog.md).
