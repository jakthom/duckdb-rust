"""Diagnose one upstream deadline against two Rust sources; not a parity gate."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess

from reference_version import TARGETS, require_checkout
from run_upstream import run_case
from source_identity import vendored_sources
from upstream_suite import DESTINATION, ROOT, digest


def source_identity(root):
    source = hashlib.sha256()
    for path in sorted([*vendored_sources(root), root / 'Cargo.toml', root / 'Cargo.lock',
                        *(root / 'src').rglob('*.rs'), root / 'test/runner/worker.rs']):
        source.update(str(path.relative_to(root)).encode() + b'\0' + path.read_bytes())
    return source.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--previous-worktree', type=Path, required=True)
    parser.add_argument('--previous-commit', required=True)
    parser.add_argument('--path', required=True)
    parser.add_argument('--timeout', type=float, default=3)
    parser.add_argument('--pairs', type=int, default=3)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists() or args.timeout <= 0 or args.pairs < 1:
        raise ValueError('new report, positive timeout and nonempty paired run required')
    previous = args.previous_worktree.resolve(strict=True)
    commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=previous, text=True).strip()
    expected = subprocess.check_output(['git', 'rev-parse', args.previous_commit], cwd=ROOT, text=True).strip()
    if commit != expected or subprocess.check_output(
            ['git', 'status', '--porcelain', '--untracked-files=no'], cwd=previous, text=True).strip():
        raise ValueError('previous source must be clean at the requested commit')
    source = TARGETS['development'].source
    require_checkout(source, 'development')
    manifest = json.loads((DESTINATION / 'manifest.json').read_text())
    entries = [entry for entry in manifest['tests']
               if entry['kind'] == 'sqllogictest' and entry['path'] == args.path]
    assets = [entry for entry in manifest['files'] if entry['path'] == args.path]
    if len(entries) != 1 or len(assets) != 1 or digest(source / args.path) != assets[0]['sha256']:
        raise ValueError('SQL source must match exactly one retained upstream asset')
    command = ['cargo', 'build', '--offline', '--release', '--no-default-features',
               '--bin', 'duckdb-rust-test-worker']
    roots = {'previous': previous, 'current': ROOT}
    identities, binaries = {}, {}
    for name, root in roots.items():
        before = source_identity(root)
        subprocess.run(command, cwd=root, check=True)
        if before != source_identity(root):
            raise RuntimeError('source changed during build')
        binary = root / 'target/release/duckdb-rust-test-worker'
        binaries[name] = binary
        identities[name] = {'commit': subprocess.check_output(
            ['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
            'source_sha256': before, 'binary_sha256': digest(binary), 'build': command}
    report = {'recorded_at': datetime.now(timezone.utc).isoformat(),
              'scope': 'Paired Rust deadline diagnosis using unchanged upstream assertions. '
                       'Neither a C++ performance comparison nor a full test-parity gate. '
                       'Every outcome and prefix is retained; no best-run selection.',
              'identities': identities, 'path': args.path, 'sql_sha256': assets[0]['sha256'],
              'timeout_seconds': args.timeout, 'pairs': args.pairs,
              'harness_sha256': {name: digest(ROOT / 'scripts' / name) for name in
                                  ['upstream_regression.py', 'run_upstream.py', 'sqllogic.py']},
              'runs': [], 'source_unchanged': False}
    try:
        for pair in range(args.pairs):
            for name in (['previous', 'current'] if pair % 2 == 0 else ['current', 'previous']):
                outcome = run_case(binaries[name], source, entries[0], args.timeout)
                report['runs'].append({'pair': pair + 1, 'engine': name, **outcome})
                print(json.dumps(report['runs'][-1]), flush=True)
        report['source_unchanged'] = all(
            source_identity(root) == identities[name]['source_sha256']
            and digest(binaries[name]) == identities[name]['binary_sha256']
            for name, root in roots.items())
    finally:
        with args.report.open('x') as output:
            json.dump(report, output, indent=2)
            output.write('\n')
    if not report['source_unchanged']:
        raise RuntimeError('source or binary changed during diagnosis')


if __name__ == '__main__':
    main()
