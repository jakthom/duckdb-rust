"""Independent native reads and continued writes of Rust transaction logs."""
import hashlib
import gzip
import os
from pathlib import Path
import subprocess

from wal_reference import ROOT, pair, worker_binary, compare_pair

STEPS = ['LogInitializeCreate', 'LogInitializeWrite', 'LogInitializeSync',
         'LogInitializeRename', 'LogInitializeDirectorySync', 'LogAppendWrite',
         'LogAppendSync', 'LogRollbackTruncate', 'LogRollbackSync', 'Acknowledged']
CASES = {
    'native_holes': (
        None,
        """BEGIN; UPDATE t SET i=i+100 WHERE i%2=0; DELETE FROM t WHERE i=3;
        INSERT INTO t(i,s) VALUES(20,'insert'); UPDATE t SET s='own insert' WHERE i=20; COMMIT;
        UPDATE t SET s='remapped' WHERE i=102; INSERT INTO t(i,s) VALUES(21,'next');""",
        'SELECT i,s,d::VARCHAR d,n,b FROM t ORDER BY i'),
    'row_identity': (
        "CREATE TABLE t(i INTEGER PRIMARY KEY,s VARCHAR DEFAULT 'base'); INSERT INTO t VALUES(1,'one'),(2,'two'),(3,'three'); DELETE FROM t WHERE i=2;",
        """BEGIN; CREATE SCHEMA transient; CREATE TABLE transient.gone(i INTEGER);
        INSERT INTO transient.gone VALUES(1); DROP TABLE transient.gone; DROP SCHEMA transient;
        CREATE TABLE IF NOT EXISTS t(x INTEGER); DROP TABLE IF EXISTS absent;
        UPDATE t SET i=i+10; INSERT INTO t VALUES(4,'four'); UPDATE t SET s=NULL WHERE i=4;
        DELETE FROM t WHERE i=13; INSERT INTO t VALUES(5,'five'); COMMIT;
        UPDATE t SET s='remapped' WHERE i=4; DELETE FROM t WHERE i=5;
        INSERT INTO t(i) VALUES(6); BEGIN; INSERT INTO t VALUES(99,'rolled back'); ROLLBACK;""",
        'SELECT * FROM t ORDER BY i'),
    'primitive_types': (
        'CREATE SCHEMA extra;',
        """CREATE TABLE extra.t(i INTEGER PRIMARY KEY, a TINYINT, b SMALLINT, c INTEGER,
        d BIGINT, e HUGEINT, f FLOAT, g DOUBLE, h DATE, s VARCHAR DEFAULT '🦆', ok BOOLEAN);
        INSERT INTO extra.t VALUES(1,-128,-32768,-2147483648,-9223372036854775808,
        '-170141183460469231731687303715884105728'::HUGEINT,'NaN'::FLOAT,'Infinity'::DOUBLE,DATE '-infinity','🦆',true),
        (2,127,32767,2147483647,9223372036854775807,'170141183460469231731687303715884105727'::HUGEINT,
        '-Infinity'::FLOAT,'-0'::DOUBLE,DATE 'infinity','',false),(3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL);
        BEGIN; UPDATE extra.t SET h=DATE '0001-01-01 (BC)', s='updated', ok=NULL WHERE i=1;
        INSERT INTO extra.t(i,h) VALUES(4,DATE '2000-02-29'); UPDATE extra.t SET s='own insert' WHERE i=4; COMMIT;""",
        'SELECT i,a,b,c,d,e,f::DOUBLE f,g,h::VARCHAR h,s,ok FROM extra.t ORDER BY i'),
    'batches': (
        "CREATE TABLE t AS SELECT i::INTEGER i,'base' s FROM range(5013) r(i); DELETE FROM t WHERE i%7=0;",
        """BEGIN; UPDATE t SET s=CASE WHEN i%2=0 THEN NULL ELSE 'changed' END WHERE i%3=0;
        DELETE FROM t WHERE i>=2048 AND i<4096; INSERT INTO t SELECT i,'new' FROM range(5013,8026) r(i);
        UPDATE t SET s='own insert' WHERE i>=6000; DELETE FROM t WHERE i>=7000; COMMIT;
        UPDATE t SET s='remapped again' WHERE i>=6000; INSERT INTO t VALUES(9000,'last');""",
        'SELECT * FROM t ORDER BY i'),
}


def verify(rust, logged, reference, command, directory):
    worker = worker_binary('logging')
    source = hashlib.sha256()
    for path in sorted([ROOT/'Cargo.toml', ROOT/'Cargo.lock', *(ROOT/'src').rglob('*.rs'), ROOT/'test/component/logging.rs']):
        source.update(str(path.relative_to(ROOT)).encode()+b'\0'+path.read_bytes())
    report = {'writer': 'duckdb-wal-v2-writer', 'cases': [], 'boundaries': [],
              'worker_binary_sha256': hashlib.sha256(worker.read_bytes()).hexdigest(), 'worker_source_sha256': source.hexdigest(),
              'scope': 'Append-only native transaction logs, independent reads and continued writes; process exits at named I/O boundaries and after acknowledged commit. No power-loss simulation or commit-latency claim.'}
    for name, (setup, sql, query) in CASES.items():
        path = directory/f'logged-{name}.duckdb'
        oracle = directory/f'logged-{name}-oracle.duckdb'
        if setup is None:
            path.write_bytes(gzip.decompress((ROOT/'test/data/wal/mutations.duckdb.gz').read_bytes()))
        else:
            command(rust, path, setup)
        checkpoint = path.read_bytes()
        oracle.write_bytes(checkpoint)
        command(logged, path, sql)
        assert path.read_bytes() == checkpoint, 'ordinary logging rewrote checkpoint'
        command(reference, oracle, sql)
        expected = command(reference, oracle, query, json_output=True, readonly=True)
        key = expected[0]['i']
        before = pair(path)
        for engine in [rust, reference, rust]:
            assert command(engine, path, query, json_output=True, readonly=True) == expected, name
            assert pair(path) == before
        if name != 'batches':
            duplicate = directory/f'logged-{name}-duplicate.duckdb'
            duplicate.write_bytes(before[0]); Path(str(duplicate)+'.wal').write_bytes(before[1])
            table = 'extra.t' if name == 'primitive_types' else 't'
            try:
                command(reference, duplicate, f"INSERT INTO {table}(i) VALUES({expected[0]['i']})")
            except RuntimeError as error:
                assert 'Constraint Error' in str(error), str(error)
            else:
                raise AssertionError('native writer failed to enforce a WAL-restored key')
        # Complete checkpoint recovery, then log mutations against its compacted
        # row IDs. This crosses both physical identity transitions.
        table = 'extra.t' if name == 'primitive_types' else 't'
        command(logged, path, f"UPDATE {table} SET s='after recovery' WHERE i={key}")
        command(reference, oracle, f"UPDATE {table} SET s='after recovery' WHERE i={key}")
        expected = command(reference, oracle, query, json_output=True, readonly=True)
        compare_pair(rust, reference, command, path, query, expected)
        command(reference, path, f"UPDATE {table} SET s='native writer' WHERE i={key}; CHECKPOINT")
        command(logged, path, f"UPDATE {table} SET s='Rust continues' WHERE i={key}")
        assert command(reference, path, f"SELECT s FROM {table} WHERE i={key}", json_output=True, readonly=True) == [{'s':'Rust continues'}]
        assert command(reference, path, query, json_output=True, readonly=True) == command(rust, path, query, json_output=True, readonly=True)
        report['cases'].append({'name': name, 'setup': setup, 'transactions': sql, 'query': query, 'checkpoint_bytes': len(checkpoint), 'initial_log_bytes': len(before[1]), 'passed': True})
    for ordinal, step in enumerate(STEPS):
        path = directory/f'log-interruption-{ordinal}.duckdb'
        command(rust, path, "CREATE TABLE t(i INTEGER PRIMARY KEY,s VARCHAR); INSERT INTO t VALUES(1,'base')")
        environment = os.environ.copy()
        environment.update(DDB_LOG_CHILD_PATH=str(path), DDB_LOG_CHILD_STEP=str(ordinal))
        result = subprocess.run([str(worker), '--exact', 'logging_child', '--nocapture'], env=environment, capture_output=True, text=True)
        assert result.returncode == 86, result.stdout+result.stderr
        expected = [{'i':1, 's':'base'}]
        if ordinal >= 5: expected.insert(0, {'i':0, 's':'acknowledged in log'})
        if ordinal in [6,7,9]: expected.append({'i':2, 's':'pending'})
        compare_pair(rust, reference, command, path, 'SELECT * FROM t ORDER BY i', expected)
        report['boundaries'].append({'before': step, 'expected_rows': len(expected), 'passed': True})
    return report
