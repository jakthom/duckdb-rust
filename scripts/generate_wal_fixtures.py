"""Generate native WAL fixtures by interrupting an independent DuckDB process."""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

from reference_version import TARGETS, require_reference

CASES = {
    'mutations': (
        """CREATE TABLE t(i INTEGER PRIMARY KEY, s VARCHAR, d DATE, n BIGINT, b BOOLEAN);
        INSERT INTO t SELECT i, 'old-' || i, DATE '2000-01-01' + i::INTEGER, i*100, true FROM range(10) r(i);
        DELETE FROM t WHERE i IN (1,7); CHECKPOINT;""",
        ["""BEGIN; INSERT INTO t VALUES (10,'ten',DATE 'infinity',1000,false),(11,NULL,NULL,NULL,NULL);
        UPDATE t SET s='new🦆' || chr(0), d=NULL, n=-100, b=NULL WHERE i=2;
        DELETE FROM t WHERE i=3; COMMIT;""",
         """BEGIN; UPDATE t SET d=DATE '0001-01-01 (BC)', b=false WHERE i=2;
        DELETE FROM t WHERE i=0; INSERT INTO t VALUES (0,'replacement',DATE '-infinity',-9223372036854775808,true);
        CREATE SCHEMA temporary_schema; CREATE TABLE temporary_schema.gone(i INTEGER);
        DROP TABLE temporary_schema.gone; DROP SCHEMA temporary_schema; COMMIT;"""],
        'SELECT i,s,d::VARCHAR d,n,b FROM t ORDER BY i'),
    'create': (
        'CHECKPOINT;',
        ["""CREATE SCHEMA extra; CREATE TABLE extra.t(id INTEGER PRIMARY KEY, a TINYINT DEFAULT -128,
        b SMALLINT, c INTEGER, d BIGINT, e HUGEINT, f FLOAT, g DOUBLE, s VARCHAR, dt DATE, ok BOOLEAN);
        INSERT INTO extra.t VALUES (1,-128,-32768,-2147483648,-9223372036854775808,
        '-170141183460469231731687303715884105728'::HUGEINT,'NaN'::FLOAT,'Infinity'::DOUBLE,
        'quack🦆' || chr(0),DATE '5877642-06-25 (BC)',true),
        (2,127,32767,2147483647,9223372036854775807,'170141183460469231731687303715884105727'::HUGEINT,
        '-Infinity'::FLOAT,'-0'::DOUBLE,'',DATE '5881580-07-10',false),(3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL);""",
         "INSERT INTO extra.t(id,b,dt) VALUES(4,42,DATE '2000-02-29');"],
        'SELECT id,a,b,c,d,e,f::DOUBLE f,g,s,dt::VARCHAR dt,ok FROM extra.t ORDER BY id'),
    'batches': (
        "CREATE TABLE t AS SELECT i::INTEGER i, 'row-' || i s FROM range(5013) r(i); CHECKPOINT;",
        ["""BEGIN; UPDATE t SET s=CASE WHEN i%2=0 THEN NULL ELSE 'changed-'||i END WHERE i%9=0;
        DELETE FROM t WHERE i>=2048 AND i<4096; COMMIT;""",
         "INSERT INTO t SELECT i, 'appended-'||i FROM range(5013,8026) r(i);"],
        'SELECT * FROM t ORDER BY i'),
    'version65': (
        "SET force_compression='uncompressed'; CREATE TABLE t(i INTEGER, s VARCHAR); INSERT INTO t VALUES(1,'base'); CHECKPOINT;",
        ["INSERT INTO t SELECT i, 'constant' FROM range(2,200) r(i);",
         "UPDATE t SET s=NULL WHERE i%7=0;"],
        'SELECT * FROM t ORDER BY i'),
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def reference(executable, path, query):
    # Row JSON escapes embedded NUL bytes correctly in the reference shell.
    sql = f'SELECT to_json(r)::VARCHAR AS row FROM ({query}) r'
    output = subprocess.check_output([executable, str(path), '-readonly', '-json', '-c', sql], text=True)
    return [json.loads(row['row'], parse_constant={'NaN': 'NaN', 'Infinity': 'inf', '-Infinity': '-inf'}.__getitem__) for row in json.loads(output)]


def send_ready(process, sql):
    marker = 'wal-fixture-acknowledged'
    process.stdin.write((sql + f"\nSELECT '{marker}';\n").encode())
    process.stdin.flush()
    deadline = time.monotonic() + 30
    output = b''
    while marker.encode() not in output:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError('reference did not acknowledge commit')
        ready = select.select([process.stdout, process.stderr], [], [], remaining)[0]
        for pipe in ready:
            data = os.read(pipe.fileno(), 65536)
            if not data:
                raise RuntimeError('reference terminated before acknowledgment')
            if pipe is process.stderr:
                raise RuntimeError(data.decode())
            output += data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', choices=TARGETS, default='release')
    parser.add_argument('--duckdb', type=Path)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    args.duckdb, identity = require_reference(args.duckdb, target=args.target)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    manifest = {'writer': identity['version'], 'reference_identity': identity, 'cases': {}}
    for name, (setup, transactions, query) in CASES.items():
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'fixture.duckdb'
            # Exercise requested v1.3 storage separately from the producer's
            # default. The selected binary, not this option, determines its WAL
            # header encoding; the manifest records the actual producer.
            process = subprocess.Popen([args.duckdb, ':memory:' if name == 'version65' else str(path), '-json'],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                if name == 'version65':
                    send_ready(process, f"ATTACH '{path}' AS fixture (STORAGE_VERSION 'v1.3.0'); USE fixture;")
                send_ready(process, setup)
                baseline = path.read_bytes()
                points = [0]
                for transaction in transactions:
                    send_ready(process, transaction)
                    points.append(Path(str(path) + '.wal').stat().st_size)
                # The process has acknowledged all prior statements. Kill it
                # without shutdown, then verify from a separate read-only open.
                process.kill()
                process.wait(timeout=10)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
                for pipe in [process.stdin, process.stdout, process.stderr]:
                    pipe.close()
            checkpoint = path.read_bytes()
            assert checkpoint == baseline, 'unexpected automatic checkpoint'
            log_path = Path(str(path) + '.wal')
            log = log_path.read_bytes()
            states = []
            for end in points:
                log_path.write_bytes(log[:end])
                # CREATE cases have no table until their first committed group.
                rows = reference(args.duckdb, path, query) if end or name != 'create' else None
                identity_query = 'SELECT rowid AS row_id,' + ('id FROM extra.t ORDER BY id' if name == 'create' else 'i FROM t ORDER BY i')
                identities = reference(args.duckdb, path, identity_query) if rows is not None else None
                states.append({'end': end, 'rows': rows, 'identities': identities})
                assert path.read_bytes() == checkpoint and log_path.read_bytes() == log[:end]
            log_path.write_bytes(log)
            for suffix, data in [('.duckdb', checkpoint), ('.wal', log)]:
                (args.output_dir / (name + suffix + '.gz')).write_bytes(gzip.compress(data, mtime=0))
            manifest['cases'][name] = {'setup': setup, 'transactions': transactions, 'query': query,
                'checkpoint_sha256': digest(checkpoint), 'wal_sha256': digest(log), 'states': states}
            print(f'{name}: {len(checkpoint)} checkpoint bytes, {len(log)} WAL bytes')
    (args.output_dir / 'manifest.json').write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + '\n')


if __name__ == '__main__':
    main()
