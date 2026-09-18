"""Independent C++ readers of manually framed production Rust VARIANT vectors.

This is codec evidence only: public WAL version/session gates stay closed.
"""
import argparse
from datetime import datetime, timezone
import gzip
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

from generate_wal_fixtures import digest, reference
from reference_version import require_reference
from session_reference import source_fingerprint

ROOT = Path(__file__).resolve().parents[1]
TEST = 'storage::duckdb::wal::nested::variant::tests::manual::manual_variant_wal_frames_roundtrip_without_opening_session_capabilities'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('Preserve previous evidence; choose a new report path')
    before = source_fingerprint()
    report = {'recorded_at':datetime.now(timezone.utc).isoformat(), 'scope':__doc__,
              'source_sha256':before, 'script_sha256':digest(Path(__file__).read_bytes()),
              'references':{}, 'cases':[], 'full_parity':False}
    executables = {}
    for target in ['development','release']:
        executable, identity = require_reference(target=target)
        executables[target] = executable
        report['references'][target] = identity
    with tempfile.TemporaryDirectory(prefix='variant-wal-codec-reference-') as directory:
        env = dict(os.environ, CARGO_BUILD_JOBS='2', DUCKDB_VARIANT_WAL_CODEC_EXPORT=directory)
        command = ['cargo','test','--lib',TEST,'--','--exact','--nocapture']
        completed = subprocess.run(command,cwd=ROOT,env=env,text=True,capture_output=True,timeout=180)
        report['manual_encoder'] = {'command':command,'returncode':completed.returncode,'stdout':completed.stdout,'stderr':completed.stderr}
        binary = re.search(r'Running unittests .* \((target/[^)]+)\)', completed.stderr)
        if binary:
            report['manual_encoder']['binary_sha256'] = digest((ROOT/binary.group(1)).read_bytes())
        if completed.returncode == 0:
            for producer, name in [('release','variant_v1_5_0'),('development','variant_v1_5_0'),('development','variant_v2_0_0')]:
                fixture = json.loads((ROOT/f'test/data/wal-variant-{producer}/manifest.json').read_text())['cases'][name]
                original = Path(directory)/f'original-{producer}-{name}.duckdb'
                original_wal = Path(str(original)+'.wal')
                original.write_bytes(gzip.decompress((ROOT/f'test/data/wal-variant-{producer}/{name}.duckdb.gz').read_bytes()))
                original_wal.write_bytes(gzip.decompress((ROOT/f'test/data/wal-variant-{producer}/{name}.wal.gz').read_bytes()))
                expected = reference(executables['development'],original,fixture['query'],plain=True)
                assert digest(original.read_bytes()) == fixture['checkpoint_sha256']
                assert digest(original_wal.read_bytes()) == fixture['wal_sha256']
                path = Path(directory)/f'{producer}-{name}.duckdb'
                wal = Path(str(path)+'.wal')
                initial = (digest(path.read_bytes()),digest(wal.read_bytes()))
                for reader in (['development'] if name.endswith('v2_0_0') else ['development','release']):
                    case = {'fixture_producer':producer,'name':name,'reader':reader,'query':fixture['query'],
                            'checkpoint_sha256':initial[0],'wal_sha256':initial[1],
                            'oracle':'development reading independent original fixture',
                            'producer_oracle_agrees':expected == fixture['states'][-1]['rows'],'passed':False}
                    report['cases'].append(case)
                    try:
                        rows = reference(executables[reader],path,fixture['query'],plain=True)
                        if rows != expected: raise AssertionError({'expected':expected,'actual':rows})
                        if initial != (digest(path.read_bytes()),digest(wal.read_bytes())):
                            raise AssertionError('read-only C++ recovery changed file pair')
                        case.update(passed=True,rows=len(rows))
                    except Exception as error:
                        case['error'] = str(error)
        report['source_unchanged'] = before == source_fingerprint()
        report['passed'] = completed.returncode == 0 and len(report['cases']) == 5 and report['source_unchanged'] and all(case['passed'] for case in report['cases'])
    args.report.write_text(json.dumps(report,indent=2,ensure_ascii=False)+'\n')
    print(json.dumps(report,indent=2,ensure_ascii=False))
    raise SystemExit(0 if report['passed'] else 1)


if __name__ == '__main__':
    main()
