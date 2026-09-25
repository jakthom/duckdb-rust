"""Generate independently acknowledged native VARIANT WAL fixtures; no timing."""
import argparse
import gzip
import json
from pathlib import Path
import subprocess
import tempfile

from generate_wal_fixtures import digest, reference, send_ready
from reference_version import TARGETS, require_reference

SCALARS = [
    "true", "false", "'-128'::TINYINT", "'-32768'::SMALLINT", "'-2147483648'::INTEGER",
    "'-9223372036854775808'::BIGINT", "'-170141183460469231731687303715884105728'::HUGEINT",
    "255::UTINYINT", "65535::USMALLINT", "4294967295::UINTEGER", "18446744073709551615::UBIGINT",
    "'340282366920938463463374607431768211455'::UHUGEINT", "'-0.0'::FLOAT", "'nan'::DOUBLE",
    "1.2::DECIMAL(4,1)", "1.2::DECIMAL(9,1)", "1.2::DECIMAL(18,1)", "1.2::DECIMAL(38,1)",
    "'🦆'", "from_hex('610062')", "'ffffffff-ffff-ffff-ffff-ffffffffffff'::UUID",
    "DATE '-infinity'", "TIME '24:00:00'", "'23:59:59.123456789'::TIME_NS",
    "'2000-01-01'::TIMESTAMP_S", "'2000-01-01'::TIMESTAMP_MS", "'2000-01-01'::TIMESTAMP",
    "'2000-01-01 00:00:00.123456789'::TIMESTAMP_NS", "'24:00:00+05:30'::TIMETZ",
    "'2000-01-01 00:00:00+00'::TIMESTAMPTZ", "INTERVAL '1 month -2 days 3 microseconds'",
    "(-0.5::DOUBLE)::BIGNUM", "'340282366920938463463374607431768211456'::BIGNUM", "'101010101'::BIT",
    "'red'::ENUM('red','blue')", "[1,NULL,2]", "[1,NULL]::INTEGER[2]",
    "{'d':1.25::DECIMAL(12,2),'l':[TIMESTAMP_NS '2000-01-01 00:00:00.123456789',NULL]}",
    "map(['x','y'],[[1,NULL],[]])", "union_value(i:=1)", "union_value(i:=NULL)", "NULL",
]
SETUP = "CREATE TABLE t(i INTEGER PRIMARY KEY,v VARIANT,s STRUCT(v VARIANT),l VARIANT[]); CHECKPOINT;"
QUERY = "SELECT i,v::VARCHAR v,variant_typeof(v) tag,s.v::VARCHAR s,l[1]::VARCHAR l FROM t ORDER BY i"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', choices=TARGETS, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    executable, identity = require_reference(target=args.target)
    if args.output_dir.exists():
        raise FileExistsError('Preserve old fixture evidence; choose a new directory')
    args.output_dir.mkdir(parents=True)
    manifest = {'reference_identity': identity, 'cases': {}, 'full_parity': False}
    for version in (['v1.5.0', 'v2.0.0'] if args.target == 'development' else ['v1.5.0']):
        name = 'variant_' + version.replace('.', '_')
        expressions = SCALARS + (["(1,'a',NULL)", "'2000-01-01 00:00:00.123456789+00'::TIMESTAMPTZ_NS"] if version == 'v2.0.0' else [])
        insert = 'INSERT INTO t ' + ' UNION ALL '.join(
            f"SELECT {i},({expression})::VARIANT,{{'v':({expression})::VARIANT}},[({expression})::VARIANT,NULL]"
            for i, expression in enumerate(expressions)) + ';'
        transactions = [insert,
            "BEGIN; UPDATE t SET v={'changed':2.50::DECIMAL(12,2)}::VARIANT WHERE i=0; DELETE FROM t WHERE i=1; COMMIT;",
            "BEGIN; UPDATE t SET v=NULL,s=NULL,l=NULL WHERE i=0; ROLLBACK;",
            "INSERT INTO t SELECT 1000+i,42::VARIANT,{'v':42::VARIANT},[42::VARIANT,NULL] FROM range(257) r(i);",
        ]
        with tempfile.TemporaryDirectory(prefix='variant-wal-reference-') as directory:
            path = Path(directory)/'fixture.duckdb'
            process = subprocess.Popen([executable, ':memory:', '-json'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                send_ready(process, f"ATTACH '{path}' AS fixture (STORAGE_VERSION '{version}'); USE fixture;")
                send_ready(process, SETUP)
                baseline = path.read_bytes()
                points = [0]
                for sql in transactions:
                    send_ready(process, sql)
                    points.append(Path(str(path)+'.wal').stat().st_size)
                process.kill()
                process.wait(timeout=10)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
                for pipe in [process.stdin,process.stdout,process.stderr]:
                    pipe.close()
            checkpoint, log_path = path.read_bytes(), Path(str(path)+'.wal')
            assert checkpoint == baseline, 'unexpected checkpoint'
            log = log_path.read_bytes()
            states = []
            for end in points:
                log_path.write_bytes(log[:end])
                states.append({'end':end, 'rows':reference(executable,path,QUERY,plain=True)})
                assert path.read_bytes() == checkpoint and log_path.read_bytes() == log[:end]
            for suffix,data in [('duckdb',checkpoint),('wal',log)]:
                (args.output_dir/f'{name}.{suffix}.gz').write_bytes(gzip.compress(data,mtime=0))
            manifest['cases'][name] = {'storage_version':version,'setup':SETUP,'transactions':transactions,
                'query':QUERY,'checkpoint_sha256':digest(checkpoint),'wal_sha256':digest(log),'states':states}
            print(f'{name}: {len(checkpoint)} checkpoint bytes, {len(log)} WAL bytes')
    (args.output_dir/'manifest.json').write_text(json.dumps(manifest,indent=2,ensure_ascii=False)+'\n')


if __name__ == '__main__':
    main()
