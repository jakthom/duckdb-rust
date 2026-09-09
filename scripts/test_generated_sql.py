"""Reproducible common-SQL differential and predicate-partition checks."""
import json
from pathlib import Path
import random
import sqlite3
import subprocess
import tempfile
import time
import unittest

from run_upstream import RustEngine
from sqllogic import Runner, parse
from upstream_suite import ROOT


class GeneratedSQL(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        subprocess.run(["cargo", "build", "--offline", "--release", "--bin", "duckdb-rust-test-worker"], cwd=ROOT, check=True)

    def test_seeded_transactions_and_null_partitions_match_independent_sqlite(self):
        for seed in range(16):
            history = []
            with self.subTest(seed=seed), tempfile.TemporaryDirectory(prefix="ddb-generated-") as scratch:
                engine = RustEngine(ROOT / "target/release/duckdb-rust-test-worker", scratch, time.monotonic()+30)
                reference = sqlite3.connect(":memory:", isolation_level=None)
                rng = random.Random(seed)

                def statement(sql):
                    history.append(sql)
                    result = engine.request({"operation": "statement", "sql": sql})
                    self.assertTrue(result["ok"], result)
                    reference.execute(sql)

                def query(sql):
                    history.append(sql)
                    result = engine.request({"operation": "query", "sql": sql})
                    self.assertTrue(result["ok"], result)
                    expected = [["NULL" if x is None else str(x) for x in row] for row in reference.execute(sql)]
                    self.assertEqual(result["rows"], expected, sql)
                    return result["rows"]

                try:
                    statement("CREATE TABLE t(i INTEGER PRIMARY KEY,x INTEGER,y INTEGER)")
                    for iteration in range(12):
                        statement("BEGIN")
                        for _ in range(5):
                            i, x, y = rng.randrange(32), rng.choice(["NULL", str(rng.randrange(-16, 17))]), rng.randrange(-8, 9)
                            if rng.randrange(2):
                                statement(f"DELETE FROM t WHERE i={i}")
                                statement(f"INSERT INTO t VALUES({i},{x},{y})")
                            else:
                                statement(f"UPDATE t SET x={x},y={y} WHERE i={i}")
                        statement("COMMIT" if rng.randrange(2) else "ROLLBACK")
                        full = query("SELECT i,x,y FROM t ORDER BY i")
                        k = rng.randrange(-16, 17)
                        predicate = rng.choice([f"x<{k}", f"x={k}", f"x>{k} OR y<0", f"x<{k} AND y>=0", "x=y"])
                        partitioned = query(f"SELECT i,x,y FROM t WHERE {predicate} UNION ALL SELECT i,x,y FROM t WHERE NOT({predicate}) UNION ALL SELECT i,x,y FROM t WHERE ({predicate}) IS NULL ORDER BY i")
                        self.assertEqual(full, partitioned)
                        query("SELECT count(*),count(x),sum(x),min(y),max(y) FROM t")
                except BaseException:
                    output = ROOT / "target/generated-failures" / f"seed-{seed}.json"
                    output.parent.mkdir(parents=True, exist_ok=True)
                    output.write_text(json.dumps({"seed": seed, "sql": history}, indent=2)+"\n")
                    raise
                finally:
                    reference.close()
                    engine.close()

    def test_load_restart_reconnect_and_named_connections_preserve_upstream_lifecycle(self):
        with tempfile.TemporaryDirectory(prefix="ddb-lifecycle-") as scratch:
            engine = RustEngine(ROOT / "target/release/duckdb-rust-test-worker", scratch, time.monotonic()+30)
            try:
                runner = Runner(engine, {"{TEST_DIR}": scratch})
                runner.run(parse("""load {TEST_DIR}/case.duckdb
statement ok writer
CREATE TABLE t(i INTEGER); INSERT INTO t VALUES(1)

statement ok reader
BEGIN

query I reader
SELECT count(*) FROM t
----
1

statement ok writer
INSERT INTO t VALUES(2)

query I reader
SELECT count(*) FROM t
----
1

reconnect
query I reader
SELECT count(*) FROM t
----
2

restart
query I
SELECT count(*) FROM t
----
2

load {TEST_DIR}/case.duckdb readonly
query I
SELECT count(*) FROM t
----
2

load {TEST_DIR}/case.duckdb readwrite
statement error
SELECT * FROM t
----
does not exist
"""))
                self.assertEqual((runner.passed, runner.skipped), (9, 0))
                result = engine.request({"operation": "load", "path": str(Path(scratch).parent / "unowned.duckdb")})
                self.assertTrue(result.get("unsupported"))
            finally:
                engine.close()


if __name__ == "__main__":
    unittest.main()
