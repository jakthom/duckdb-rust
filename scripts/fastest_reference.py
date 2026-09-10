"""Apply the per-workload faster-C++ gate to two retained native campaigns."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path

from compare_native import compare
from reference_version import TARGETS
from upstream_suite import digest


def summarize(release, development, cases):
    reports = {'release': release, 'development': development}
    for name, report in reports.items():
        identity = report['reference_identity']
        if identity['target'] != name or report['cpp_revision'] != TARGETS[name].revision:
            raise ValueError('wrong reference identity')
        if report.get('error') or report['max_ratio'] != 1.0:
            raise ValueError('incomplete campaign or changed acceptance threshold')
    for key in ('rust_source_sha256', 'rust_binary_sha256', 'workloads_sha256',
                'cpp_worker_source_sha256', 'platform', 'machine', 'iterations',
                'warmups', 'compiler', 'rustc'):
        if release[key] != development[key]:
            raise ValueError('non-comparable campaigns: ' + key)
    expected = {case['name']: case for case in cases}
    if not expected or len(expected) != len(cases):
        raise ValueError('empty or duplicate workload identities')
    indexed = {}
    for name, report in reports.items():
        indexed[name] = {case['name']: case for case in report['workloads']}
        if len(indexed[name]) != len(report['workloads']) or indexed[name].keys() != expected.keys():
            raise ValueError('missing, extra or duplicate measured workloads')
    outcomes = []
    for name, case in expected.items():
        measured = {}
        for target in reports:
            entry = indexed[target][name]
            if any(entry.get(key) != value for key, value in case.items()):
                raise ValueError('changed workload semantics: ' + name)
            measured[target] = compare(entry['cpp'], entry['rust'], case)
        if indexed['release'][name]['workers'][1] != indexed['development'][name]['workers'][1]:
            raise ValueError('different Rust adapter configuration')
        selected = min(measured, key=lambda target: measured[target]['cpp_median_ns'])
        baseline = measured[selected]['cpp_median_ns']
        # Each paired campaign has its own Rust samples. Retain and gate BOTH;
        # never cherry-pick the lower Rust median after seeing the references.
        ratios = {target: result['rust_median_ns'] / baseline for target, result in measured.items()}
        outcomes.append({'name': name, 'selected_reference': selected,
                         'baseline_median_ns': baseline, 'campaign_medians': measured,
                         'rust_over_fastest': ratios, 'passed': all(ratio <= 1 for ratio in ratios.values())})
    return {'workloads': outcomes, 'passed': all(case['passed'] for case in outcomes),
            'max_ratio': 1.0, 'complete_performance_parity': False,
            'scope': 'Same workload/source/binary/host/configuration identities across two separately paired campaigns. '
                     'Each workload selects the smaller C++ median and gates both retained Rust medians against it. '
                     'All original samples and failed runs remain in the input reports. This is scoped latency evidence; '
                     'CPU, memory, I/O, concurrency and other unmeasured costs remain open.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', type=Path, required=True)
    parser.add_argument('--development', type=Path, required=True)
    parser.add_argument('--workloads', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('preserve earlier evidence; choose a new report')
    release, development = (json.loads(path.read_text()) for path in (args.release, args.development))
    if any(report['workloads_sha256'] != digest(args.workloads) for report in (release, development)):
        raise ValueError('workload manifest hash mismatch')
    result = summarize(release, development, json.loads(args.workloads.read_text())['workloads'])
    result.update(recorded_at=datetime.now(timezone.utc).isoformat(),
                  inputs={str(path): digest(path) for path in (args.release, args.development, args.workloads)},
                  script_sha256=digest(Path(__file__)))
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'passed': result['passed'], 'report': str(args.report),
                      'regressions': [case['name'] for case in result['workloads'] if not case['passed']]}))
    raise SystemExit(0 if result['passed'] else 1)


if __name__ == '__main__':
    main()
