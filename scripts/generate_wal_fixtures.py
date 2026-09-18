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
    'nested_paths': (
        """CREATE TABLE t(i INTEGER,s STRUCT(a INTEGER,b STRUCT(x VARCHAR,d DECIMAL(12,2))));
        INSERT INTO t VALUES(0,{'a':1,'b':{'x':'old','d':1.25}}),(1,NULL),(2,{'a':NULL,'b':NULL}); CHECKPOINT;""",
        ["UPDATE t SET s={'a':7,'b':{'x':'changed','d':2.50}} WHERE i=0;",
         "BEGIN; UPDATE t SET s={'a':8,'b':{'x':'materialized','d':3.75}} WHERE i=1; UPDATE t SET s=NULL WHERE i=0; COMMIT;",
         "UPDATE t SET s={'a':NULL,'b':{'x':'nested','d':NULL}} WHERE i=2;",
         "UPDATE t SET s={'a':7,'b':{'x':'changed','d':2.50}} WHERE i=0;",
         "BEGIN; UPDATE t SET s=NULL WHERE i=1; UPDATE t SET s={'a':8,'b':{'x':'materialized','d':3.75}} WHERE i=1; COMMIT;"],
        'SELECT i,s::VARCHAR s FROM t ORDER BY i'),
    'nested': (
        """CREATE TABLE t(i INTEGER PRIMARY KEY,s STRUCT(n DECIMAL(12,2),z TIMESTAMP_NS,b BIT),l STRUCT(x INTEGER)[],a INTEGER[2],m MAP(VARCHAR,INTEGER[]),u UNION(n INTEGER,s VARCHAR)); CHECKPOINT;""",
        ["""INSERT INTO t VALUES(0,{'n':1.25,'z':TIMESTAMP_NS '2000-01-01 00:00:00.123456789','b':'101'::BIT},[{'x':1},NULL],[1,NULL],map(['x','y'],[[1,NULL],[]]),union_value(n:=NULL)),(1,{'n':NULL,'z':NULL,'b':NULL},[],[NULL,2],map([],[]),union_value(s:='a')),(2,NULL,NULL,NULL,NULL,NULL);""",
         """BEGIN; UPDATE t SET s={'n':2.50,'z':TIMESTAMP_NS '2001-01-01 00:00:00.000000001','b':'0'::BIT},u=union_value(s:='changed') WHERE i=0; DELETE FROM t WHERE i=2; COMMIT;"""],
        'SELECT i,s::VARCHAR s,l::VARCHAR l,a::VARCHAR a,m::VARCHAR m,u::VARCHAR u FROM t ORDER BY i'),
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
CASES['nested_v2'] = CASES['nested']


def digest(data):
    return hashlib.sha256(data).hexdigest()


def reference(executable, path, query, plain=False):
    # Row JSON escapes embedded NUL bytes correctly in the reference shell.
    # Nested fixtures explicitly cast values to VARCHAR and contain no embedded
    # NUL text, so they do not require the optional JSON extension to quote rows.
    sql = query if plain else f'SELECT to_json(r)::VARCHAR AS row FROM ({query}) r'
    output = subprocess.check_output([executable, str(path), '-readonly', '-json', '-c', sql], text=True)
    rows = json.loads(output) if output.strip() else []
    return rows if plain else [json.loads(row['row'], parse_constant={'NaN': 'NaN', 'Infinity': 'inf', '-Infinity': '-inf'}.__getitem__) for row in rows]


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
    parser.add_argument('--case', action='append', choices=CASES, help='Generate selected cases only; repeat to select several')
    args = parser.parse_args()
    if args.target != 'development' and args.case and 'nested_v2' in args.case:
        parser.error('nested_v2 requires the pinned development producer')
    args.duckdb, identity = require_reference(args.duckdb, target=args.target)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = args.output_dir / 'manifest.json'
    manifest = json.loads(manifest_path.read_text()) if args.case and manifest_path.exists() else {'writer': identity['version'], 'reference_identity': identity, 'cases': {}}
    if manifest['reference_identity']['sha256'] != identity['sha256']:
        raise ValueError('Refusing to mix fixture producer identities')
    for name in args.case or CASES:
        if name == 'nested_v2' and args.target != 'development':
            continue
        setup, transactions, query = CASES[name]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'fixture.duckdb'
            # Exercise requested v1.3 storage separately from the producer's
            # default. The selected binary, not this option, determines its WAL
            # header encoding; the manifest records the actual producer.
            process = subprocess.Popen([args.duckdb, ':memory:' if name in ['version65','nested_v2'] else str(path), '-json'],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                if name == 'version65':
                    send_ready(process, f"ATTACH '{path}' AS fixture (STORAGE_VERSION 'v1.3.0'); USE fixture;")
                if name == 'nested_v2':
                    send_ready(process, f"ATTACH '{path}' AS fixture (STORAGE_VERSION 'v2.0.0'); USE fixture;")
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
                rows = reference(args.duckdb, path, query, plain=name.startswith('nested')) if end or name != 'create' else None
                identity_query = 'SELECT rowid AS row_id,' + ('id FROM extra.t ORDER BY id' if name == 'create' else 'i FROM t ORDER BY i')
                identities = reference(args.duckdb, path, identity_query, plain=name.startswith('nested')) if rows is not None else None
                states.append({'end': end, 'rows': rows, 'identities': identities})
                assert path.read_bytes() == checkpoint and log_path.read_bytes() == log[:end]
            log_path.write_bytes(log)
            for suffix, data in [('.duckdb', checkpoint), ('.wal', log)]:
                (args.output_dir / (name + suffix + '.gz')).write_bytes(gzip.compress(data, mtime=0))
            manifest['cases'][name] = {'setup': setup, 'transactions': transactions, 'query': query,
                'checkpoint_sha256': digest(checkpoint), 'wal_sha256': digest(log), 'states': states}
            if name == 'nested_v2':
                manifest['cases'][name]['storage_version'] = 'v2.0.0'
            print(f'{name}: {len(checkpoint)} checkpoint bytes, {len(log)} WAL bytes')
    (args.output_dir / 'manifest.json').write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + '\n')


if __name__ == '__main__':
    main()
