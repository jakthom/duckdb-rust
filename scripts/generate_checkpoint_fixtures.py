"""Capture native checkpoints interrupted before header publication or WAL retirement."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from generate_wal_fixtures import reference, send_ready

ROOT = Path(__file__).resolve().parents[1]
SETUP = """SET force_compression='uncompressed';
CREATE TABLE t(i INTEGER PRIMARY KEY, s VARCHAR);
INSERT INTO t VALUES(1,'old'),(2,'two'); CHECKPOINT;"""
TRANSACTIONS = "UPDATE t SET s='new' WHERE i=1; INSERT INTO t VALUES(3,'three');"
QUERY = 'SELECT * FROM t ORDER BY i'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--duckdb', default='/opt/homebrew/bin/duckdb')
    args = parser.parse_args()
    destination = ROOT/'test/data/wal/checkpoints'
    destination.mkdir(parents=True, exist_ok=True)
    manifest = {'writer': subprocess.check_output([args.duckdb, '--version'], text=True).strip(), 'cases': {}}
    for abort in ['BEFORE_HEADER', 'BEFORE_TRUNCATE']:
        name = abort.lower()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'fixture.duckdb'
            process = subprocess.Popen([args.duckdb, str(path), '-json'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                send_ready(process, SETUP)
                send_ready(process, TRANSACTIONS)
                try:
                    send_ready(process, f"SET debug_checkpoint_abort='{abort}'; CHECKPOINT;")
                except RuntimeError as error:
                    assert 'Checkpoint aborted before ' in str(error), str(error)
                else:
                    raise AssertionError('reference checkpoint did not abort')
            finally:
                # Keep stdin open until killed: normal shutdown can retry the
                # failed checkpoint and append another checkpoint marker.
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=10)
                for pipe in [process.stdin, process.stdout, process.stderr]:
                    pipe.close()
            checkpoint = path.read_bytes()
            log_path = Path(str(path)+'.wal')
            log = log_path.read_bytes()
            rows = reference(args.duckdb, path, QUERY)
            assert rows == [{'i':1,'s':'new'}, {'i':2,'s':'two'}, {'i':3,'s':'three'}]
            assert path.read_bytes() == checkpoint and log_path.read_bytes() == log
            for suffix, data in [('duckdb', checkpoint), ('wal', log)]:
                (destination/f'{name}.{suffix}.gz').write_bytes(gzip.compress(data, mtime=0))
            manifest['cases'][name] = {
                'setup': SETUP, 'transactions': TRANSACTIONS, 'abort': abort,
                'query': QUERY, 'rows': rows,
                'checkpoint_published': abort == 'BEFORE_TRUNCATE',
                'checkpoint_sha256': hashlib.sha256(checkpoint).hexdigest(),
                'wal_sha256': hashlib.sha256(log).hexdigest(),
            }
            print(f'{name}: {len(checkpoint)} checkpoint bytes, {len(log)} WAL bytes')
    (destination/'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


if __name__ == '__main__':
    main()
