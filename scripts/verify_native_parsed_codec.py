"""Reverse-read retained Rust ParsedExpression exports with each producing pin.

This is a codec prerequisite campaign, not DEFAULT/catalog or SQL acceptance.
Raw wire, full source inventory and diagnostic display are retained. The oracle
compares inventories and display separately. The Rust IR and wire retain optional
diagnostic source spans, but this campaign does not yet compare their positions.
Exact scalar payload coverage belongs to the typed-Value campaign.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import tempfile

from native_nested_expression_reference import compile_helper, digest, oracle
from reference_version import ROOT


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    source_files = sorted((ROOT / 'src').rglob('*.rs'))
    before = {str(path.relative_to(ROOT)): digest(path) for path in source_files}
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    evidence = {'generated_at': datetime.now(timezone.utc).isoformat(),
                'engine_revision': revision, 'references': {}, 'cases': [],
                'purpose': 'Retained native expression codec; no DEFAULT acceptance'}
    with tempfile.TemporaryDirectory(prefix='native-parsed-', dir=ROOT / 'target') as directory:
        export = Path(directory) / 'rust.json'
        command = ['cargo', 'test', '--lib',
                   'storage::duckdb::parsed::tests::independent_cpp_parsed_expression_fixtures_remain_unevaluated']
        subprocess.run(command, cwd=ROOT, env={**os.environ, 'DUCKDB_NATIVE_PARSED_CODEC_EXPORT': str(export)},
                       check=True, timeout=180)
        cases = json.loads(export.read_text())
        helpers = {}
        for target in ('development', 'release'):
            helpers[target], evidence['references'][target] = compile_helper(target, directory)
        for case in cases:
            if 'rust_wire_hex' not in case:
                case['status'] = 'unsupported'
            else:
                result = oracle(helpers[case['producer']], 'decode', case['compatibility'], case['rust_wire_hex'])
                case['cpp_read_rust'] = result
                case['inventory_equal'] = result.get('inventory') == case['decoded'].get('inventory')
                case['display_equal'] = result.get('display') == case['decoded'].get('display')
                case['status'] = 'passed' if case['inventory_equal'] and case['display_equal'] else 'failed'
            evidence['cases'].append(case)
    evidence['source_before'] = before
    evidence['source_after'] = {str(path.relative_to(ROOT)): digest(path) for path in source_files}
    evidence['source_unchanged'] = before == evidence['source_after']
    evidence['summary'] = {status: sum(case['status'] == status for case in evidence['cases'])
                           for status in ('passed', 'failed', 'unsupported')}
    args.output.write_text(json.dumps(evidence, indent=2) + '\n')
    print(json.dumps(evidence['summary']))
    if evidence['summary']['failed'] or not evidence['source_unchanged']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
