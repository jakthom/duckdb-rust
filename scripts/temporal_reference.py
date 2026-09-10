"""Preserved typed temporal SQL and native-file evidence against both pinned cores.

This is correctness evidence, not a performance campaign or ICU-extension claim.
The default debug build keeps worker build artifacts isolated to this worktree.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import platform
import subprocess
import tempfile
import time

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import CppEngine, source_fingerprint
from run_upstream import RustEngine
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


SQL = [
    "SELECT TIME '24:00:00',TIME '12:34:56.1234567',TIME_NS '12:34:56.123456789'",
    "SELECT TIMESTAMP '1969-12-31 23:59:59.9999999',TIMESTAMP_NS '1969-12-31 23:59:59.999999999',TIMESTAMPTZ_NS 'epoch'",
    "SELECT TIMESTAMPTZ '2000-01-01 12:00:00+02',TIMETZ '12:00:00-05:30'",
    "SELECT INTERVAL '1.5 months',INTERVAL '1.5 days',INTERVAL '1 day -24 hours'",
    "SELECT INTERVAL '1 month'=INTERVAL '30 days',TIMETZ '13:00:00+01'=TIMETZ '12:00:00+00',TIMETZ '13:00:00+01'<TIMETZ '12:00:00+00'",
    "SELECT DATE '2024-01-31'+INTERVAL '1 month',TIMESTAMP '2023-03-31 12:01:02.123456'-INTERVAL '1 month'",
    "SELECT TIME '24:00:00'+INTERVAL '0 seconds',TIME '23:00:00'+INTERVAL '2 hours',TIMETZ '00:00:00+02'-INTERVAL '1 hour'",
    "SELECT TIMESTAMP '2000-01-02'-TIMESTAMP '2000-01-01',DATE '2000-01-01'+TIME '12:00:00',DATE '2000-01-01'+TIMETZ '12:00:00+02'",
    "SELECT -INTERVAL '1 month 2 days 03:04:05',INTERVAL '1 month'*1.5,INTERVAL '1 day'/2",
    "SELECT TIMESTAMP_NS '1969-12-31 23:59:59.999999999'::TIMESTAMP,TIMESTAMP '1969-12-31 23:59:59.999999'::TIMESTAMP_S",
    "SELECT min(t),max(t),count(DISTINCT t) FROM (VALUES (INTERVAL '1 month'),(INTERVAL '30 days'),(INTERVAL '31 days'),(NULL)) v(t)",
    "SELECT a.t,count(*) FROM (VALUES (INTERVAL '1 month'),(INTERVAL '30 days'),(INTERVAL '31 days')) a(t) JOIN (VALUES (INTERVAL '1 month'),(INTERVAL '31 days')) b(t) ON a.t=b.t GROUP BY a.t ORDER BY a.t",
    "SELECT t,min(t) OVER(ORDER BY t ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM (VALUES (TIMESTAMP 'epoch'),(TIMESTAMP '2000-01-01'),(NULL)) v(t) ORDER BY t",
    "SELECT TRY_CAST('25:00:00' AS TIME),TRY_CAST('2000-02-30 12:00:00' AS TIMESTAMP),TRY_CAST('1 blarg' AS INTERVAL)",
    "SELECT TIME '12:00+02'",
    "SELECT TIMESTAMP 'infinity'-TIMESTAMP 'infinity'",
    "SELECT TIME_NS '12:00:00'+INTERVAL '1 second'",
    "SELECT TIMESTAMPTZ_NS 'epoch'+INTERVAL '1 day'",
]
DEFINITION = "CREATE TABLE t(id INTEGER PRIMARY KEY,tm TIME DEFAULT TIME '12:00:00',ts TIMESTAMP DEFAULT TIMESTAMP 'epoch',s TIMESTAMP_S DEFAULT TIMESTAMP_S 'epoch',ms TIMESTAMP_MS DEFAULT TIMESTAMP_MS 'epoch',ns TIMESTAMP_NS DEFAULT TIMESTAMP_NS 'epoch',z TIMESTAMPTZ DEFAULT TIMESTAMPTZ 'epoch',tz TIMETZ DEFAULT TIMETZ '12:00:00+02',iv INTERVAL DEFAULT INTERVAL '1 month 2 days 03:04:05'); INSERT INTO t(id) VALUES(1); INSERT INTO t VALUES (2,NULL,TIMESTAMP '1969-12-31 23:59:59.999999',TIMESTAMP_S '2000-01-01',TIMESTAMP_MS '2000-01-01 12:00:00.123',TIMESTAMP_NS '2000-01-01 12:00:00.123456789',TIMESTAMPTZ '2000-01-01 12:00:00+02',TIMETZ '00:00:00-05:30',INTERVAL '-1 month 30 days -00:00:00.000001')"
QUERY = "SELECT id,tm::VARCHAR AS tm,ts::VARCHAR AS ts,s::VARCHAR AS s,ms::VARCHAR AS ms,ns::VARCHAR AS ns,z::VARCHAR AS z,tz::VARCHAR AS tz,iv::VARCHAR AS iv FROM t ORDER BY id"


def equivalent(left, right):
    if left.get('ok') and right.get('ok'):
        return left['columns'] == right['columns'] and left['rows'] == right['rows']
    # Error presence only here; retain full categories/messages, never assert
    # category parity from this coarse rejection check.
    return left.get('ok') is False and right.get('ok') is False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('preserve prior evidence; choose a new report')
    before = source_fingerprint()
    build = ['cargo', 'build', '--offline', '--no-default-features', '--bin', 'duckdb-rust', '--bin', 'duckdb-rust-test-worker']
    subprocess.run(build, cwd=ROOT, check=True)
    worker = ROOT / 'target/debug/duckdb-rust-test-worker'
    rust = Engine(ROOT / 'target/debug/duckdb-rust', True)
    report = {'recorded_at': datetime.now(timezone.utc).isoformat(), 'source_sha256': before, 'build_command': build,
              'rust_worker_sha256': digest(worker), 'rust_cli_sha256': digest(rust.binary), 'script_sha256': digest(Path(__file__)),
              'targets': [], 'full_parity': False, 'scope': 'Selected typed temporal SQL, scalar rejection presence, and bidirectional native checkpoint/WAL/default/index/mutation evidence. Error category and full temporal/ICU/performance parity remain separate obligations.'}
    for target, selected in TARGETS.items():
        require_checkout(selected.source, target)
        cpp_path, identity = require_reference(target=target)
        library = selected.build / 'src' / ('libduckdb.dylib' if platform.system() == 'Darwin' else 'libduckdb.so')
        reference = ROOT / f'target/temporal-reference-{target}'
        compile_command = ['c++', '-std=c++17', '-O2', '-I'+str(selected.source/'src/include'), str(ROOT/'test/runner/reference.cpp'), str(library), '-Wl,-rpath,'+str(library.parent), '-o', str(reference)]
        subprocess.run(compile_command, check=True)
        trial = {'target': target, 'reference_identity': identity, 'cpp_library_sha256': digest(library), 'cpp_worker_sha256': digest(reference), 'sql': [], 'persistence': []}
        report['targets'].append(trial)
        with tempfile.TemporaryDirectory(prefix='ddb-temporal-reference-') as scratch:
            cpp = CppEngine(reference, scratch, time.monotonic()+120)
            actual = RustEngine(worker, scratch, time.monotonic()+120)
            try:
                if not selected.revision.startswith(cpp.identity['source_id']):
                    raise ValueError('loaded reference differs from pinned revision')
                for sql in SQL:
                    a, b = actual.request({'operation': 'query', 'sql': sql}), cpp.request({'operation': 'query', 'sql': sql})
                    trial['sql'].append({'sql': sql, 'rust': a, 'cpp': b, 'passed': equivalent(a, b)})
            finally:
                actual.close()
                cpp.close()
            cpp_cli = Engine(cpp_path, False)
            for label, producer in [('cpp', cpp_cli), ('rust-checkpoint', rust), ('rust-wal', Engine(rust.binary, True, ('--durability', 'wal')))]:
                case = {'producer': label, 'passed': False}
                trial['persistence'].append(case)
                try:
                    path = Path(scratch) / (label+'.duckdb')
                    command(producer, path, DEFINITION)
                    case['checkpoint_sha256'] = digest(path)
                    wal = Path(str(path)+'.wal')
                    if wal.exists():
                        case['wal_sha256'] = digest(wal)
                    expected = command(cpp_cli, path, QUERY, json_output=True, readonly=True)
                    actual_rows = command(rust, path, QUERY, json_output=True, readonly=True)
                    if expected != actual_rows:
                        raise AssertionError({'cpp': expected, 'rust': actual_rows})
                    command(rust, path, "BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET ts=TIMESTAMP '1970-01-02',iv=iv+INTERVAL '1 day' WHERE id=1")
                    expected = command(cpp_cli, path, QUERY, json_output=True, readonly=True)
                    actual_rows = command(rust, path, QUERY, json_output=True, readonly=True)
                    if expected != actual_rows:
                        raise AssertionError({'cpp': expected, 'rust': actual_rows})
                    case.update(passed=True, final_rows=actual_rows)
                except Exception as error:
                    case['error'] = str(error)
    if source_fingerprint() != before:
        raise RuntimeError('source changed during campaign')
    report['passed'] = all(c['passed'] for t in report['targets'] for kind in ('sql', 'persistence') for c in t[kind])
    args.report.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps({t['target']: {kind: sum(c['passed'] for c in t[kind]) for kind in ('sql', 'persistence')} for t in report['targets']}))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
