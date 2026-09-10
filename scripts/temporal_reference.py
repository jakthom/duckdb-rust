"""Preserved typed temporal SQL and native-file evidence against both pinned cores.

This is correctness evidence, not a performance campaign or ICU-extension claim.
The default debug build keeps worker build artifacts isolated to this worktree.
"""
import argparse
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
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
    "SELECT make_date(2024,2,29),make_date(1),make_time(23,59,59.9999999),make_timestamp(2000,1,1,23,59,59.9999999)",
    "SELECT make_timestamp(-1),make_timestamp_ms(-1),make_timestamp_ns(-1),epoch_ms(-1)",
    "SELECT INTERVAL '1.9' YEAR,INTERVAL (-1.9) MONTH,INTERVAL '1.5' SECOND,INTERVAL 1 WEEK,INTERVAL 1 QUARTER,INTERVAL 1 CENTURY,INTERVAL 1 DECADE,INTERVAL 1 MILLENNIUM",
    "SELECT to_seconds(0.0000009),to_milliseconds(0.0009),to_years(2)+to_days(3)+to_hours(4)+to_minutes(5)+to_microseconds(6)",
    "SELECT last_day(DATE '2024-02-01'),dayname(DATE 'epoch'),monthname(DATE 'epoch'),quarter(DATE '2024-12-31'),dayofyear(DATE '2024-12-31'),dayofweek(DATE 'epoch'),isodow(DATE 'epoch')",
    "SELECT extract(year FROM DATE '0001-01-01 (BC)'),extract(microseconds FROM TIME '12:34:56.123456'),year(INTERVAL '-13 months'),month(INTERVAL '-13 months'),hour(INTERVAL '35 hours'),quarter(INTERVAL '-13 months'),century(INTERVAL '-1300 months')",
    "SELECT epoch(INTERVAL '1 year'),epoch(TIMETZ '13:00:00+01'),epoch_us(TIME_NS '00:00:00.000000001'),epoch(TIMESTAMP_NS '1969-12-31 23:59:59.999999999'),epoch_ns(TIMESTAMP_NS '1969-12-31 23:59:59.999999999')",
    "SELECT isinf(TIMESTAMP 'infinity'),isfinite(DATE 'epoch'),isfinite('NaN'::DOUBLE),isinf('NaN'::DOUBLE),isinf('-Infinity'::FLOAT),epoch(TIMESTAMP 'infinity'),year(DATE '-infinity')",
    "SELECT typeof(date_part(p,t)),date_part(p,t) FROM (VALUES ('epoch',TIMESTAMP 'epoch'),('year',TIMESTAMP '2000-01-01')) v(p,t)",
    "SELECT date_part('year',DATE 'epoch'),date_part('epoch',TIMESTAMP 'epoch'),date_part('timezone',TIMETZ '12:00:00-05:30')",
    "SELECT INTERVAL (n) DAY FROM (VALUES (1.9),(-1.9),(NULL)) v(n)",
    "SELECT make_date(2023,2,29)",
    "SELECT make_time(25,0,0)",
    "SELECT to_years(2147483647)",
    "SELECT make_timestamp_ns('-9223372036854775808'::BIGINT)",
    "SELECT make_date(DATE 'epoch')",
    "SELECT make_time(TIME '00:00')",
    "SELECT isfinite(INTERVAL '1 year')",
    "SELECT isinf(TIME '00:00')",
    "SELECT year(TIME '00:00')",
    "SELECT year(TIMESTAMPTZ 'epoch')",
    "SELECT INTERVAL 2147483648 DAY",
    "SELECT TIMESTAMP_S '1969-12-31 23:59:59.999999',TIMESTAMP_MS '1969-12-31 23:59:59.999999'",
    "SELECT TIMESTAMPTZ '2000-01-01 00:00:00+23:59'",
    "SELECT TIMESTAMPTZ 'epoch'::DATE",
    "SELECT TIMETZ '12:00:00+02'::TIME",
    "SELECT TIME '12:00:00'::TIMETZ",
    "SELECT TIME_NS '00:00:00.000000500'::TIME,TIME_NS '24:00:00'::TIME,TIME '24:00:00'::TIME_NS,TIMESTAMP_NS '1969-12-31 23:59:59.999999999'::TIME,TIMESTAMP_NS '1969-12-31 23:59:59.999999999'::DATE",
    "SELECT TIMESTAMP_S '1969-12-31 23:59:59.5',TIMESTAMP_S '1970-01-01 00:00:00.5',TIMESTAMP_MS '1970-01-01 00:00:00.0005',TIMESTAMP_NS '2000-01-01 23:59:59.999999500'::DATE,TIMESTAMP_NS '1969-12-31 23:59:59.999999500'::TIMESTAMP",
    "SELECT epoch(INTERVAL '1 year'),epoch_ms(INTERVAL '1 year'),epoch_us(INTERVAL '1 year'),epoch_ns(INTERVAL '1 year'),epoch_ms(TIMESTAMP '1969-12-31 23:59:59.999500'),epoch(TIME_NS '00:00:00.000000789')",
    "SELECT typeof(NULL::TIMESTAMP(0)),typeof(NULL::TIMESTAMP(3)),typeof(NULL::TIMESTAMP(6)),typeof(NULL::TIMESTAMP(10)),typeof(NULL::TIMESTAMP(3) WITH TIME ZONE)",
    "SELECT NULL::TIMESTAMP(11)",
    "SELECT NULL::TIMESTAMPTZ(3)",
    "SELECT NULL::TIME(3)",
    "SELECT INTERVAL '2y3mons4d ago',INTERVAL '@ 2y 3mons 4d',INTERVAL '123.5',INTERVAL '1.1quarters',INTERVAL '1.1years'",
    "SELECT INTERVAL '0.0000009 seconds',INTERVAL '1.9us',INTERVAL '1.9ms',INTERVAL '0.1 months',INTERVAL '0.1 weeks'",
    "SELECT INTERVAL '1:02:03 junk',INTERVAL '1:02:03 4days',INTERVAL '1:02:03 ago',INTERVAL '1:'",
    "SELECT INTERVAL '+1 day'",
    "SELECT INTERVAL '.5 seconds'",
    "SELECT INTERVAL ' @1 day'",
    "SELECT INTERVAL '2147483647days 1day -1day'",
    "SELECT INTERVAL 'P1Y'",
    "SELECT INTERVAL '9223372036854775807us 1us -1us'",
    "SELECT INTERVAL '-2147483648months ago'",
    "SELECT INTERVAL '0.00000000001 seconds'",
    "SELECT INTERVAL 2 YEARS,INTERVAL 2 MONTHS,INTERVAL 2 DAYS,INTERVAL 2 HOURS,INTERVAL 2 MINUTES,INTERVAL 2 SECONDS",
    "SELECT INTERVAL 3 MICROSECONDS,INTERVAL 3 MILLISECONDS,INTERVAL 3 WEEKS,INTERVAL 3 QUARTERS,INTERVAL 3 DECADES,INTERVAL 3 CENTURIES,INTERVAL 3 MILLENNIA",
    "SELECT INTERVAL (i+1) DAYS FROM range(1,4) t(i)",
    "SELECT INTERVAL '2 10' YEARS TO MONTHS",
    "SELECT CAST('3 DOOPIEDOOS' AS INTERVAL)",
    "SELECT CAST('3 years 2 doy' AS INTERVAL)",
    "SELECT TRY_CAST('   ' AS INTERVAL),TRY_CAST('AAAA' AS INTERVAL),TRY_CAST('3 DOOPIEDOOS' AS INTERVAL),TRY_CAST('2147483648 days' AS INTERVAL)",
]
CAST_VALUES = {
    'DATE': "DATE '2000-01-01'",
    'TIME': "TIME '12:34:56.123456'",
    'TIME_NS': "TIME_NS '12:34:56.123456789'",
    'TIMETZ': "TIMETZ '12:34:56.123456+02'",
    'TIMESTAMP': "TIMESTAMP '2000-01-01 12:34:56.123456'",
    'TIMESTAMP_S': "TIMESTAMP_S '2000-01-01 12:34:56'",
    'TIMESTAMP_MS': "TIMESTAMP_MS '2000-01-01 12:34:56.123'",
    'TIMESTAMP_NS': "TIMESTAMP_NS '2000-01-01 12:34:56.123456789'",
    'TIMESTAMPTZ': "TIMESTAMPTZ '2000-01-01 12:34:56.123456+02'",
    'TIMESTAMPTZ_NS': "TIMESTAMPTZ_NS '2000-01-01 12:34:56.123456789+02'",
    'INTERVAL': "INTERVAL '1 month 2 days 03:04:05'",
}
SQL += [f'SELECT ({value})::{target}' for value in CAST_VALUES.values() for target in CAST_VALUES]
SQL += [f'SELECT {name}({value})' for name in ('year','month','day','quarter','dayofyear','dayofweek','isodow','century','decade','millennium','hour','minute','second','millisecond','microsecond','epoch','epoch_ms','epoch_us','epoch_ns','isfinite','isinf','last_day','dayname','monthname') for value in CAST_VALUES.values()]
CLOCK_TEXT = [
    '1:', '1:2', '1:02:', '1:2:3', '12:34:56junk', '12:34:56 +02',
    '12:34:56+2', '12:34:56+02', '12:34:56+02junk', '12:34:56Z',
    '12:34:56+00:99:99', '12:34:56+16', '12:34+02', '24:00:00.000000999',
    '2000-01-01 12:34:56+02', '2000-01-01 12:34:56.123456789',
    '2000-01-01 12:34:56 UTC', '2000-01-01 12:34:56 America/New_York',
    '2000-01-01T12:34:56',
]
TIMESTAMP_TEXT = [
    '2000-01-01 1:2', '2000-01-01 1:', '2000-01-01 12:34+02',
    '2000-01-01 12:34:56+2', '2000-01-01 12:34:56+02',
    '2000-01-01 12:34:56+99:99:99', '2000-01-01 12:34:56+000000',
    '2000-01-01 12:34:56+00:00:99', '2000-01-01 12:34:56+00:99',
    '2000-01-01 12:34:56+02junk', '2000-01-01 12:34:56 UTC',
    '2000-01-01 12:34:56 utc', '2000-01-01 12:34:56 America/New_York',
    '2000-01-01 12:34:56 America-New/York', '2000-01-01 12:34:56  UTC',
    '2000-01-01 12:34:56 junk', '2000 01 02 12:34:56',
    '0001-01-01 (BC) 12:34:56', '0001-01-01 (BC)', 'epoch', '-epoch',
    '-infinity', '2000-01-01 ', '2000-01-01\t', '2000-01-01 24:00:00.000000999',
]
SQL += [f"SELECT {target} '{value}'" for value in CLOCK_TEXT for target in ('TIME','TIME_NS','TIMETZ')]
SQL += [f"SELECT {target} '{value}'" for value in TIMESTAMP_TEXT for target in ('TIMESTAMP','TIMESTAMP_S','TIMESTAMP_MS','TIMESTAMP_NS','TIMESTAMPTZ','TIMESTAMPTZ_NS')]
SQL += [
    "SELECT v,v::TIME,(v::TIME)::TIME_NS,(v::TIME)::TIMETZ,epoch_us(v) FROM (VALUES (TIME_NS '24:00:00'),(TIME_NS '24:00:00.000000001'),(TIME_NS '24:00:00.000000499'),(TIME_NS '24:00:00.000000500'),(TIME_NS '24:00:00.000000999')) t(v) ORDER BY v",
    "SELECT TRY_CAST('24:00:00.000001' AS TIME_NS),TRY_CAST('24:00:00.000001' AS TIME),TRY_CAST('24:00:00.000001+00' AS TIMETZ)",
    "SELECT make_time(23,59,60),make_time(23,59,60.49999999999999),make_time(0,0,-0.0000001),make_time(23,59,60.49)::TIME_NS,make_time(23,59,60.49)::TIMETZ",
    "SELECT make_time(23,59,60.5)",
    "SELECT DATE '2000-01-01'+(TIME_NS '24:00:00.000000999')::TIME,(TIME_NS '24:00:00.000000999')::TIME+INTERVAL '0 seconds'",
]
SQL += [
    "SELECT TIMESTAMP '290309-12-21 (BC) 23:59:59.999999'",
    "SELECT DATE '290309-12-21 (BC)'+TIME '23:59:59.999999'",
    "SELECT DATE '294247-01-10'+TIMETZ '04:00:54.775807+01'",
    "SELECT make_timestamp(-290308,12,21,23,59,59.999999)",
    "SELECT DATE '290309-12-21 (BC)'+INTERVAL '1day'",
    "SELECT make_timestamp(-9223372036854775806)+INTERVAL '1day'",
    "SELECT t+INTERVAL '-1day 24hours' FROM (VALUES (TIMESTAMP '290309-12-22 (BC) 00:00:00'),(TIMESTAMP '294247-01-10 04:00:54.775806')) v(t)",
    "SELECT DATE 'inf',TIMESTAMP '-INF',TRY_CAST('inf ' AS TIMESTAMP)",
    "SELECT TRY_CAST(['1 day','2147483648 days','9223372036854775807us 1us'] AS INTERVAL[])::VARCHAR",
]
SQL += [f"SELECT TIMESTAMP '{value}'{operator}INTERVAL '{unit}'"
        for value, operator in [('290309-12-22 (BC) 00:00:00', '-'),
                                ('294247-01-10 04:00:54.775806', '+')]
        for unit in ['1us', '1second', '1hour', '1day', '1month', '1year']]
SQL += [f"SELECT epoch_ns({kind} '1677-09-21 00:12:43.{fraction}')"
        for kind in ['TIMESTAMP_NS', 'TIMESTAMPTZ_NS']
        for fraction in ['145224194', '145224500', '145224999', '145225000', '145225001']]
SQL += [
    "SELECT epoch_ns(TRY_CAST('1677-09-21 00:12:43.145224999' AS TIMESTAMP_NS))",
    "SELECT TIME_NS '1677-09-21 00:12:43.145224194'",
    "SELECT epoch_ns(TIMESTAMP_NS '2262-04-11 23:47:16.854775806')",
]
RENDER_SQL = [
    "SELECT make_timestamp(-9223372036854775806)",
    "SELECT make_timestamp_ns(-9223372036854775806)",
    "SELECT {'t':make_timestamp_ns(-9223372036854775806)}",
    "SELECT make_timestamp(-9223372036854775806)::VARCHAR",
    "SELECT TRY_CAST(make_timestamp(-9223372036854775806) AS VARCHAR)",
    "SELECT make_timestamp_ns(-9223372036854775806)::VARCHAR",
    "SELECT TRY_CAST([make_timestamp_ns(-9223372036854775806)] AS VARCHAR)",
    "SELECT TRY_CAST(make_timestamp(-9223372036854775806)::VARIANT AS VARCHAR)",
]
SQL += RENDER_SQL
BOUNDARY_DEFINITION = "CREATE TABLE clock_boundaries(id INTEGER PRIMARY KEY,n TIME_NS UNIQUE,u TIME,z TIMETZ,child STRUCT(n TIME_NS,z TIMETZ),items TIME_NS[]); INSERT INTO clock_boundaries SELECT id,v,v::TIME,(v::TIME)::TIMETZ,{'n':v,'z':(v::TIME)::TIMETZ},[v,NULL] FROM (VALUES(1,TIME_NS '24:00:00'),(2,TIME_NS '24:00:00.000000001'),(3,TIME_NS '24:00:00.000000999'),(4,make_time(23,59,60.49999999999999)::TIME_NS)) t(id,v)"
BOUNDARY_QUERY = "SELECT id,n::VARCHAR AS n,u::VARCHAR AS u,z::VARCHAR AS z,child::VARCHAR AS child,items::VARCHAR AS items,(DATE '2000-01-01'+u)::VARCHAR AS shifted FROM clock_boundaries ORDER BY id"
DEFINITION = "CREATE TABLE t(id INTEGER PRIMARY KEY,tm TIME DEFAULT TIME '12:00:00',ts TIMESTAMP DEFAULT TIMESTAMP 'epoch',s TIMESTAMP_S DEFAULT TIMESTAMP_S 'epoch',ms TIMESTAMP_MS DEFAULT TIMESTAMP_MS 'epoch',ns TIMESTAMP_NS DEFAULT TIMESTAMP_NS 'epoch',z TIMESTAMPTZ DEFAULT TIMESTAMPTZ 'epoch',tz TIMETZ DEFAULT TIMETZ '12:00:00+02',iv INTERVAL DEFAULT INTERVAL '1 month 2 days 03:04:05'); INSERT INTO t(id) VALUES(1); INSERT INTO t VALUES (2,NULL,TIMESTAMP '1969-12-31 23:59:59.999999',TIMESTAMP_S '2000-01-01',TIMESTAMP_MS '2000-01-01 12:00:00.123',TIMESTAMP_NS '2000-01-01 12:00:00.123456789',TIMESTAMPTZ '2000-01-01 12:00:00+02',TIMETZ '00:00:00-05:30',INTERVAL '-1 month 30 days -00:00:00.000001')"
DEFINITION += "; INSERT INTO t(id,ts) VALUES (3,TIMESTAMP '290309-12-22 (BC) 00:00:00'),(4,TIMESTAMP '294247-01-10 04:00:54.775806')"
QUERY = "SELECT id,tm::VARCHAR AS tm,ts::VARCHAR AS ts,s::VARCHAR AS s,ms::VARCHAR AS ms,ns::VARCHAR AS ns,z::VARCHAR AS z,tz::VARCHAR AS tz,iv::VARCHAR AS iv,(ts+INTERVAL '0us')::VARCHAR AS calendar_roundtrip FROM t ORDER BY id"


def equivalent(left, right):
    if left.get('ok') and right.get('ok'):
        if left['columns'] != right['columns'] or len(left['rows']) != len(right['rows']):
            return False
        for a, b in zip(left['rows'], right['rows']):
            if len(a) != len(b) or len(a) != len(left['columns']):
                return False
            for kind, x, y in zip(left['columns'], a, b):
                if x == y:
                    continue
                if kind not in ('FLOAT', 'DOUBLE') or x == 'NULL' or y == 'NULL':
                    return False
                try:
                    # Exact numeric equality, not a tolerance: textual 0 and
                    # 0.0 denote the same floating value. Retain both originals.
                    if Decimal(x) != Decimal(y):
                        return False
                except InvalidOperation:
                    return False
        return True
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
                    # Development's non-fallible string-cast metadata turns
                    # these range failures into INTERNAL errors and may poison
                    # the database. Each extrema witness needs a fresh engine,
                    # not a later rejection caused by an earlier failure.
                    fresh = sql in RENDER_SQL
                    if fresh:
                        for engine in (actual, cpp):
                            reset = engine.request({'operation': 'load'})
                            if not reset.get('ok'):
                                raise RuntimeError({'extrema_reset_failed': reset})
                    a, b = actual.request({'operation': 'query', 'sql': sql}), cpp.request({'operation': 'query', 'sql': sql})
                    trial['sql'].append({'sql': sql, 'rust': a, 'cpp': b, 'fresh_database': fresh, 'passed': equivalent(a, b)})
            finally:
                actual.close()
                cpp.close()
            cpp_cli = Engine(cpp_path, False, serialize_json_rows=selected.serialize_json_rows)
            for label, producer in [('cpp', cpp_cli), ('rust-checkpoint', rust), ('rust-wal', Engine(rust.binary, True, ('--durability', 'wal')))]:
                case = {'producer': label, 'passed': False}
                trial['persistence'].append(case)
                try:
                    path = Path(scratch) / (label+'.duckdb')
                    command(producer, path, DEFINITION + ';' + BOUNDARY_DEFINITION)
                    case['checkpoint_sha256'] = digest(path)
                    wal = Path(str(path)+'.wal')
                    if wal.exists():
                        case['wal_sha256'] = digest(wal)
                    expected = command(cpp_cli, path, QUERY, json_output=True, readonly=True)
                    actual_rows = command(rust, path, QUERY, json_output=True, readonly=True)
                    if expected != actual_rows:
                        raise AssertionError({'cpp': expected, 'rust': actual_rows})
                    expected_boundary = command(cpp_cli, path, BOUNDARY_QUERY, json_output=True, readonly=True)
                    actual_boundary = command(rust, path, BOUNDARY_QUERY, json_output=True, readonly=True)
                    case['initial_clock_boundaries'] = {'cpp': expected_boundary, 'rust': actual_boundary}
                    if expected_boundary != actual_boundary:
                        raise AssertionError(case['initial_clock_boundaries'])
                    command(rust, path, "BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET ts=TIMESTAMP '1970-01-02',iv=iv+INTERVAL '1 day' WHERE id=1")
                    command(rust, path, "BEGIN; DELETE FROM clock_boundaries; ROLLBACK; UPDATE clock_boundaries SET n=(n::TIME)::TIME_NS WHERE id=3")
                    expected = command(cpp_cli, path, QUERY, json_output=True, readonly=True)
                    actual_rows = command(rust, path, QUERY, json_output=True, readonly=True)
                    if expected != actual_rows:
                        raise AssertionError({'cpp': expected, 'rust': actual_rows})
                    expected_boundary = command(cpp_cli, path, BOUNDARY_QUERY, json_output=True, readonly=True)
                    actual_boundary = command(rust, path, BOUNDARY_QUERY, json_output=True, readonly=True)
                    case['final_clock_boundaries'] = {'cpp': expected_boundary, 'rust': actual_boundary}
                    if expected_boundary != actual_boundary:
                        raise AssertionError(case['final_clock_boundaries'])
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
