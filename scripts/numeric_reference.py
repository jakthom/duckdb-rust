"""Typed numeric SQL and bidirectional native persistence against both pinned references."""
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
    "SELECT 255::UTINYINT,65535::USMALLINT,4294967295::UINTEGER,18446744073709551615::UBIGINT,'340282366920938463463374607431768211455'::UHUGEINT",
    "SELECT 1.25, 1.25+2.5, 1.25*2.5, 1.25/2.5, 10.0%3.0, -1.25",
    "SELECT 1::UTINYINT+1, 1::UTINYINT+1::USMALLINT, 1::UINTEGER+1::BIGINT, 1::UBIGINT+1::BIGINT, 1::UHUGEINT+1::BIGINT",
    "SELECT 0.5::DECIMAL(1,1)+1::TINYINT, 0.5::DECIMAL(1,1)+1::BIGINT, 0.5::DECIMAL(1,1)*10::INTEGER",
    "SELECT '-1.235'::DECIMAL(5,2), '1.234999999999999999999999999999999999'::DECIMAL(5,2), '123.45e-1'::DECIMAL(8,3)",
    "SELECT 1.235::DECIMAL(5,2), (-1.235)::DECIMAL(5,2), (1.5::DECIMAL(3,1))::INTEGER, (-1.5::DECIMAL(3,1))::INTEGER",
    "SELECT TRY_CAST('256' AS UTINYINT),TRY_CAST('-1' AS UHUGEINT),TRY_CAST('999.995' AS DECIMAL(5,2)),TRY_CAST('bad' AS DECIMAL(38,3))",
    "SELECT abs(-1.25),round(-1.25),abs(1::UTINYINT),round(1::UBIGINT),sqrt(4::DECIMAL(3,1))",
    "SELECT x FROM (SELECT 1::UTINYINT x UNION ALL SELECT -1::TINYINT UNION ALL SELECT 255::UTINYINT) t ORDER BY x",
    "SELECT x FROM (SELECT 1.25::DECIMAL(5,2) x UNION ALL SELECT 1.250::DECIMAL(8,3)) t ORDER BY x",
    "SELECT sum(x),avg(x),min(x),max(x) FROM (VALUES (1.25::DECIMAL(5,2)),(2.50),(NULL)) t(x)",
    "SELECT sum(x),avg(x),min(x),max(x) FROM (VALUES (18446744073709551615::UBIGINT),(1::UBIGINT),(NULL)) t(x)",
    "SELECT sum(x),avg(x) FROM (VALUES ('340282366920938463463374607431768211455'::UHUGEINT),(NULL)) t(x)",
    "SELECT x, sum(x) OVER (ORDER BY x ROWS UNBOUNDED PRECEDING) FROM (VALUES (1.25::DECIMAL(5,2)),(2.50)) t(x) ORDER BY x",
    "SELECT a.x FROM (VALUES (1.25::DECIMAL(5,2)),(NULL)) a(x) JOIN (VALUES (1.250::DECIMAL(8,3))) b(x) USING(x)",
    "SELECT x FROM (VALUES ('340282366920938463463374607431768211455'::UHUGEINT),(0::UHUGEINT),(NULL)) t(x) INTERSECT SELECT x FROM (VALUES ('340282366920938463463374607431768211455'::UHUGEINT),(NULL)) u(x) ORDER BY x",
    "SELECT typeof(1::DECIMAL(18,0)+1::DECIMAL(18,0)),typeof(coalesce(1::DECIMAL(38,0),1::DECIMAL(38,38)))",
    "SELECT typeof(CAST('bad' AS DECIMAL(5,2))),typeof(round(1::UTINYINT)),typeof(round(1::UHUGEINT))",
    "SELECT '1_2.3_4'::DECIMAL(5,2),'0xf_f'::UHUGEINT,'0b1_0'::UBIGINT,'1e1_0'::DECIMAL(38,0)",
    "SELECT TRY_CAST('+0xff' AS UBIGINT),TRY_CAST('0x_ff' AS UBIGINT),TRY_CAST('1__2' AS UBIGINT),TRY_CAST('1_.0' AS DECIMAL(5,2))",
]
ERROR_CASES = [(f'SELECT -1::{kind}', 'Out of Range Error')
               for kind in ('UTINYINT', 'USMALLINT', 'UINTEGER', 'UBIGINT', 'UHUGEINT')]
ERROR_CASES += [(f'SELECT 1::{kind}{op}0::{kind}', 'Invalid Input Error')
                for kind in ('UTINYINT', 'USMALLINT', 'UINTEGER', 'UBIGINT', 'UHUGEINT') for op in ('//', '%')]
ERROR_CASES += [('SELECT 1.0%0.0', 'Invalid Input Error'),
                ("SELECT 'a'::DECIMAL(5,2)", 'Conversion Error'),
                ('SELECT 256::UTINYINT', 'Conversion Error')]


def equivalent(a, b, expected_error=None):
    if expected_error:
        return all(result.get('ok') is False and not result.get('unsupported')
                   and result.get('message', '').startswith(expected_error + ':') for result in (a, b))
    if not a.get('ok') or not b.get('ok') or a['columns'] != b['columns'] or len(a['rows']) != len(b['rows']):
        return False
    for left, right in zip(a['rows'], b['rows']):
        if len(left) != len(right) or len(left) != len(a['columns']):
            return False
        for kind, x, y in zip(a['columns'], left, right):
            if x == y:
                continue
            # Preserve exact decimal/integer values; floating formatting alone
            # (0 versus 0.0) is not a value mismatch. No tolerance hides errors.
            if kind not in ('FLOAT', 'DOUBLE') or x == 'NULL' or y == 'NULL':
                return False
            try:
                if Decimal(x) != Decimal(y):
                    return False
            except InvalidOperation:
                return False
    return True


def persistence(rust, cpp, directory):
    outcomes = []
    definition = "CREATE TABLE t(k DECIMAL(38,3) PRIMARY KEY, a UTINYINT DEFAULT 255, b USMALLINT DEFAULT 65535, c UINTEGER DEFAULT 4294967295, d UBIGINT DEFAULT 18446744073709551615, e UHUGEINT DEFAULT '340282366920938463463374607431768211455'); INSERT INTO t(k) VALUES (1.125),(-99999999999999999999999999999999999.999)"
    query = "SELECT k::VARCHAR AS k,a::VARCHAR AS a,b::VARCHAR AS b,c::VARCHAR AS c,d::VARCHAR AS d,e::VARCHAR AS e FROM t ORDER BY k"
    for label, producer in [('cpp', cpp), ('rust-checkpoint', rust), ('rust-wal', Engine(rust.binary, True, ('--durability', 'wal')))]:
        result = {'producer': label, 'passed': False}
        outcomes.append(result)
        try:
            path = directory / (label+'.duckdb')
            command(producer, path, definition)
            result['checkpoint_sha256'] = digest(path)
            wal = Path(str(path)+'.wal')
            if wal.exists():
                result['wal_sha256'] = digest(wal)
            expected = command(cpp, path, query, json_output=True, readonly=True)
            actual = command(rust, path, query, json_output=True, readonly=True)
            if actual != expected:
                raise AssertionError({'cpp': expected, 'rust': actual})
            command(rust, path, "UPDATE t SET k=2.25 WHERE k=1.125; INSERT INTO t(k) VALUES (3.375)")
            expected = command(cpp, path, query, json_output=True, readonly=True)
            actual = command(rust, path, query, json_output=True, readonly=True)
            if actual != expected or len(actual) != 3:
                raise AssertionError({'cpp': expected, 'rust': actual})
            command(cpp, path, "UPDATE t SET k=4.5 WHERE k=2.25; CHECKPOINT")
            expected = command(cpp, path, query, json_output=True, readonly=True)
            actual = command(rust, path, query, json_output=True, readonly=True)
            if actual != expected:
                raise AssertionError({'cpp': expected, 'rust': actual})
            result.update(passed=True, final_rows=actual)
        except Exception as error:
            result['error'] = str(error)
    return outcomes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('preserve earlier evidence; choose a new report')
    before = source_fingerprint()
    build = ['cargo','build','--offline','--release','--no-default-features','--bin','duckdb-rust','--bin','duckdb-rust-test-worker']
    subprocess.run(build, cwd=ROOT, check=True)
    worker = ROOT/'target/release/duckdb-rust-test-worker'
    rust = Engine(ROOT/'target/release/duckdb-rust',True)
    report = {'recorded_at':datetime.now(timezone.utc).isoformat(), 'source_sha256':before, 'build_command':build,
              'rust_worker_sha256':digest(worker), 'rust_cli_sha256':digest(rust.binary), 'script_sha256':digest(Path(__file__)),
              'targets':[], 'full_parity':False, 'scope':'Selected typed numeric SQL plus native checkpoint/WAL/default/index round trips. Error cases check the declared category, not full diagnostic text; full messages are retained. Development governs reference disagreements. This is not full numeric or engine parity.'}
    for target, selected in TARGETS.items():
        trial = {'target':target, 'sql':[], 'persistence':[], 'passed':False}
        report['targets'].append(trial)
        try:
            require_checkout(selected.source,target)
            cpp_path, trial['reference_identity'] = require_reference(target=target)
            library = selected.build/'src'/('libduckdb.dylib' if platform.system()=='Darwin' else 'libduckdb.so')
            reference = ROOT/f'target/numeric-reference-{target}'
            compile_command = ['c++','-std=c++17','-O3','-DNDEBUG','-I'+str(selected.source/'src/include'),str(ROOT/'test/runner/reference.cpp'),str(library),'-Wl,-rpath,'+str(library.parent),'-o',str(reference)]
            subprocess.run(compile_command,check=True)
            trial.update(compile_command=compile_command,cpp_library_sha256=digest(library),cpp_worker_sha256=digest(reference))
            with tempfile.TemporaryDirectory(prefix='ddb-numeric-reference-') as scratch:
                cpp = CppEngine(reference,scratch,time.monotonic()+120)
                actual = RustEngine(worker,scratch,time.monotonic()+120)
                try:
                    if not selected.revision.startswith(cpp.identity['source_id']):
                        raise ValueError('loaded reference library identity differs')
                    trial['rust_adapters'] = actual.request({'operation':'describe'})
                    for sql, expected_error in [(sql, None) for sql in SQL] + ERROR_CASES:
                        request = {'operation':'query','sql':sql}
                        a,b = actual.request(request),cpp.request(request)
                        trial['sql'].append({'sql':sql,'expected_development_error':expected_error,'rust':a,'cpp':b,'passed':equivalent(a,b,expected_error)})
                finally:
                    actual.close()
                    cpp.close()
                trial['persistence'] = persistence(rust,Engine(cpp_path,False,serialize_json_rows=selected.serialize_json_rows),Path(scratch))
            trial['passed'] = all(r['passed'] for r in trial['sql']+trial['persistence'])
        except Exception as error:
            trial['error'] = str(error)
    report['source_unchanged'] = before == source_fingerprint()
    targets = {trial['target']: trial for trial in report['targets']}
    report['development_sql_passed'] = (report['source_unchanged']
        and len(targets['development']['sql']) == len(SQL) + len(ERROR_CASES)
        and all(case['passed'] for case in targets['development']['sql']))
    report['reference_divergences'] = []
    for release, development in zip(targets['release']['sql'], targets['development']['sql']):
        if release['sql'] != development['sql']:
            raise ValueError('reference case identities differ')
        if not release['passed'] and development['passed'] and release['rust'] == development['rust']:
            report['reference_divergences'].append({'sql': release['sql'], 'authority': 'development',
                'release': release['cpp'], 'development': development['cpp'], 'rust': development['rust']})
    report['passed'] = report['source_unchanged'] and all(t['passed'] for t in report['targets'])
    args.report.parent.mkdir(parents=True,exist_ok=True)
    args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({'passed':report['passed'],'report':str(args.report),'targets':[{'target':t['target'],'sql_failures':[r for r in t['sql'] if not r['passed']],'persistence_failures':[r for r in t['persistence'] if not r['passed']],'error':t.get('error')} for t in report['targets']]}))
    raise SystemExit(0 if report['passed'] else 1)


if __name__ == '__main__':
    main()
