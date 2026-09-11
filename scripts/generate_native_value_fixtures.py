"""Pinned C++ raw Value metadata fixtures, independent of parsed DEFAULT trees.

The tiny helper links each already-built reference library; it does not rebuild
DuckDB. Hex payloads preserve floating bits, declared widths and typed NULLs.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from reference_version import ROOT, TARGETS, require_checkout, require_reference


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compile_helper(target, directory):
    reference = TARGETS[target]
    require_checkout(reference.source, target)
    _, identity = require_reference(target=target)
    library = reference.build / 'src/libduckdb.dylib'
    output = Path(directory) / ('value-' + target)
    command = ['c++', '-std=c++17', '-DNDEBUG', '-O0', '-I', str(reference.source / 'src/include'),
               str(ROOT / 'scripts/native_value_reference.cpp'), '-L', str(library.parent),
               '-lduckdb', '-Wl,-rpath,' + str(library.parent), '-o', str(output)]
    if target == 'release':
        command.insert(1, '-DNATIVE_VALUE_RELEASE')
    compiled = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=60)
    if compiled.returncode:
        raise RuntimeError(compiled.stderr)
    loaded = subprocess.check_output([str(output), 'identity', '-'], text=True, timeout=10).splitlines()
    if loaded != ['v' + reference.version, reference.revision[:10]]:
        raise RuntimeError(f'Linked library identity differs from pin: {loaded}')
    identity.update(library_path=str(library), library_sha256=digest(library),
                    loaded_library_version=loaded[0], loaded_library_source_id=loaded[1],
                    helper_sha256=digest(output), helper_source_sha256=digest(ROOT / 'scripts/native_value_reference.cpp'),
                    helper_compile_command=command)
    return output, identity


def oracle(helper, mode, version, input):
    result = subprocess.run([str(helper), mode, version], input=input, capture_output=True, text=True, timeout=20)
    if result.returncode:
        raise AssertionError(result.stderr)
    type_hex, wire_hex, content_hex = result.stdout.splitlines()
    return {'type': bytes.fromhex(type_hex).decode(), 'wire_hex': wire_hex, 'content_hex': content_hex}


CASES = [
    ('null_list', '[NULL,NULL]'),
    ('empty_list', '[]'),
    ('typed_null', 'NULL::STRUCT(a DECIMAL(12,2), b TIMETZ[])'),
    ('decimal_widths', "[{'s': 1.2::DECIMAL(4,1), 'i': -3.45::DECIMAL(9,2), 'b': 9.876::DECIMAL(18,3), 'h': 12345678901234567890.1::DECIMAL(38,1)}, NULL]"),
    ('integers', "{'i8': '-128'::TINYINT, 'i16': '-32768'::SMALLINT, 'i32': '-2147483648'::INTEGER, 'i64': '-9223372036854775808'::BIGINT, 'i128': '-170141183460469231731687303715884105728'::HUGEINT, 'u8':255::UTINYINT, 'u16':65535::USMALLINT, 'u32':4294967295::UINTEGER, 'u64':18446744073709551615::UBIGINT, 'u128':'340282366920938463463374607431768211455'::UHUGEINT}"),
    ('floats', "{'f': ['NaN'::FLOAT, '-0.0'::FLOAT, 'infinity'::FLOAT], 'd': ['NaN'::DOUBLE, '-0.0'::DOUBLE, '-infinity'::DOUBLE]}"),
    ('strings', "{'s': ['🦆', '', chr(0), NULL], 'b': [from_hex('005c222780ff'), NULL], 'uuid': 'ffffffff-ffff-ffff-ffff-ffffffffffff'::UUID, 'bit': '100000001'::BIT, 'n': (-0.5::DOUBLE)::BIGNUM, 'huge': '340282366920938463463374607431768211456'::BIGNUM, 'e': 'A'::ENUM('', 'A', 'a')}"),
    ('temporal', "{'d': [DATE '-infinity', DATE '2000-01-01', NULL], 't': TIME '24:00:00', 'z': [TIMETZ '00:00:00+00', TIMETZ '12:34:56+05:30', TIMETZ '23:59:59-15:59:59'], 's': '2000-01-01'::TIMESTAMP_S, 'ms': '2000-01-01'::TIMESTAMP_MS, 'us': '2000-01-01'::TIMESTAMP, 'ns': '2000-01-01 00:00:00.123456789'::TIMESTAMP_NS, 'tz': '2000-01-01 00:00:00+00'::TIMESTAMPTZ, 'i': INTERVAL '1 month -2 days 3 microseconds'}"),
    ('array', "[{'a': 1.25::DECIMAL(8,2)}, NULL]::STRUCT(a DECIMAL(8,2))[2]"),
    ('map', "map(['','A','a'], [[1,NULL],[],NULL])"),
    ('union', "[union_value(i:=42)::UNION(i INTEGER,s VARCHAR), union_value(s:=NULL::VARCHAR)::UNION(i INTEGER,s VARCHAR), NULL]"),
]
VARIANTS = [
    ('variant_null', 'NULL::VARIANT'),
    ('variant_decimal', "12345678901234567890.12::DECIMAL(38,2)::VARIANT"),
    ('variant_object', "map(['','A','a'], [1,NULL,3])::VARIANT"),
    ('variant_nested', "{'v': ({'d': 1.25::DECIMAL(8,2), 'l': [TIMESTAMP_NS '2000-01-01 00:00:00.123456789',NULL]})::VARIANT, 'v2': [(-0.5::DOUBLE)::BIGNUM::VARIANT, '1001'::BIT::VARIANT, NULL]}"),
    ('variant_array', "[1::UTINYINT::VARIANT, 'x'::VARIANT, NULL, union_value(i:=NULL)::VARIANT]::VARIANT"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError('Preserve previous evidence; use a new output path')
    fixture = {'recorded_at': datetime.now(timezone.utc).isoformat(), 'scope': __doc__, 'references': {}, 'cases': []}
    with tempfile.TemporaryDirectory(prefix='native-value-producers-') as directory:
        for target in ('development', 'release'):
            helper, identity = compile_helper(target, directory)
            fixture['references'][target] = identity
            groups = [(64, 'v1.0.0', CASES), (65, 'v1.2.0', CASES), (68, 'v1.5.0', CASES + VARIANTS)]
            if target == 'development':
                groups += [(69, 'v2.0.0', [('tuple', "(1::UTINYINT, [NULL,1.2::DECIMAL(12,2)], 'x')"),
                                         ('empty_tuple', 'row()'), ('empty_struct', "{}"),
                                         ('nested_tuple', "[row(),NULL]"),
                                         ('nanoseconds', "{'t':'24:00:00'::TIME_NS,'tz':'2000-01-01 00:00:00.123456789+00'::TIMESTAMPTZ_NS}")])]
            for number, version, cases in groups:
                for name, expression in cases:
                    print(target, version, name, flush=True)
                    result = oracle(helper, 'encode', version, expression)
                    reread = oracle(helper, 'decode', version, result['wire_hex'])
                    if reread != result:
                        raise AssertionError({'case': name, 'write': result, 'reread': reread})
                    fixture['cases'].append(dict(producer=target, version=number, compatibility=version,
                                                 name=name, expression=expression, **result))
    args.output.write_text(json.dumps(fixture, indent=2, ensure_ascii=False) + '\n')
    print(f"Retained {len(fixture['cases'])} independently reread native Value fixtures: {args.output}")


if __name__ == '__main__':
    main()
