"""Independent timestamp MIN API/native fixtures and exact SQL error witnesses."""
import argparse
from datetime import datetime, timezone
import gzip
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import time

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import CppEngine, source_fingerprint
from run_upstream import RustEngine
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


MINIMUM = "time_bucket(INTERVAL '4us',make_timestamp(-9223372036854775806))"
SQL = [
    "SELECT epoch_us(make_timestamp('-9223372036854775808'::BIGINT))",
    "SELECT epoch_ns(make_timestamp_ns('-9223372036854775808'::BIGINT))",
    f"SELECT epoch_us({MINIMUM})",
    f"SELECT {MINIMUM}",
    "SELECT make_timestamp_ns('-9223372036854775808'::BIGINT)",
    f"SELECT epoch_us(time_bucket(INTERVAL '4us',t)) FROM (VALUES (make_timestamp(-9223372036854775806)))x(t)",
]
for expression in (
    "isfinite(t),isinf(t),epoch_us(t),t=TIMESTAMP '-infinity',t<TIMESTAMP '-infinity'",
    "epoch_us(t::TIMESTAMP)", "epoch_us(t::TIMESTAMPTZ)", "epoch_us(t::TIMESTAMP_S)",
    "epoch_us(t::TIMESTAMP_MS)", "epoch_ns(t::TIMESTAMP_NS)", "t::DATE", "t::TIME",
    "epoch_us(t+INTERVAL '1 day')", "epoch_us((t::VARIANT)::TIMESTAMP)",
    "epoch_us({'t':t}.t)", "epoch_us([t,NULL][1])", "CAST(t AS VARCHAR)",
    "TRY_CAST(t AS VARCHAR)", "CAST(t::VARIANT AS VARCHAR)",
    "epoch_us(date_trunc('microsecond',t))", "epoch_us(date_trunc('day',t))",
    "epoch_us(time_bucket(INTERVAL '1us',t))", "epoch_us(time_bucket(INTERVAL '4us',t))",
):
    SQL.append(f"SELECT {expression} FROM (SELECT {MINIMUM} t) q")
for expression in ("epoch_ns(t)", "epoch_us(t::TIMESTAMP)", "epoch_ns(t::TIMESTAMPTZ_NS)",
                   "t::DATE", "t::TIME", "CAST(t AS VARCHAR)", "TRY_CAST(t AS VARCHAR)"):
    SQL.append(f"SELECT {expression} FROM (SELECT make_timestamp_ns('-9223372036854775808'::BIGINT) t) q")


def native_roundtrips(binary, scratch, target):
    results = []
    columns = ['u', 's', 'ms', 'n', 'z'] + (['zn'] if target == 'development' else [])
    mutation = 'BEGIN; DELETE FROM minima WHERE id=0; UPDATE minima SET u=NULL WHERE id=5; ROLLBACK; UPDATE minima SET '
    mutation += ','.join(f'{column}=(SELECT {column} FROM minima WHERE id=0)' for column in columns)
    mutation += ' WHERE id=3'
    for from_wal in (False, True):
        source = scratch/('minima-wal.duckdb' if from_wal else 'minima.duckdb')
        for mode in ('checkpoint', 'wal'):
            case = {'from_cpp_wal': from_wal, 'rust_publication': mode, 'sql': mutation,
                    'passed': False, 'observations': []}
            results.append(case)
            try:
                destination = scratch/f'roundtrip-{from_wal}-{mode}.duckdb'
                shutil.copyfile(source, destination)
                if from_wal:
                    shutil.copyfile(str(source)+'.wal', str(destination)+'.wal')
                for stage in ('original', 'mutated'):
                    if stage == 'mutated':
                        command(Engine(ROOT/'target/debug/duckdb-rust', True, ('--durability', mode)), destination, mutation)
                    observed = subprocess.run([str(binary), '--inspect', str(destination), stage],
                                              capture_output=True, text=True)
                    case['observations'].append({'stage': stage, 'status': observed.returncode,
                                                 'stdout': observed.stdout, 'stderr': observed.stderr})
                    if observed.returncode:
                        raise RuntimeError(case['observations'][-1])
                case['passed'] = True
            except Exception as error:
                case['error'] = str(error)
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', required=True, type=Path)
    parser.add_argument('--fixtures', type=Path, help='write new compressed independent fixtures; never overwrite')
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('preserve earlier reports')
    before = source_fingerprint()
    subprocess.run(['cargo','build','--offline','--no-default-features','--bin','duckdb-rust-test-worker','--bin','duckdb-rust'],cwd=ROOT,check=True)
    worker = ROOT/'target/debug/duckdb-rust-test-worker'
    report = {'recorded_at': datetime.now(timezone.utc).isoformat(), 'source_sha256': before,
              'rust_worker_sha256': digest(worker), 'script_sha256': digest(Path(__file__)),
              'scope': 'Raw C++ timestamp Value/Appender API, checkpoint/WAL fixtures, exact SQL values/errors; no performance claim.',
              'full_parity': False, 'targets': []}
    for target, selected in TARGETS.items():
        require_checkout(selected.source,target)
        _, identity = require_reference(target=target)
        library = selected.build/'src'/('libduckdb.dylib' if platform.system()=='Darwin' else 'libduckdb.so')
        binary = ROOT/f'target/temporal-minimum-{target}'
        command = ['c++','-std=c++17','-O2','-I'+str(selected.source/'src/include'),
                   str(ROOT/'test/runner/temporal_minimum.cpp'),str(library),'-Wl,-rpath,'+str(library.parent),'-o',str(binary)]
        if target=='development':
            command.insert(1,'-DDDB_DEVELOPMENT')
        subprocess.run(command,check=True)
        reference = ROOT/f'target/temporal-minimum-sql-{target}'
        subprocess.run(['c++','-std=c++17','-O2','-I'+str(selected.source/'src/include'),str(ROOT/'test/runner/reference.cpp'),str(library),'-Wl,-rpath,'+str(library.parent),'-o',str(reference)],check=True)
        trial = {'target': target, 'reference_identity': identity, 'cpp_library_sha256':digest(library),
                 'cpp_fixture_source_sha256':digest(ROOT/'test/runner/temporal_minimum.cpp'),
                 'cpp_fixture_binary_sha256':digest(binary), 'sql':[], 'fixtures':[]}
        report['targets'].append(trial)
        with tempfile.TemporaryDirectory(prefix='ddb-temporal-minimum-') as scratch:
            scratch=Path(scratch)
            native=subprocess.run([str(binary),str(scratch/'minima.duckdb'),str(scratch/'minima-wal.duckdb')],capture_output=True,text=True)
            trial['cpp_api']={'status':native.returncode,'stdout':native.stdout,'stderr':native.stderr}
            if native.returncode:
                raise RuntimeError(trial['cpp_api'])
            for name in ('minima.duckdb','minima-wal.duckdb','minima-wal.duckdb.wal'):
                source=scratch/name
                fixture={'name':name,'sha256':digest(source),'bytes':source.stat().st_size}
                if args.fixtures:
                    args.fixtures.mkdir(parents=True,exist_ok=True)
                    destination=args.fixtures/f'temporal-minimum-{target}-{name}.gz'
                    if destination.exists():
                        raise FileExistsError(destination)
                    destination.write_bytes(gzip.compress(source.read_bytes(),mtime=0))
                    fixture['archive']=str(destination.relative_to(ROOT) if destination.is_absolute() else destination)
                    fixture['archive_sha256']=digest(destination)
                trial['fixtures'].append(fixture)
            trial['native_roundtrips'] = native_roundtrips(binary, scratch, target)
            cpp=CppEngine(reference,scratch,time.monotonic()+120)
            rust=RustEngine(worker,scratch,time.monotonic()+120)
            try:
                if not selected.revision.startswith(cpp.identity['source_id']):
                    raise ValueError('loaded reference differs from pin')
                for sql in SQL:
                    # Fatal renderer witnesses can invalidate a C++ database;
                    # every case gets an independent session database.
                    for engine in (rust,cpp):
                        if not engine.request({'operation':'load'}).get('ok'):
                            raise RuntimeError('minimum witness reset failed')
                    a,b=(engine.request({'operation':'query','sql':sql}) for engine in (rust,cpp))
                    keys=('ok','columns','rows') if a.get('ok') and b.get('ok') else ('ok','message')
                    exact=all(a.get(key)==b.get(key) for key in keys)
                    trial['sql'].append({'sql':sql,'rust':a,'cpp':b,'passed':exact,
                                         'outcome_passed':exact or (not a.get('ok') and not b.get('ok'))})
            finally:
                rust.close()
                cpp.close()
    if source_fingerprint()!=before:
        raise RuntimeError('source changed during minimum campaign')
    report['passed']=all(case['passed'] for trial in report['targets'] for case in trial['sql']+trial['native_roundtrips'])
    args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({trial['target']:{'exact':sum(case['passed'] for case in trial['sql']),
                                     'outcomes':sum(case['outcome_passed'] for case in trial['sql']),
                                     'total':len(trial['sql']),'cpp_api':trial['cpp_api'],
                                     'native_passed':sum(case['passed'] for case in trial['native_roundtrips']),
                                     'native_total':len(trial['native_roundtrips'])} for trial in report['targets']}))
    return 0 if report['passed'] else 1


if __name__=='__main__':
    raise SystemExit(main())
