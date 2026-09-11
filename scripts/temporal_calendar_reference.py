"""Retained core calendar truncation and bucketing evidence for both pinned references."""
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
from verify_reference import Engine, command


SQL = []
for name in ('date_trunc', 'datetrunc'):
    for unit in ('millennium','century','decade','year','quarter','month','week','isoyear',
                 'day','hour','minute','second','millisecond','microsecond',
                 'yr','MONS','dayofweek','isodow','doy','julian','yearweek','epoch','mil','usecs'):
        for value in ("DATE '2001-08-22'", "TIMESTAMP '1969-12-31 23:59:59.999999'",
                      "TIMESTAMP_NS '1969-12-31 23:59:59.999999999'",
                      "INTERVAL '-13 months -8 days -01:02:03.456789'"):
            SQL.append(f"SELECT {name}('{unit}',{value})")
    for part in ("'bad'", "'era'", "'timezone'", "'day'", "NULL", "'day'::ENUM('day')"):
        for value in ("DATE 'infinity'", "DATE '-infinity'", "NULL::DATE", "NULL::INTERVAL"):
            SQL.append(f"SELECT {name}({part},{value})")
    for part in ('bad','era','day'):
        for source in (f"(VALUES ('{part}'))t(p)", f"(SELECT '{part}' p)",
                       f"(SELECT '{part}' p FROM range(3) ORDER BY 1)"):
            for value in ("DATE 'infinity'", "NULL::DATE", "DATE 'epoch'"):
                SQL.append(f"SELECT {name}(p,{value}) FROM {source}")
    for expression in (
        "'day','2001-02-03'", "'day',NULL", "'day',TIME '12:00:00'",
        "'day',TIME_NS '12:00:00'", "'day',TIMETZ '12:00:00+00'",
        "1,DATE 'epoch'", "'day',true",
        "'day',TIMESTAMP_S '2001-02-03 12:34:56'",
        "'day',TIMESTAMP_MS '2001-02-03 12:34:56.789'",
        "'month',DATE '0002-12-31 (BC)'", "'isoyear',DATE '0001-01-01 (BC)'",
        "'year',DATE '5881580-07-10'", "'millennium',DATE '5877642-06-25 (BC)'",
        "'era',INTERVAL '1 day'",
    ):
        SQL.append(f"SELECT {name}({expression})")
    for unit in ('day','hour','minute','second','millisecond','microsecond','month','isoyear'):
        for ticks in (-9223372036854775806,-9223372036854000000,9223372036854775806):
            SQL.append(f"SELECT epoch_us({name}('{unit}',make_timestamp({ticks})))")
    for query in (
        f"SELECT CASE WHEN false THEN {name}('bad',DATE 'epoch') ELSE NULL END",
        f"SELECT CASE WHEN false THEN {name}('bad',INTERVAL '1 day') ELSE NULL END",
        f"SELECT {name}('bad',d) FROM (SELECT NULL::DATE d)",
        f"SELECT {name}(p,CAST('bad' AS DATE)) FROM (SELECT NULL::VARCHAR p)",
        f"SELECT {name}((SELECT 'era'),DATE 'infinity')",
        f"SELECT {name}((SELECT 'bad'),DATE 'infinity') FROM range(0)",
    ):
        SQL.append(query)

for width in ('1 microsecond','7 milliseconds','1 second','5 minutes','3 hours','2 days',
              '1 week','3 months','2 years','1 day -1 hour','-1 day 25 hours','0 days',
              '-1 month','1 month 1 day','106751992 days'):
    for value in ("DATE '2001-08-22'", "TIMESTAMP '1969-12-31 23:59:59.999999'",
                  "TIME '12:34:56.789123'", "TIMESTAMP_NS '2001-02-03 12:34:56.123456789'",
                  "DATE 'infinity'", "NULL::DATE"):
        SQL.append(f"SELECT time_bucket(INTERVAL '{width}',{value})")
        for offset in ("INTERVAL '12 hours'", "INTERVAL '-1 month 2 days'", "NULL::INTERVAL"):
            SQL.append(f"SELECT time_bucket(INTERVAL '{width}',{value},{offset})")
    for value, origin in (("DATE '2001-08-22'", "DATE '2000-01-17'"),
                          ("TIMESTAMP '1969-12-31 23:59:59.999999'", "TIMESTAMP '1969-12-25 23:01:00'"),
                          ("TIME '12:34:56.789123'", "TIME '03:15:00'"),
                          ("DATE 'infinity'", "DATE '-infinity'"),
                          ("DATE '2001-08-22'", "DATE 'infinity'")):
        SQL.append(f"SELECT time_bucket(INTERVAL '{width}',{value},{origin})")
for args in ("'1 day',DATE 'epoch'", "INTERVAL '1 day',NULL",
             "INTERVAL '1 day',DATE 'epoch',NULL", "INTERVAL '1 day','2001-02-03'",
             "INTERVAL '1 day',TIME_NS '12:00:00'", "NULL::INTERVAL,CAST('bad' AS DATE)",
             "INTERVAL '1 day',DATE 'epoch',TIMESTAMP '2000-01-03 01:02:03'",
             "INTERVAL '1 day',TIMESTAMP 'epoch',DATE '2000-01-03'",
             "INTERVAL '1 day',DATE 'epoch','2000-01-03'",
             "INTERVAL '1 day','2001-02-03',DATE '2000-01-03'"):
    SQL.append(f"SELECT time_bucket({args})")

DEFINITION = "CREATE TABLE calendar_results(id INTEGER PRIMARY KEY,k TIMESTAMP UNIQUE,b DATE,clock TIME,p STRUCT(d DATE,v TIMESTAMP[],span INTERVAL)); INSERT INTO calendar_results SELECT id,date_trunc('day',ts),time_bucket(INTERVAL '1 month',ts::DATE),TIME '12:00:00',{'d':time_bucket(INTERVAL '1 week',ts::DATE),'v':[date_trunc('hour',ts),NULL::TIMESTAMP],'span':date_trunc('month',INTERVAL '14 months 3 days')} FROM (VALUES (1,TIMESTAMP_NS '2024-01-31 12:34:56.123456789'),(2,TIMESTAMP_NS '2024-02-01 23:59:59.999999999'),(3,NULL::TIMESTAMP_NS)) t(id,ts)"
QUERY = "SELECT id,k::VARCHAR AS k,b::VARCHAR AS b,clock::VARCHAR AS clock,p::VARCHAR AS p,time_bucket(INTERVAL '1 week',k)::VARCHAR AS week,date_trunc('month',k)::VARCHAR AS month FROM calendar_results ORDER BY id"
MUTATION = "BEGIN; DELETE FROM calendar_results WHERE id=2; UPDATE calendar_results SET b=DATE 'epoch'; ROLLBACK; UPDATE calendar_results SET k=date_trunc('day',TIMESTAMP '2024-03-04 12:34:56'),b=time_bucket(INTERVAL '1 month',DATE '2024-03-04'),clock=time_bucket(INTERVAL '1 hour',TIME '00:00:00',TIME '00:30:00'),p={'d':time_bucket(INTERVAL '1 week',DATE '2024-03-04'),'v':[date_trunc('hour',TIMESTAMP '2024-03-04 12:34:56')],'span':date_trunc('year',INTERVAL '14 months')} WHERE id=1"


def native_cases(scratch, rust, cpp):
    results = []
    for label, producer in [('cpp', cpp), ('rust-checkpoint', rust),
                             ('rust-wal', Engine(rust.binary, True, ('--durability', 'wal')))]:
        case = {'producer': label, 'definition': DEFINITION, 'query': QUERY,
                'mutation': MUTATION, 'passed': False, 'observations': []}
        results.append(case)
        try:
            path = Path(scratch)/(label+'.duckdb')
            command(producer, path, DEFINITION)
            case['checkpoint_sha256'] = digest(path)
            wal = Path(str(path)+'.wal')
            if wal.exists():
                case['wal_sha256'] = digest(wal)
            for stage in ('initial', 'after-rust-mutation'):
                if stage == 'after-rust-mutation':
                    command(rust, path, MUTATION)
                expected = command(cpp, path, QUERY, json_output=True, readonly=True)
                actual = command(rust, path, QUERY, json_output=True, readonly=True)
                case['observations'].append({'stage': stage, 'cpp': expected, 'rust': actual})
                if expected != actual:
                    raise AssertionError(case['observations'][-1])
            case['passed'] = True
        except Exception as error:
            case['error'] = str(error)
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('preserve earlier trials; choose a new report')
    before = source_fingerprint()
    build = ['cargo', 'build', '--offline', '--no-default-features', '--bin', 'duckdb-rust-test-worker', '--bin', 'duckdb-rust']
    subprocess.run(build, cwd=ROOT, check=True)
    worker = ROOT/'target/debug/duckdb-rust-test-worker'
    rust_cli = Engine(ROOT/'target/debug/duckdb-rust', True)
    report = {'recorded_at': datetime.now(timezone.utc).isoformat(), 'source_sha256': before,
              'build_command': build, 'rust_worker_sha256': digest(worker), 'script_sha256': digest(Path(__file__)),
              'scope': 'Exact result types/values or complete error messages for core calendar functions; broader rejection-presence is recorded separately, not substituted. Bidirectional mixed native checkpoint/WAL/reopen with Rust mutation; no DEFAULT/ICU/performance claim.',
              'full_parity': False, 'targets': []}
    for target, selected in TARGETS.items():
        require_checkout(selected.source, target)
        cpp_path, identity = require_reference(target=target)
        library = selected.build/'src'/('libduckdb.dylib' if platform.system() == 'Darwin' else 'libduckdb.so')
        reference = ROOT/f'target/temporal-calendar-{target}'
        compile_command = ['c++', '-std=c++17', '-O2', '-I'+str(selected.source/'src/include'),
                           str(ROOT/'test/runner/reference.cpp'), str(library), '-Wl,-rpath,'+str(library.parent), '-o', str(reference)]
        subprocess.run(compile_command, check=True)
        trial = {'target': target, 'reference_identity': identity, 'cpp_library_sha256': digest(library),
                 'cpp_worker_sha256': digest(reference), 'sql': []}
        report['targets'].append(trial)
        with tempfile.TemporaryDirectory(prefix='ddb-temporal-calendar-') as scratch:
            cpp = CppEngine(reference, scratch, time.monotonic()+360)
            rust = RustEngine(worker, scratch, time.monotonic()+120)
            try:
                if not selected.revision.startswith(cpp.identity['source_id']):
                    raise ValueError('loaded reference is not pinned')
                for sql in SQL:
                    a, b = (engine.request({'operation': 'query', 'sql': sql}) for engine in (rust, cpp))
                    keys = ('ok', 'columns', 'rows') if a.get('ok') and b.get('ok') else ('ok', 'message')
                    trial['sql'].append({'sql': sql, 'rust': a, 'cpp': b,
                                         'passed': all(a.get(key) == b.get(key) for key in keys),
                                         'outcome_passed': (not a.get('ok') and not b.get('ok')) or all(a.get(key) == b.get(key) for key in keys)})
            finally:
                rust.close()
                cpp.close()
            trial['persistence'] = native_cases(scratch, rust_cli,
                Engine(cpp_path, False, serialize_json_rows=selected.serialize_json_rows))
    if source_fingerprint() != before:
        raise RuntimeError('source changed during campaign')
    report['passed'] = all(case['passed'] for trial in report['targets'] for kind in ('sql','persistence') for case in trial[kind])
    args.report.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps({trial['target']: {'passed': sum(case['passed'] for case in trial['sql']), 'total': len(trial['sql']), 'outcome_passed': sum(case['outcome_passed'] for case in trial['sql']), 'persistence': sum(case['passed'] for case in trial['persistence'])}
                      for trial in report['targets']}))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
