"""Cross-engine conformance of live checkpoint scheduling and publication."""
from dataclasses import replace
import gzip
import hashlib
import os
from pathlib import Path
import subprocess

from logging_reference import CASES
from wal_reference import ROOT, REPLACE_STEPS, compare_pair, pair, worker_binary


def verify(rust, reference, command, directory):
    worker = worker_binary('checkpointing')
    source = hashlib.sha256()
    from source_identity import vendored_sources
    for path in sorted([*vendored_sources(ROOT), ROOT/'Cargo.toml', ROOT/'Cargo.lock', *(ROOT/'src').rglob('*.rs'), ROOT/'test/component/checkpointing.rs']):
        source.update(str(path.relative_to(ROOT)).encode()+b'\0'+path.read_bytes())
    report = {'worker_binary_sha256': hashlib.sha256(worker.read_bytes()).hexdigest(), 'worker_source_sha256': source.hexdigest(),
              'cases': [], 'boundaries': [], 'scope': 'Two checkpoint policies and explicit maintenance with independent native reads/writes. Exits before named I/O and after acknowledgment; no power-loss simulation or checkpoint-latency claim.'}
    for option, limit in [('--checkpoint-bytes', '256'), ('--checkpoint-commits', '2')]:
        logged = replace(rust, arguments=('--durability', 'wal', option, limit))
        for name, (setup, sql, query) in CASES.items():
            path = directory/f'online-{option}-{name}.duckdb'
            oracle = directory/f'online-{option}-{name}-oracle.duckdb'
            if setup is None:
                path.write_bytes(gzip.decompress((ROOT/'test/data/wal/mutations.duckdb.gz').read_bytes()))
            else:
                command(rust, path, setup)
            baseline = path.read_bytes(); oracle.write_bytes(baseline)
            command(reference, oracle, sql)
            key = command(reference, oracle, query, json_output=True, readonly=True)[0]['i']
            table = 'extra.t' if name == 'primitive_types' else 't'
            tail = f"; CHECKPOINT; UPDATE {table} SET s='after checkpoint' WHERE i={key}; CHECKPOINT; UPDATE {table} SET s='continued' WHERE i={key};"
            command(logged, path, sql+tail)
            command(reference, oracle, tail)
            expected = command(reference, oracle, query, json_output=True, readonly=True)
            before = pair(path)
            for engine in [rust, reference, rust]:
                assert command(engine, path, query, json_output=True, readonly=True) == expected, (option,name)
                assert pair(path) == before
            # Inspect the checkpoint alone: the final update belongs to the WAL.
            image = directory/f'online-{option}-{name}-image.duckdb'; image.write_bytes(before[0])
            assert command(reference, image, f'SELECT s FROM {table} WHERE i={key}', json_output=True, readonly=True) == [{'s':'after checkpoint'}]
            command(logged, path, 'CHECKPOINT')
            assert not Path(str(path)+'.wal').exists()
            command(reference, path, f"UPDATE {table} SET s='native resumed' WHERE i={key}; CHECKPOINT")
            command(logged, path, f"UPDATE {table} SET s='Rust resumed' WHERE i={key}; CHECKPOINT")
            assert command(reference, path, f'SELECT s FROM {table} WHERE i={key}', json_output=True, readonly=True) == [{'s':'Rust resumed'}]
            report['cases'].append({'name': name, 'policy': option, 'limit': int(limit), 'checkpoint_sha256': hashlib.sha256(baseline).hexdigest(), 'query': query, 'tail': tail, 'passed': True})
    for automatic in [False,True]:
        for ordinal, step in enumerate([*REPLACE_STEPS, 'Acknowledged']):
            path = directory/f'online-interruption-{automatic}-{ordinal}.duckdb'
            command(rust, path, "CREATE TABLE t(i INTEGER PRIMARY KEY,s VARCHAR); INSERT INTO t VALUES(1,'base')")
            environment = os.environ.copy()
            environment.update(DDB_CHECKPOINT_CHILD_PATH=str(path), DDB_CHECKPOINT_CHILD_STEP=str(ordinal), DDB_CHECKPOINT_CHILD_AUTO='1' if automatic else '0')
            result = subprocess.run([str(worker), '--exact', 'checkpoint_child', '--nocapture'], env=environment, capture_output=True, text=True)
            assert result.returncode == 86, result.stdout+result.stderr
            expected = [{'i':1,'s':'base'}, {'i':2,'s':'acknowledged'}]
            if automatic and step == 'Acknowledged': expected.append({'i':3,'s':'incoming'})
            compare_pair(rust, reference, command, path, 'SELECT * FROM t ORDER BY i', expected)
            report['boundaries'].append({'automatic': automatic, 'before': step, 'expected_rows': len(expected), 'passed': True})
    return report
