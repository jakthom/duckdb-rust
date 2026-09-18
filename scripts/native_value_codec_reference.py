"""Exact C++ rereading of raw typed Value metadata emitted by the Rust codec.

This does not claim parsed DEFAULT expression or catalog integration. Both
readers deserialize each independently produced original and Rust round trip,
then compare exact typed/tagged content, not SQL equality or displays. VARIANT
descriptor allocation order and unused bytes are excluded, but tags, widths,
raw scalar bits, ordered exact-name object keys and nested NULLs are retained.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import tempfile

from generate_native_value_fixtures import ROOT, compile_helper, digest, oracle
from session_reference import source_fingerprint

TEST = 'storage::duckdb::value::tests::fixtures::independent_cpp_typed_value_metadata_fixtures_and_rust_exports'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('Preserve previous evidence; use a new report path')
    before = source_fingerprint()
    fixture = ROOT / 'test/data/native-value-metadata.json'
    report = {'recorded_at': datetime.now(timezone.utc).isoformat(), 'scope': __doc__,
              'source_sha256': before, 'fixture_sha256': digest(fixture), 'script_sha256': digest(Path(__file__)),
              'references': {}, 'cases': [], 'full_parity': False}
    with tempfile.TemporaryDirectory(prefix='native-value-readers-') as directory:
        exports = Path(directory) / 'rust.json'
        env = dict(os.environ, CARGO_BUILD_JOBS='2', DUCKDB_NATIVE_VALUE_CODEC_EXPORT=str(exports))
        command = ['cargo', 'test', '--lib', TEST, '--', '--exact', '--nocapture']
        result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True, timeout=180)
        report['rust_codec'] = {'command': command, 'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}
        if result.returncode == 0:
            helpers = {}
            for target in ('development', 'release'):
                helpers[target], report['references'][target] = compile_helper(target, directory)
            for source in json.loads(exports.read_text()):
                for target in (('development',) if source['version'] == 69 else ('development', 'release')):
                    case = {key: source[key] for key in ('producer', 'version', 'compatibility', 'name')}
                    case.update(reader=target, passed=False)
                    report['cases'].append(case)
                    try:
                        original = oracle(helpers[target], 'decode', source['compatibility'], source['wire_hex'])
                        reread = oracle(helpers[target], 'decode', source['compatibility'], source['rust_wire_hex'])
                        if original['type'] != reread['type'] or original['content_hex'] != reread['content_hex']:
                            raise AssertionError({'original': original, 'rust_reread': reread})
                        case.update(passed=True, type=reread['type'], original_bytes=len(source['wire_hex']) // 2,
                                    rust_bytes=len(source['rust_wire_hex']) // 2,
                                    raw_bytes_equal=source['wire_hex'] == source['rust_wire_hex'],
                                    exact_content_hex=reread['content_hex'])
                    except Exception as error:
                        case['error'] = str(error)
        report['source_unchanged'] = before == source_fingerprint()
        report['passed'] = result.returncode == 0 and len(report['cases']) == 157 and report['source_unchanged'] and all(case['passed'] for case in report['cases'])
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps({'passed': report['passed'], 'cases': len(report['cases']), 'failures': [case for case in report['cases'] if not case['passed']]}, indent=2))
    raise SystemExit(0 if report['passed'] else 1)


if __name__ == '__main__':
    main()
