"""Check SQL corpora with persistent Rust/C++ sessions and unchanged assertions."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import selectors
import select
import subprocess
import tempfile
import time

import sqllogic
from reference_version import TARGETS, require_checkout, require_reference
from run_upstream import RustEngine
from upstream_suite import ROOT, digest


def encode_request(request):
    """Byte lengths preserve Unicode, newlines and embedded delimiters."""
    if request['operation'] not in ('query', 'statement', 'load', 'reconnect'):
        raise ValueError('unsupported reference transport operation')
    if request.get('path') or request.get('read_only'):
        raise ValueError('reference transport supports in-memory sessions only')
    fields = [request['operation'], request.get('connection', ''), request.get('sql', '')]
    encoded = [field.encode('utf-8') for field in fields]
    if any(len(field) > 16 * 1024 * 1024 for field in encoded):
        raise ValueError('reference request field exceeds transport limit')
    return b''.join(str(len(field)).encode() + b'\n' + field for field in encoded)


class CppEngine:
    def __init__(self, binary, directory, deadline):
        self.errors = tempfile.TemporaryFile()
        self.process = subprocess.Popen([str(binary)], cwd=directory, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.errors)
        self.events = selectors.DefaultSelector()
        self.events.register(self.process.stdout, selectors.EVENT_READ)
        self.deadline = deadline
        self.buffer = bytearray()
        os.set_blocking(self.process.stdin.fileno(), False)
        try:
            self.identity = self.read()
            if not self.identity.get('ready'):
                raise ValueError('C++ reference worker did not initialize')
        except BaseException:
            self.close()
            raise

    def read(self):
        while True:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('C++ reference deadline exceeded')
            if b'\n' in self.buffer:
                line, _, self.buffer = self.buffer.partition(b'\n')
                return json.loads(line)
            if not self.events.select(timeout=remaining):
                raise TimeoutError('C++ reference deadline exceeded')
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                self.errors.seek(0)
                raise RuntimeError('C++ reference exited: ' + self.errors.read(2000).decode(errors='replace'))
            self.buffer.extend(chunk)

    def request(self, request):
        pending = memoryview(encode_request(request))
        while pending:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0 or not select.select([], [self.process.stdin], [], remaining)[1]:
                raise TimeoutError('C++ reference request deadline exceeded')
            try:
                count = os.write(self.process.stdin.fileno(), pending[:65536])
                pending = pending[count:]
            except BlockingIOError:
                continue
        return self.read()

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        if not self.process.stdin.closed:
            self.process.stdin.close()
        self.process.stdout.close()
        self.events.close()
        self.errors.close()


def verify_records(engine, records):
    """Keep each failure and continue, without promoting partial files to passes."""
    runner = sqllogic.Runner(engine)
    outcomes = []
    for ordinal, record in enumerate(records, 1):
        outcome = {'ordinal': ordinal, 'line': record.line, 'directive': record.words,
                   'sql': record.sql, 'passed': False}
        outcomes.append(outcome)
        try:
            before = runner.passed
            runner.run([record])
            outcome['passed'] = runner.passed == before + 1 and runner.skipped == 0
        except Exception as error:
            outcome['error'] = {'type': type(error).__name__, 'message': str(error)}
    return outcomes


def source_fingerprint():
    from source_identity import vendored_sources
    digest = hashlib.sha256()
    for path in sorted([*vendored_sources(ROOT), ROOT/'Cargo.toml', ROOT/'Cargo.lock', *(ROOT/'src').rglob('*.rs'),
                        ROOT/'test/runner/worker.rs']):
        digest.update(str(path.relative_to(ROOT)).encode()+b'\0'+path.read_bytes())
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust', type=Path,
                        help='Use an existing worker; its source identity is unverified. The default builds the current release worker.')
    parser.add_argument('--corpus', type=Path, action='append', required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('preserve earlier evidence; choose a new report')
    before_build = source_fingerprint()
    build_command = None
    if args.rust is None:
        build_command = ['cargo', 'build', '--offline', '--release', '--bin', 'duckdb-rust-test-worker']
        subprocess.run(build_command, cwd=ROOT, check=True)
    rust = (args.rust or ROOT/'target/release/duckdb-rust-test-worker').resolve(strict=True)
    if source_fingerprint() != before_build:
        raise RuntimeError('Rust source changed while preparing the reference campaign')
    corpora = [(path.resolve(strict=True), sqllogic.parse(path.read_text())) for path in args.corpus]
    if any(not records or any(r.words[0] not in ('query', 'statement') for r in records) for _, records in corpora):
        raise ValueError('this campaign requires nonempty corpora of SQL records')
    source = ROOT/'test/runner/reference.cpp'
    report = {'recorded_at': datetime.now(timezone.utc).isoformat(), 'rust_binary_sha256': digest(rust),
              'rust_build_command': build_command, 'rust_source_verified': build_command is not None,
              'working_tree_source_sha256': before_build,
              'reference_worker_source_sha256': digest(source),
              'harness_sha256': {name: digest(ROOT/'scripts'/name) for name in
                                ('session_reference.py', 'sqllogic.py', 'run_upstream.py', 'reference_version.py')},
              'targets': [], 'passed': False,
              'complete_test_parity': False, 'complete_compatibility': False,
              'scope': 'Persistent in-memory sessions with unchanged local SQL assertions. Named connections share a database. Each corpus starts with a fresh database. Errors, unsupported cases and partial runs remain failures. No performance or full diagnostic/native-file parity claim.'}
    for target, selected in TARGETS.items():
        trial = {'target': target, 'corpora': [], 'passed': False}
        report['targets'].append(trial)
        try:
            revision = require_checkout(selected.source, target)
            _, trial['reference_identity'] = require_reference(target=target)
            library = selected.build/'src'/('libduckdb.dylib' if platform.system() == 'Darwin' else 'libduckdb.so')
            binary = ROOT/f'target/session-reference-{target}'
            command = ['c++', '-std=c++17', '-O3', '-DNDEBUG', '-I'+str(selected.source/'src/include'),
                       str(source), str(library), '-Wl,-rpath,'+str(library.parent), '-o', str(binary)]
            subprocess.run(command, check=True)
            trial.update(compile_command=command, cpp_library_sha256=digest(library), cpp_worker_sha256=digest(binary))
            for corpus, records in corpora:
                result = {'path': str(corpus), 'sha256': digest(corpus), 'engines': {}, 'passed': False}
                trial['corpora'].append(result)
                with tempfile.TemporaryDirectory(prefix='ddb-session-reference-') as directory:
                    for name, factory, executable in [('rust', RustEngine, rust), ('cpp', CppEngine, binary)]:
                        engine = factory(executable, directory, time.monotonic()+60)
                        try:
                            if name == 'rust':
                                description = engine.request({'operation': 'describe'})
                                if not description.get('ok') or not description.get('adapters'):
                                    raise ValueError('Rust worker did not report its adapter composition')
                                result['rust_adapters'] = description['adapters']
                            else:
                                result['worker_identity'] = engine.identity
                                source_id = engine.identity['source_id']
                                if len(source_id) < 10 or not revision.startswith(source_id):
                                    raise ValueError('loaded C++ library has the wrong revision')
                            result['engines'][name] = verify_records(engine, records)
                        finally:
                            engine.close()
                result['passed'] = all(len(rows) == len(records) and all(r['passed'] for r in rows)
                                       for rows in result['engines'].values()) and len(result['engines']) == 2
            trial['passed'] = all(c['passed'] for c in trial['corpora'])
        except Exception as error:
            trial['error'] = {'type': type(error).__name__, 'message': str(error)}
    report['passed'] = all(t['passed'] for t in report['targets'])
    report['source_unchanged'] = source_fingerprint() == before_build
    report['rust_binary_unchanged'] = digest(rust) == report['rust_binary_sha256']
    if not report['source_unchanged'] or not report['rust_binary_unchanged']:
        report['passed'] = False
        report['error'] = 'Rust source or worker binary changed during the campaign'
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps({'passed': report['passed'], 'report': str(args.report)}))
    raise SystemExit(0 if report['passed'] else 1)


if __name__ == '__main__':
    main()
