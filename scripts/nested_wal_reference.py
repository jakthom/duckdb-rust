"""Selected nested WAL interoperability; correctness only, not a benchmark."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from generate_wal_fixtures import CASES
from reference_version import TARGETS, require_reference
from session_reference import source_fingerprint
from verify_reference import Engine, command

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def pair(path):
    wal = Path(str(path) + '.wal')
    return digest(path), digest(wal) if wal.exists() else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust', type=Path, default=ROOT/'target/debug/duckdb-rust')
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.report.exists():
        raise FileExistsError('Preserve previous evidence; choose a new report path')
    before = source_fingerprint()
    rust = Engine(args.rust, True)
    wal_writer = Engine(args.rust, True, ('--durability','wal'))
    report = {'recorded_at': datetime.now(timezone.utc).isoformat(), 'source_sha256': before,
              'rust_binary_sha256': digest(args.rust), 'script_sha256': digest(Path(__file__)),
              'rustc': subprocess.check_output(['rustc','--version'],text=True).strip(),
              'scope': 'Selected nested WAL/rollback/checkpoint/reopen with independent C++ readers and writers. No nested child-update-path, TUPLE/VARIANT native, performance, or full parity claim.',
              'targets': [], 'full_parity': False}
    setup, transactions, query = CASES['nested']
    with tempfile.TemporaryDirectory(prefix='nested-wal-reference-') as directory:
        for target in TARGETS:
            binary, identity = require_reference(target=target)
            cpp = Engine(binary,False,serialize_json_rows=False)
            trial = {'target': target, 'reference_identity': identity, 'cases': []}
            report['targets'].append(trial)
            for producer in ['rust-wal','cpp-checkpoint-then-rust-wal']:
                result = {'producer':producer,'passed':False}
                trial['cases'].append(result)
                try:
                    path=Path(directory)/f'{target}-{producer}.duckdb'
                    command(wal_writer if producer=='rust-wal' else cpp,path,setup+';'+ ';'.join(transactions))
                    command(wal_writer,path,"BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET l=[] WHERE i=0")
                    result['pair_sha256']=pair(path)
                    actual=command(rust,path,query,json_output=True,readonly=True)
                    expected=command(cpp,path,query,json_output=True,readonly=True)
                    if actual != expected:
                        raise AssertionError({'cpp':expected,'rust':actual})
                    if pair(path)!=tuple(result['pair_sha256']):
                        raise AssertionError('Read-only recovery changed durable bytes')
                    command(cpp,path,"INSERT INTO t VALUES(77,NULL,NULL,NULL,NULL,NULL); CHECKPOINT")
                    actual=command(rust,path,query,json_output=True,readonly=True)
                    expected=command(cpp,path,query,json_output=True,readonly=True)
                    if actual != expected or len(actual)!=3:
                        raise AssertionError({'cpp':expected,'rust':actual})
                    result.update(passed=True,final_rows=actual)
                except Exception as error:
                    result['error']=str(error)
    report['source_unchanged']=source_fingerprint()==before
    report['passed']=report['source_unchanged'] and all(case['passed'] for target in report['targets'] for case in target['cases'])
    args.report.write_text(json.dumps(report,indent=2,ensure_ascii=False)+'\n')
    print(json.dumps(report,indent=2,ensure_ascii=False))
    raise SystemExit(0 if report['passed'] else 1)


if __name__=='__main__':
    main()
