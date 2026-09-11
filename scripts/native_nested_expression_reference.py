"""Generate independent pinned raw ParsedExpression fixtures without evaluation.

These are codec/binding prerequisites, not native persisted DEFAULT acceptance.
All helper builds and subprocesses are local; the C++ checkout remains unchanged.
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
    helper = Path(directory) / ('nested-expression-' + target)
    source = ROOT / 'scripts/native_nested_expression_reference.cpp'
    command = ['c++', '-std=c++17', '-DNDEBUG', '-O0', '-I', str(reference.source / 'src/include'),
               str(source), '-L', str(library.parent), '-lduckdb',
               '-Wl,-rpath,' + str(library.parent), '-o', str(helper)]
    if target == 'release':
        command.insert(1, '-DNESTED_EXPRESSION_RELEASE')
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=60)
    if result.returncode:
        raise RuntimeError(result.stderr)
    loaded = subprocess.check_output([str(helper), 'identity', '-'], text=True, timeout=10).splitlines()
    if loaded != ['v' + reference.version, reference.revision[:10]]:
        raise RuntimeError(f'Linked library differs from pin: {loaded}')
    identity.update(library_path=str(library), library_sha256=digest(library),
                    loaded_library_version=loaded[0], loaded_library_source_id=loaded[1],
                    helper_sha256=digest(helper), helper_source_sha256=digest(source),
                    helper_compile_command=command)
    return helper, identity


def oracle(helper, mode, version, payload):
    result = subprocess.run([str(helper), mode, version], input=payload,
                            capture_output=True, text=True, timeout=20)
    if result.returncode:
        return {'error': result.stderr.strip()}
    wire, display, *inventory = result.stdout.splitlines()
    return {'wire_hex': wire, 'display': bytes.fromhex(display).decode(), 'inventory': inventory}


CASES = [
    ('list', '[1,NULL,1::TINYINT]'),
    ('array_keyword', 'ARRAY[1,NULL,1::TINYINT]'),
    ('array_call', 'array_value(1,NULL,1::TINYINT)'),
    ('array_cast', '[1,NULL]::INTEGER[2]'),
    ('empty_list', '[]'),
    ('struct', "{'Amount':NULL::DECIMAL(12,2),'Clock':TIMESTAMP_NS '2000-01-01 00:00:00.123456789','Items':[1,NULL]}"),
    ('struct_call', 'struct_pack(Amount:=NULL::DECIMAL(12,2),Items:=[1,NULL])'),
    ('empty_struct', '{}'),
    ('tuple', '(1,NULL::DECIMAL(12,2))'),
    ('row_call', 'row(1,NULL::DECIMAL(12,2))'),
    ('empty_row', 'row()'),
    ('map_literal', "MAP {'a':[1,NULL],'b':NULL::INTEGER[]}"),
    ('map_call', "map(['a','b'],[[1,NULL],NULL::INTEGER[]])"),
    ('empty_map', 'MAP {}'),
    ('union', 'union_value(Amount:=NULL::DECIMAL(12,2))::UNION(Amount DECIMAL(12,2),Items INTEGER[])'),
    ('variant', "{'Amount':NULL::DECIMAL(12,2),'Items':[1,NULL]}::VARIANT"),
    ('typed_null', 'NULL::STRUCT(Amount DECIMAL(12,2),Items INTEGER[])'),
    ('list_index', '[1,NULL][2]'),
    ('struct_field', "({'Amount':NULL::DECIMAL(12,2)}).Amount"),
    ('struct_index', "({'Amount':NULL::DECIMAL(12,2)})['Amount']"),
    ('map_index', "map(['a'],[[1,NULL]])['a'][2]"),
    ('union_field', '(union_value(Amount:=NULL::DECIMAL(12,2))).Amount'),
    ('variant_field', "({'Amount':NULL::DECIMAL(12,2)}::VARIANT).Amount"),
    ('tuple_index', '(1,NULL::DECIMAL(12,2))[2]'),
    ('compound', '(struct_pack(Items:=[1,NULL])).Items[2]'),
    ('qualified_base', 't.s.Items[2]'),
    ('quoted_base', '"t.q"."s.x"."Items.y"[2]'),
    ('qualified_call', 'main.struct_pack(Amount:=1)'),
    ('failing_child_not_evaluated', "struct_pack(Amount:=CAST('bad' AS INTEGER))"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError('Preserve prior evidence; choose a new output path')
    evidence = {'generated_at': datetime.now(timezone.utc).isoformat(),
                'purpose': 'Unevaluated raw parsed nested expressions; no DEFAULT publication claim',
                'references': {}, 'fixtures': []}
    source_files = ['src/planner/binder/stored.rs', 'src/planner/binder/nested.rs',
                    'src/function/nested/constructor.rs', 'src/catalog/expression.rs']
    before = {path: digest(ROOT / path) for path in source_files}
    with tempfile.TemporaryDirectory(prefix='nested-expression-', dir=ROOT / 'target') as directory:
        for target in ['development', 'release']:
            helper, identity = compile_helper(target, directory)
            evidence['references'][target] = identity
            versions = [('64', 'v1.2.0'), ('65', 'v1.3.0'), ('68', 'v1.5.0')]
            if target == 'development':
                versions.append(('69', 'v2.0.0'))
            for version, compatibility in versions:
                for name, sql in CASES:
                    parsed = oracle(helper, 'parse', compatibility, sql)
                    item = {'producer': target, 'version': int(version), 'compatibility': compatibility,
                            'name': name, 'sql': sql, 'parsed': parsed}
                    if 'wire_hex' in parsed:
                        decoded = oracle(helper, 'decode', compatibility, parsed['wire_hex'])
                        item['decoded'] = decoded
                        if 'wire_hex' in decoded:
                            repeated = oracle(helper, 'decode', compatibility, decoded['wire_hex'])
                            item['second_decode'] = repeated
                            item['stable_after_decode'] = decoded == repeated
                        else:
                            item['stable_after_decode'] = False
                    evidence['fixtures'].append(item)
    evidence['source_before'] = before
    evidence['source_after'] = {path: digest(ROOT / path) for path in source_files}
    evidence['source_unchanged'] = evidence['source_before'] == evidence['source_after']
    evidence['summary'] = {'cases': len(evidence['fixtures']),
                           'parsed': sum('wire_hex' in row['parsed'] for row in evidence['fixtures']),
                           'stable_decode_cycles': sum(row.get('stable_after_decode', False) for row in evidence['fixtures'])}
    args.output.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(evidence['summary']))
    if not evidence['source_unchanged'] or any('wire_hex' in row['parsed'] and not row.get('stable_after_decode') for row in evidence['fixtures']):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
