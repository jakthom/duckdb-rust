"""Independent checks of interrupted Rust recovery against native DuckDB."""
import gzip
import hashlib
import json
import os
from pathlib import Path
import subprocess

from generate_wal_fixtures import send_ready

ROOT = Path(__file__).resolve().parents[1]
REPLACE_STEPS = [
    'CheckpointCreate', 'CheckpointWrite', 'CheckpointSync', 'RecoveryLogCreate',
    'RecoveryLogWrite', 'RecoveryLogSync', 'RecoveryLogRename',
    'RecoveryLogDirectorySync', 'CheckpointRename', 'CheckpointDirectorySync',
    'LogRemove', 'LogRetirementDirectorySync',
]
RETIRE_STEPS = ['CurrentCheckpointSync', 'CheckpointDirectorySync', 'LogRemove', 'LogRetirementDirectorySync']


def worker_binary(target='recovery'):
    output = subprocess.check_output(['cargo', 'test', '--offline', '--test', target, '--no-run', '--message-format=json'], cwd=ROOT, text=True)
    for line in output.splitlines():
        record = json.loads(line)
        if record.get('reason') == 'compiler-artifact' and record['target']['name'] == target and record.get('executable'):
            return Path(record['executable'])
    raise RuntimeError(f'Cargo did not report the {target} test executable')


def interrupt(worker, path, ordinal, retire=False):
    environment = os.environ.copy()
    environment.update(DDB_RECOVERY_CHILD_PATH=str(path), DDB_RECOVERY_CHILD_STEP=str(ordinal))
    environment.pop('DDB_RECOVERY_CHILD_RETIRE', None)
    if retire:
        environment['DDB_RECOVERY_CHILD_RETIRE'] = '1'
    result = subprocess.run([str(worker), '--exact', 'publication::publication_child', '--nocapture'], env=environment, capture_output=True, text=True)
    assert result.returncode == 86, result.stdout + result.stderr


def pair(path):
    wal = Path(str(path) + '.wal')
    return path.read_bytes(), wal.read_bytes() if wal.exists() else None


def compare_pair(rust, reference, command, path, query, expected):
    before = pair(path)
    for engine in [rust, reference, rust]:
        assert command(engine, path, query, json_output=True, readonly=True) == expected
        assert pair(path) == before, 'read-only open changed recovery state'
    command(rust, path, 'SELECT 1')  # complete writable recovery
    assert not Path(str(path) + '.wal').exists()
    assert command(reference, path, query, json_output=True, readonly=True) == expected


def checkpoint_root(path):
    data = path.read_bytes()
    headers = [data[4096:8192], data[8192:12288]]
    header = max(headers, key=lambda h: int.from_bytes(h[8:16], 'little'))
    return int.from_bytes(header[16:24], 'little')


def verify(rust, reference, command, directory):
    worker = worker_binary()
    sources = hashlib.sha256()
    from source_identity import vendored_sources
    for path in sorted([*vendored_sources(ROOT), ROOT/'Cargo.toml', ROOT/'Cargo.lock', *(ROOT/'src').rglob('*.rs'), ROOT/'test/component/recovery.rs', *(ROOT/'test/component/recovery').rglob('*.rs')]):
        sources.update(str(path.relative_to(ROOT)).encode() + b'\0' + path.read_bytes())
    report = {'worker_binary_sha256': hashlib.sha256(worker.read_bytes()).hexdigest(),
              'worker_source_sha256': sources.hexdigest(), 'boundaries': [],
              'scope': 'Process exits before named real I/O operations; no Rust destructors run. Includes retry and independent DuckDB reads. Does not simulate power loss or arbitrary filesystem behavior.'}
    fixtures = ROOT/'test/data/wal'
    case = json.loads((fixtures/'manifest.json').read_text())['cases']['mutations']
    checkpoint = gzip.decompress((fixtures/'mutations.duckdb.gz').read_bytes())
    log = gzip.decompress((fixtures/'mutations.wal.gz').read_bytes())
    for retire, steps in [(False,REPLACE_STEPS), (True,RETIRE_STEPS)]:
        for ordinal, step in enumerate(steps):
            path = directory/f'publication-{retire}-{ordinal}.duckdb'
            path.write_bytes(checkpoint); Path(str(path)+'.wal').write_bytes(log)
            if retire:
                # Stop immediately after checkpoint rename. The next recovery
                # must retire its log without replaying already checkpointed data.
                interrupt(worker, path, REPLACE_STEPS.index('CheckpointDirectorySync'))
            interrupt(worker, path, ordinal, retire)
            compare_pair(rust, reference, command, path, case['query'], case['states'][-1]['rows'])
            report['boundaries'].append({'protocol': 'retire' if retire else 'replace', 'before': step, 'passed': True})

    # A Rust-origin checkpoint with a same-shaped update would reuse the same
    # catalog root under ordinary encoding. A successor must relocate that root.
    path = directory/'root-collision.duckdb'
    command(rust, path, "CREATE TABLE t(i INTEGER,s VARCHAR); INSERT INTO t VALUES(1,'old'),(2,'two')")
    old_root = checkpoint_root(path)
    process = subprocess.Popen([str(reference.binary), str(path), '-json'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        send_ready(process, "UPDATE t SET s='new' WHERE i=1;")
        process.kill(); process.wait(timeout=10)
    finally:
        if process.poll() is None:
            process.kill(); process.wait(timeout=10)
        for pipe in [process.stdin, process.stdout, process.stderr]: pipe.close()
    assert checkpoint_root(path) == old_root
    interrupt(worker, path, REPLACE_STEPS.index('CheckpointDirectorySync'))
    assert checkpoint_root(path) != old_root, 'successor reused the old catalog root'
    compare_pair(rust, reference, command, path, 'SELECT * FROM t ORDER BY i', [{'i':1,'s':'new'},{'i':2,'s':'two'}])
    report['root_collision'] = 'passed'
    report['native_checkpoints'] = []
    checkpoint_fixtures = fixtures/'checkpoints'
    for name, case in json.loads((checkpoint_fixtures/'manifest.json').read_text())['cases'].items():
        path = directory/f'native-checkpoint-{name}.duckdb'
        checkpoint = gzip.decompress((checkpoint_fixtures/f'{name}.duckdb.gz').read_bytes())
        log = gzip.decompress((checkpoint_fixtures/f'{name}.wal.gz').read_bytes())
        assert hashlib.sha256(checkpoint).hexdigest() == case['checkpoint_sha256']
        assert hashlib.sha256(log).hexdigest() == case['wal_sha256']
        path.write_bytes(checkpoint); Path(str(path)+'.wal').write_bytes(log)
        compare_pair(rust, reference, command, path, case['query'], case['rows'])
        report['native_checkpoints'].append({'abort': case['abort'], 'checkpoint_published': case['checkpoint_published'], 'passed': True})
    return report
