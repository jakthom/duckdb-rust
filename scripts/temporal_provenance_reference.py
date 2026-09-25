"""Exact retained period-specifier execution-provenance evidence for both pins."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import platform
import subprocess
import tempfile
import time

from reference_version import TARGETS, require_checkout, require_reference
from run_upstream import RustEngine
from session_reference import CppEngine, source_fingerprint
from upstream_suite import ROOT, digest


SQL = []
for function in ('date_diff', 'datediff', 'date_sub', 'datesub'):
    for source in (
        "(SELECT 'bad' p)",
        "(VALUES ('bad')) t(p)",
        "(VALUES ('bad'),('bad')) t(p)",
        "(SELECT 'bad' p FROM range(3))",
        "(SELECT 'bad' p FROM range(3) t(i) WHERE i<>1)",
        "(SELECT 'bad' p FROM range(3) ORDER BY 1)",
        "(SELECT 'bad' p FROM range(3) LIMIT 1)",
        "(SELECT 'bad' p,concat('x','y') x FROM range(3))",
        "(SELECT p FROM (VALUES ('bad'),('bad')) t(p) WHERE p='bad')",
    ):
        SQL.append(f"SELECT {function}(p,DATE 'infinity',DATE 'epoch') FROM {source}")
    for part in (
        "'bad'", "'day'", "(SELECT 'bad')",
        "(SELECT p FROM (VALUES ('bad')) t(p))",
        "concat('b','ad')", "'bad'::ENUM('bad')",
        "CASE WHEN true THEN 'bad' ELSE 'day' END",
    ):
        SQL.append(f"SELECT {function}({part},DATE 'infinity',DATE 'epoch')")
    SQL.extend([
        f"SELECT {function}(p,DATE 'infinity',DATE 'epoch') FROM (SELECT 'bad' p FROM range(0))",
        f"SELECT {function}((SELECT 'bad'),DATE 'infinity',DATE 'epoch') FROM range(0)",
        f"SELECT CASE WHEN false THEN {function}((SELECT 'bad'),DATE 'infinity',DATE 'epoch') ELSE 7 END",
        f"SELECT {function}(p,d,DATE 'epoch') FROM (VALUES ('bad',DATE 'infinity'),('day',DATE '1970-01-02')) t(p,d)",
        f"SELECT {function}((SELECT CASE WHEN i=0 THEN 'bad' ELSE 'day' END),DATE 'infinity',DATE 'epoch') FROM range(2) t(i)",
        f"SELECT {function}('bad',NULL::DATE,CAST('bad' AS DATE))",
        f"SELECT {function}('bad',d,DATE 'epoch') FROM (SELECT NULL::DATE d)",
        f"SELECT {function}('bad',d,DATE 'epoch') FROM (SELECT NULL::DATE d FROM range(3))",
        f"SELECT {function}(p,d,DATE 'epoch') FROM (SELECT 'bad' p,NULL::DATE d)",
        f"SELECT {function}(p,CAST('bad' AS DATE),DATE 'epoch') FROM (SELECT NULL::VARCHAR p)",
        f"SELECT {function}('bad',d,CAST('bad' AS DATE)) FROM (SELECT NULL::DATE d)",
        f"SELECT {function}('bad',d,DATE 'epoch') FROM (VALUES (NULL::DATE)) t(d)",
    ])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('preserve earlier trials; choose a new report')
    before = source_fingerprint()
    build = ['cargo', 'build', '--offline', '--no-default-features', '--bin', 'duckdb-rust-test-worker']
    subprocess.run(build, cwd=ROOT, check=True)
    worker = ROOT/'target/debug/duckdb-rust-test-worker'
    report = {'recorded_at': datetime.now(timezone.utc).isoformat(), 'source_sha256': before,
              'build_command': build, 'rust_worker_sha256': digest(worker), 'script_sha256': digest(Path(__file__)),
              'scope': 'Exact result types/values or complete error messages for physical period-specifier provenance; no native/ICU/performance claim.',
              'full_parity': False, 'targets': []}
    for target, selected in TARGETS.items():
        require_checkout(selected.source, target)
        _, identity = require_reference(target=target)
        library = selected.build/'src'/('libduckdb.dylib' if platform.system() == 'Darwin' else 'libduckdb.so')
        reference = ROOT/f'target/temporal-provenance-{target}'
        compile_command = ['c++', '-std=c++17', '-O2', '-I'+str(selected.source/'src/include'),
                           str(ROOT/'test/runner/reference.cpp'), str(library), '-Wl,-rpath,'+str(library.parent), '-o', str(reference)]
        subprocess.run(compile_command, check=True)
        trial = {'target': target, 'reference_identity': identity, 'cpp_library_sha256': digest(library),
                 'cpp_worker_sha256': digest(reference), 'sql': []}
        report['targets'].append(trial)
        with tempfile.TemporaryDirectory(prefix='ddb-temporal-provenance-') as scratch:
            cpp = CppEngine(reference, scratch, time.monotonic()+120)
            rust = RustEngine(worker, scratch, time.monotonic()+120)
            try:
                if not selected.revision.startswith(cpp.identity['source_id']):
                    raise ValueError('loaded reference is not pinned')
                for sql in SQL:
                    a, b = (engine.request({'operation': 'query', 'sql': sql}) for engine in (rust, cpp))
                    keys = ('ok', 'columns', 'rows') if a.get('ok') and b.get('ok') else ('ok', 'message')
                    trial['sql'].append({'sql': sql, 'rust': a, 'cpp': b,
                                         'passed': all(a.get(key) == b.get(key) for key in keys)})
            finally:
                rust.close()
                cpp.close()
    if source_fingerprint() != before:
        raise RuntimeError('source changed during campaign')
    report['passed'] = all(case['passed'] for trial in report['targets'] for case in trial['sql'])
    args.report.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps({trial['target']: {'passed': sum(case['passed'] for case in trial['sql']), 'total': len(trial['sql'])}
                      for trial in report['targets']}))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
