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
for source in ('FLOAT', 'DOUBLE'):
    for target in ('TINYINT','SMALLINT','INTEGER','BIGINT','HUGEINT','UTINYINT','USMALLINT','UINTEGER','UBIGINT','UHUGEINT'):
        SQL.append(f"SELECT ('0.5'::{source})::{target},('1.5'::{source})::{target},('2.5'::{source})::{target},('3.5'::{source})::{target},TRY_CAST('-0.5'::{source} AS {target}),TRY_CAST('-3.5'::{source} AS {target})")
    SQL.append(f"SELECT TRY_CAST('127.4'::{source} AS TINYINT),TRY_CAST('127.5'::{source} AS TINYINT),TRY_CAST('127.6'::{source} AS TINYINT),TRY_CAST('-128.4'::{source} AS TINYINT),TRY_CAST('-128.5'::{source} AS TINYINT),TRY_CAST('-128.6'::{source} AS TINYINT),TRY_CAST('255.4'::{source} AS UTINYINT),TRY_CAST('255.5'::{source} AS UTINYINT),TRY_CAST('255.6'::{source} AS UTINYINT)")
    SQL.append(f"SELECT TRY_CAST('-170141183460469231731687303715884105728'::{source} AS HUGEINT),TRY_CAST('170141183460469231731687303715884105728'::{source} AS HUGEINT),TRY_CAST('340282366920938463463374607431768211456'::{source} AS UHUGEINT),('2.5'::{source})::DECIMAL(2,0),round('2.5'::{source})")
ERROR_CASES = [(f'SELECT -1::{kind}', 'Out of Range Error')
               for kind in ('UTINYINT', 'USMALLINT', 'UINTEGER', 'UBIGINT', 'UHUGEINT')]
ERROR_CASES += [(f'SELECT 1::{kind}{op}0::{kind}', 'Invalid Input Error')
                for kind in ('UTINYINT', 'USMALLINT', 'UINTEGER', 'UBIGINT', 'UHUGEINT') for op in ('//', '%')]
ERROR_CASES += [('SELECT 1.0%0.0', 'Invalid Input Error'),
                ("SELECT 'a'::DECIMAL(5,2)", 'Conversion Error'),
                ('SELECT 256::UTINYINT', 'Conversion Error')]

# Integral-direction overloads span every decimal storage transition. Values
# stay textual and exact; no Python floating-point conversion builds decimals.
for width in (1, 4, 5, 9, 10, 18, 19, 38):
    for scale in range(width + 1):
        magnitude = '9' * width
        if scale:
            magnitude = (magnitude[:-scale] or '0') + '.' + magnitude[-scale:]
        kind = f'DECIMAL({width},{scale})'
        SQL.append(f"SELECT x::VARCHAR,ceil(x)::VARCHAR,ceiling(x)::VARCHAR,floor(x)::VARCHAR,sign(x),typeof(ceil(x)),typeof(floor(x)),typeof(sign(x)) FROM (VALUES ('-{magnitude}'::{kind}),('0'::{kind}),('{magnitude}'::{kind}),(NULL::{kind})) t(x) ORDER BY x")
for kind in ('TINYINT','SMALLINT','INTEGER','BIGINT','HUGEINT','UTINYINT','USMALLINT','UINTEGER','UBIGINT','UHUGEINT','BIGNUM'):
    SQL.append(f"SELECT ceil(x)::VARCHAR,floor(x)::VARCHAR,sign(x),typeof(ceil(x)),typeof(floor(x)),typeof(sign(x)) FROM (VALUES (0::{kind}),(1::{kind}),(NULL::{kind})) t(x) ORDER BY x")
for kind in ('FLOAT','DOUBLE'):
    values = ','.join(f"('{value}'::{kind})" for value in ('-inf','-1.25','-1.0','-0.25','-0.0','0.0','0.25','1.0','1.25','inf','nan'))
    SQL.append(f"SELECT x::VARCHAR,ceil(x)::VARCHAR,floor(x)::VARCHAR,sign(x),typeof(ceil(x)) FROM (VALUES {values}) t(x) ORDER BY x")
SQL += [
    "SELECT ceil,floor FROM (VALUES (1,2)) t(ceil,floor)",
    "SELECT ceil(floor.x) AS floor FROM (VALUES (1.25::DECIMAL(4,2)),(-1.25)) floor(x) ORDER BY floor",
    "SELECT typeof(ceil(NULL)),typeof(floor(NULL)),typeof(sign(NULL)),ceil(NULL),sign(NULL)",
    "SELECT sign('-170141183460469231731687303715884105728'::HUGEINT),sign('340282366920938463463374607431768211455'::UHUGEINT),sign('0.00000000000000000000000000000000000001'::DECIMAL(38,38))",
    "SELECT [ceil(1.25::FLOAT),floor(-1.25::FLOAT),NULL]::VARCHAR,concat(ceil(1.25::DOUBLE)),{'x':floor(-1.25::DECIMAL(38,2))}::VARCHAR",
    "SELECT sign(x),count(*),sum(ceil(x))::VARCHAR FROM (VALUES (-1.25::DECIMAL(8,2)),(-0.01),(0),(0.01),(1.25),(NULL)) t(x) GROUP BY sign(x) ORDER BY sign(x)",
    "SELECT a.x::VARCHAR,ceil(a.x)::VARCHAR FROM (VALUES (-1.25::DECIMAL(8,2)),(1.25),(NULL)) a(x) JOIN (VALUES (-2::DECIMAL(8,0)),(1),(NULL)) b(y) ON floor(a.x)=b.y ORDER BY a.x",
    "SELECT ceil(x)::VARCHAR,lag(floor(x)) OVER(ORDER BY x)::VARCHAR,sum(sign(x)) OVER(ORDER BY x ROWS UNBOUNDED PRECEDING) FROM (VALUES (-1.25::DECIMAL(8,2)),(0),(1.25),(NULL)) t(x) ORDER BY x",
]
for name in ('ceil','ceiling','floor','sign'):
    ERROR_CASES += [(f'SELECT {name}({argument})','Binder Error') for argument in
                    ("'1.25'", "'1.25'::VARCHAR", 'TRUE', "'1.25'::ENUM('1.25')", '[1]', "DATE '2024-01-01'", '', '1,2')]
ERROR_CASES += [("SELECT floor(TIMESTAMP '2024-01-01' TO DAY)", 'Parser Error')]

# Precision rounding retains exact textual comparisons rather than accepting
# floating tolerance. The transition-width matrix includes nullable columns,
# whose coarse DECIMAL cutoffs differ from a known constant NULL expression.
for width in (1, 4, 5, 9, 10, 18, 19, 38):
    for scale in sorted({0, min(2, width), width}):
        raw = str(5 * 10 ** (width - 1) - 1)
        if scale:
            raw = (raw[:-scale] or '0') + '.' + raw[-scale:]
        kind = f'DECIMAL({width},{scale})'
        for precision in sorted({-2147483648, -40, -width, -1, 0, 1, scale, 40, 2147483647}):
            columns = ','.join(f"{name}(x,{precision})::VARCHAR,typeof({name}(x,{precision}))"
                               for name in ('round', 'trunc', 'round_even', 'roundbankers'))
            SQL.append(f"SELECT x::VARCHAR,{columns} FROM (VALUES ('-{raw}'::{kind}),('0'::{kind}),('{raw}'::{kind}),(NULL::{kind})) t(x) ORDER BY x")
for kind, maximum in (('TINYINT',127),('SMALLINT',32767),('INTEGER',2147483647),
                      ('BIGINT',9223372036854775807),('HUGEINT',2**127-1),
                      ('UTINYINT',255),('USMALLINT',65535),('UINTEGER',2**32-1),
                      ('UBIGINT',2**64-1),('UHUGEINT',2**128-1)):
    for precision in (-2147483648,-39,-38,-19,-18,-1,0,1,2147483647):
        # Small values avoid intentionally failing whole queries on carry;
        # maximum-value truncation and explicit overflow cases are separate.
        columns = ','.join(f"{name}(x,{precision})::VARCHAR,typeof({name}(x,{precision}))"
                           for name in ('round','trunc','round_even','roundbankers'))
        SQL.append(f"SELECT x::VARCHAR,{columns},trunc('{maximum}'::{kind},{precision})::VARCHAR FROM (VALUES (0::{kind}),(25::{kind}),(NULL::{kind})) t(x) ORDER BY x")
for kind in ('FLOAT','DOUBLE'):
    values = ','.join(f"('{value}'::{kind})" for value in
                      ('-inf','-12.55','-2.5','-1.25','-0.0','0.0','1.25','2.5','12.55','inf','nan'))
    for precision in (-2147483648,-400,-40,-2,-1,0,1,2,40,400,2147483647):
        columns = ','.join(f"{name}(x,{precision})::VARCHAR,typeof({name}(x,{precision}))"
                           for name in ('round','trunc','round_even','roundbankers'))
        SQL.append(f"SELECT x::VARCHAR,{columns} FROM (VALUES {values}) t(x) ORDER BY x")
SQL += [
    "SELECT typeof(round(NULL)),typeof(round(NULL,NULL)),typeof(round_even(NULL,NULL)),typeof(round(NULL::INTEGER,1)),typeof(round(NULL::DECIMAL(4,2),1)),typeof(trunc(NULL::DECIMAL(4,2))),typeof(round(1.25::DECIMAL(4,2),NULL)),typeof(round_even(1.25::DECIMAL(4,2),NULL::INTEGER))",
    "SELECT round(NULL::DECIMAL(4,2),i::INTEGER),typeof(round(NULL::DECIMAL(4,2),i::INTEGER)) FROM range(2) t(i)",
    "SELECT round(1.25::DECIMAL(4,2),'1')::VARCHAR,trunc(1.25::DECIMAL(4,2),'1')::VARCHAR,round_even(1.25::DECIMAL(4,2),'1')::VARCHAR",
    "SELECT typeof(round(9999::DECIMAL(4,0),-1)),round(9999::DECIMAL(4,0),-1)::VARCHAR,typeof(trunc(9999::DECIMAL(4,0),-1)),trunc(9999::DECIMAL(4,0),-1)::VARCHAR",
    "SELECT {'d':round(1.25::DECIMAL(5,2),1),'f':[round_even(1.25::FLOAT,1),NULL]}::VARCHAR,concat(round(1.25::DOUBLE,1))",
    "SELECT trunc(x,-4)::VARCHAR,count(*),sum(round(x,1))::VARCHAR FROM (VALUES (1.25::DECIMAL(5,2)),(2.25),(NULL)) t(x) GROUP BY trunc(x,-4)",
    "SELECT a.x::VARCHAR,b.x::VARCHAR FROM (VALUES (1.25::DECIMAL(5,2)),(1.35),(NULL)) a(x) JOIN (VALUES (1.3::DECIMAL(5,1)),(NULL)) b(x) ON round(a.x,1)=b.x ORDER BY a.x",
    "SELECT round(x,1)::VARCHAR,lag(round_even(x,1)) OVER(ORDER BY x)::VARCHAR,sum(round(x,1)) OVER(ORDER BY x ROWS UNBOUNDED PRECEDING)::VARCHAR FROM (VALUES (1.25::DECIMAL(5,2)),(2.25),(NULL)) t(x) ORDER BY x",
    "SELECT trunc(340282366920938463463374607431768211455::UHUGEINT,-38)",
]
for name in ('round','trunc','round_even','roundbankers'):
    SQL += [f"SELECT CASE WHEN false THEN {name}(CAST('bad' AS DECIMAL(4,2)),1) ELSE 1 END",
            f"SELECT {name}(NULL::DECIMAL(4,2),'bad'),typeof({name}(NULL::DECIMAL(4,2),'bad'))"]
    ERROR_CASES += [(f"SELECT CASE WHEN false THEN {name}(1.25::DECIMAL(4,2),'bad') ELSE 1 END", 'Invalid Input Error'),
                    (f"SELECT CASE WHEN false THEN {name}(1.25::DECIMAL(4,2),CAST('bad' AS INTEGER)) ELSE 1 END", 'Conversion Error')]
for name in ('round','trunc','round_even','roundbankers'):
    ERROR_CASES += [(f'SELECT {name}({arguments}) FROM range(2) t(i)','Binder Error') for arguments in
                    ("1.25::DECIMAL(4,2),1.5", "1.25::DECIMAL(4,2),'1'::VARCHAR",
                     '1.25::DECIMAL(4,2),i::INTEGER', '1.25::DOUBLE,i::BIGINT',
                     'TRUE,1', "'1.25',1", '1,1,1', '')]
ERROR_CASES += [(f'SELECT {name}(1)', 'Binder Error') for name in ('round_even','roundbankers')]
ERROR_CASES += [(f"SELECT {name}({value}, {precision})", 'Out of Range Error')
                for name in ('round','round_even','roundbankers')
                for value,precision in [('127::TINYINT',-1),('32767::SMALLINT',-1),
                                        ("'170141183460469231731687303715884105727'::HUGEINT",-38),
                                        ("'99999999999999999999999999999999999999'::DECIMAL(38,0)",-38)]]

# ABS keeps every declared signed/unsigned domain and the complete DECIMAL
# metadata. String casts make floating zero and NaN rendering comparisons exact.
for kind, bits in (('TINYINT',8),('SMALLINT',16),('INTEGER',32),('BIGINT',64),
                   ('HUGEINT',128),('UTINYINT',8),('USMALLINT',16),
                   ('UINTEGER',32),('UBIGINT',64),('UHUGEINT',128)):
    if kind.startswith('U'):
        values = (0,1,2**bits-1)
    else:
        minimum = -(2**(bits-1))
        values = (minimum+1,-1,0,1,-minimum-1)
        ERROR_CASES += [(f"SELECT abs('{minimum}'::{kind})", 'Out of Range Error'),
                        (f"SELECT abs(x) FROM (VALUES (1::{kind}),('{minimum}'::{kind})) t(x)", 'Out of Range Error')]
        SQL.append(f"SELECT CASE WHEN false THEN abs('{minimum}'::{kind}) ELSE 1 END")
    rows = ','.join(f"('{value}'::{kind})" for value in values) + f',(NULL::{kind})'
    SQL.append(f"SELECT x::VARCHAR,abs(x)::VARCHAR,typeof(abs(x)) FROM (VALUES {rows}) t(x) ORDER BY x")
for width in (1,4,5,9,10,18,19,38):
    for scale in sorted({0,min(2,width),width}):
        digits = '9'*width
        magnitude = ((digits[:-scale] or '0')+'.'+digits[-scale:]) if scale else digits
        kind = f'DECIMAL({width},{scale})'
        SQL.append(f"SELECT x::VARCHAR,abs(x)::VARCHAR,typeof(abs(x)) FROM (VALUES ('-{magnitude}'::{kind}),('0'::{kind}),('{magnitude}'::{kind}),(NULL::{kind})) t(x) ORDER BY x")
for kind in ('FLOAT','DOUBLE'):
    rows = ','.join(f"('{value}'::{kind})" for value in ('-inf','-1.25','-0.0','0.0','1.25','inf','nan','-nan'))
    SQL.append(f"SELECT x::VARCHAR,abs(x)::VARCHAR,typeof(abs(x)) FROM (VALUES {rows}) t(x)")
SQL += [
    "SELECT typeof(abs(NULL)),typeof(abs(NULL::DECIMAL(4,2))),typeof(abs(NULL::TINYINT)),typeof(abs(NULL::BIGNUM)),abs(NULL)",
    "SELECT abs('-1'::BIGNUM)::VARCHAR,abs('-0'::BIGNUM)::VARCHAR,typeof(abs('1'::BIGNUM))",
    "SELECT CASE WHEN false THEN abs(CAST('bad' AS DECIMAL(4,2))) ELSE 1 END",
    "SELECT [abs(-1.25::DOUBLE),NULL]::VARCHAR,{'d':abs(-1.25::DECIMAL(4,2))}::VARCHAR,concat(abs('-0.0'::FLOAT))",
    "SELECT abs(x)::VARCHAR,count(*),sum(abs(x))::VARCHAR FROM (VALUES (-1.25::DECIMAL(8,2)),(1.25),(NULL)) t(x) GROUP BY abs(x) ORDER BY abs(x)",
    "SELECT a.x::VARCHAR,b.x::VARCHAR FROM (VALUES (-1.25::DECIMAL(8,2)),(1.25),(NULL)) a(x) JOIN (VALUES (1.25::DECIMAL(8,2)),(NULL)) b(x) ON abs(a.x)=b.x ORDER BY a.x",
    "SELECT abs(x)::VARCHAR,lag(abs(x)) OVER(ORDER BY x)::VARCHAR,sum(abs(x)) OVER(ORDER BY x ROWS UNBOUNDED PRECEDING)::VARCHAR FROM (VALUES (-1.25::DECIMAL(8,2)),(0),(1.25),(NULL)) t(x) ORDER BY x",
]
ERROR_CASES += [(f'SELECT abs({argument})', 'Binder Error') for argument in
                ("'1'", "'1'::VARCHAR", 'TRUE', "'1'::ENUM('1')", '[1]', "DATE '2024-01-01'", '', '1,2')]

for literal in ('2147483647','2147483648','-2147483648','-2147483649',
                '9223372036854775807','9223372036854775808','-9223372036854775808','-9223372036854775809',
                '170141183460469231731687303715884105727','170141183460469231731687303715884105728',
                '-170141183460469231731687303715884105728','-170141183460469231731687303715884105729',
                '340282366920938463463374607431768211455','340282366920938463463374607431768211456',
                '-340282366920938463463374607431768211455','1234567890'*100):
    SQL.append(f'SELECT {literal},typeof({literal}),({literal})::VARCHAR')
for expression in (
    'CASE WHEN false THEN 0::TINYINT ELSE 1 END',
    'CASE WHEN false THEN 0::TINYINT ELSE 1::INTEGER END',
    'CASE WHEN false THEN 0::UTINYINT ELSE 255 END',
    'CASE WHEN false THEN 0::UTINYINT ELSE 256 END',
    'CASE WHEN false THEN NULL WHEN false THEN 2::TINYINT ELSE 1 END',
    'CASE WHEN false THEN 1 WHEN false THEN 2::TINYINT ELSE 1 END',
    'CASE WHEN false THEN 2::TINYINT WHEN false THEN 1 ELSE 1 END',
    'CASE WHEN false THEN 2::TINYINT ELSE NULL END',
    'CASE 3 WHEN 1 THEN 0::SMALLINT WHEN 2 THEN 1 ELSE 1 END',
    "CASE WHEN false THEN 'bad' ELSE 1::INTEGER END",
    "CASE WHEN false THEN 1::INTEGER ELSE '2' END",
):
    SQL.append(f'SELECT {expression},typeof({expression})')
ERROR_CASES += [(f'SELECT {expression}', 'Binder Error') for expression in (
    "CASE WHEN false THEN '1' WHEN false THEN 2 ELSE '1' END",
    "CASE WHEN false THEN NULL WHEN false THEN 2 ELSE '1' END",
    "CASE WHEN false THEN 1 ELSE '2'::VARCHAR END",
)]
# Originally retained wider inference failures. Keep these identities when the
# selected full-width literal/common-type follow-up repairs them.
SQL += [
    'SELECT typeof(CASE WHEN false THEN 340282366920938463463374607431768211455 ELSE 1 END)',
    'SELECT typeof(CASE WHEN false THEN 340282366920938463463374607431768211455::UHUGEINT ELSE 1::INTEGER END)',
    'SELECT typeof([340282366920938463463374607431768211455,1])',
]

# Full-width inference distinguishes generic combination from arithmetic
# overload ranking and retains casts for values outside the selected domain.
for signed in ('TINYINT','SMALLINT','INTEGER','BIGINT','HUGEINT'):
    SQL.append(f'SELECT typeof(CASE WHEN false THEN 1::UHUGEINT ELSE 1::{signed} END),typeof([1::UHUGEINT,1::{signed}]),typeof(1::UHUGEINT+1::{signed}),typeof(1::{signed}+1::UHUGEINT)')
    for value in ('170141183460469231731687303715884105728','340282366920938463463374607431768211455'):
        for expression in (value,f'{value}::UHUGEINT'):
            SQL.append(f'SELECT typeof(CASE WHEN false THEN {expression} ELSE 1::{signed} END),typeof([{expression},1::{signed}]),CASE WHEN false THEN {expression} ELSE 1::{signed} END')
            if signed != 'HUGEINT':
                ERROR_CASES += [(f'SELECT CASE WHEN true THEN {expression} ELSE 1::{signed} END','Conversion Error'),
                                (f'SELECT [{expression},1::{signed}]','Conversion Error')]
for value in ('170141183460469231731687303715884105728','340282366920938463463374607431768211455'):
    for expression in (value,f'{value}::UHUGEINT',f'CASE WHEN true THEN {value} ELSE {value} END'):
        for other in ('1','1::INTEGER','1::UHUGEINT','NULL','CASE WHEN true THEN 1 ELSE 1 END'):
            SQL.append(f'SELECT typeof(CASE WHEN false THEN ({expression}) ELSE ({other}) END),typeof([({expression}),({other})])')
SQL += [
    'SELECT typeof([340282366920938463463374607431768211455,NULL,340282366920938463463374607431768211455]),typeof([340282366920938463463374607431768211455,NULL,1])',
    "SELECT typeof([{'u':340282366920938463463374607431768211455},{'u':1::INTEGER}]),typeof(CASE WHEN false THEN {'u':340282366920938463463374607431768211455} ELSE {'u':1::INTEGER} END)",
    'SELECT typeof(MAP {340282366920938463463374607431768211455:1,1:2})',
    'SELECT CASE WHEN false THEN 340282366920938463463374607431768211455 ELSE 1 END',
    "SELECT CASE WHEN false THEN CAST('bad' AS UHUGEINT) ELSE 1::INTEGER END",
    'SELECT CASE WHEN f THEN u ELSE s END FROM (VALUES (false,340282366920938463463374607431768211455::UHUGEINT,0::INTEGER),(true,2::UHUGEINT,1::INTEGER)) t(f,u,s) ORDER BY s',
]
ERROR_CASES += [
    ('SELECT CASE WHEN true THEN 340282366920938463463374607431768211455 ELSE 1 END','Conversion Error'),
    ('SELECT [340282366920938463463374607431768211455,1]','Conversion Error'),
    ('SELECT MAP {340282366920938463463374607431768211455:1,1:2}','Conversion Error'),
    ('SELECT TRY_CAST(CASE WHEN true THEN 340282366920938463463374607431768211455 ELSE 1 END AS BIGINT)','Conversion Error'),
    ('SELECT xor(1::UHUGEINT,1::INTEGER)','Binder Error'),
]


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
    query = "SELECT k::VARCHAR AS k,a::VARCHAR AS a,b::VARCHAR AS b,c::VARCHAR AS c,d::VARCHAR AS d,e::VARCHAR AS e,ceil(k)::VARCHAR AS ceiling_value,floor(k)::VARCHAR AS floor_value,sign(k) AS sign_value,round(k,2)::VARCHAR AS rounded_value,trunc(k,1)::VARCHAR AS truncated_value,trunc(e,-38)::VARCHAR AS unsigned_truncated,abs(k)::VARCHAR AS absolute_value,abs(e)::VARCHAR AS unsigned_absolute,nullif(k,0::DECIMAL(38,3))::VARCHAR AS nullif_decimal,nullif(e,0::UHUGEINT)::VARCHAR AS nullif_unsigned FROM t ORDER BY k"
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
            command(rust, path, "BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET k=round(k,0)+1.25 WHERE nullif(k,0::DECIMAL(38,3))=1.125; INSERT INTO t(k) VALUES (3.375)")
            expected = command(cpp, path, query, json_output=True, readonly=True)
            actual = command(rust, path, query, json_output=True, readonly=True)
            if actual != expected or len(actual) != 3:
                raise AssertionError({'cpp': expected, 'rust': actual})
            command(cpp, path, "UPDATE t SET k=trunc(k,0)+2.5 WHERE k=2.25; CHECKPOINT")
            expected = command(cpp, path, query, json_output=True, readonly=True)
            actual = command(rust, path, query, json_output=True, readonly=True)
            if actual != expected:
                raise AssertionError({'cpp': expected, 'rust': actual})
            result.update(passed=True, final_rows=actual)
        except Exception as error:
            result['error'] = str(error)
    return outcomes


def math_persistence(rust, cpp, directory):
    """Carry selected math results through both producers and native mutation.

    The input operations agree in either IEEE mode so release remains an
    independent physical-file witness without changing its default setting.
    """
    definition = "CREATE TABLE ieee_math(k INTEGER PRIMARY KEY,x DOUBLE,r DOUBLE); INSERT INTO ieee_math VALUES(1,-4,sqrt(4)),(2,0,log(1)),(3,9,power(3,2)),(4,'inf'::DOUBLE,pow(1e308,2)),(5,'nan'::DOUBLE,sqrt('nan'::DOUBLE)),(6,NULL,NULL),(7,'-0.0'::DOUBLE,sqrt('-0.0'::DOUBLE))"
    query = "SELECT k,x::VARCHAR AS x,r::VARCHAR AS r,sqrt(abs(x))::VARCHAR AS root_value,ln(abs(x)+1)::VARCHAR AS ln_value,log2(abs(x)+1)::VARCHAR AS log2_value FROM ieee_math ORDER BY k"
    rust_mutation = "BEGIN; DELETE FROM ieee_math; ROLLBACK; UPDATE ieee_math SET r=pow(sqrt(abs(x)),2) WHERE k=1; INSERT INTO ieee_math VALUES(8,16,sqrt(16))"
    cpp_mutation = "UPDATE ieee_math SET r=log(2,16) WHERE k=8; CHECKPOINT"
    outcomes = []
    for label, producer in [('cpp', cpp), ('rust-checkpoint', rust), ('rust-wal', Engine(rust.binary, True, ('--durability', 'wal')))]:
        result = {'family': 'ieee-math', 'producer': label, 'passed': False,
                  'definition': definition, 'query': query,
                  'rust_mutation': rust_mutation, 'cpp_mutation': cpp_mutation}
        outcomes.append(result)
        try:
            path = directory / ('ieee-math-' + label + '.duckdb')
            command(producer, path, definition)
            result['checkpoint_sha256'] = digest(path)
            wal = Path(str(path) + '.wal')
            if wal.exists():
                result['wal_sha256'] = digest(wal)
            for stage, mutation, actor, count in [
                    ('initial', None, None, 7),
                    ('rust-mutated', rust_mutation, rust, 8),
                    ('cpp-mutated', cpp_mutation, cpp, 8)]:
                if mutation is not None:
                    command(actor, path, mutation)
                expected = command(cpp, path, query, json_output=True, readonly=True)
                actual = command(rust, path, query, json_output=True, readonly=True)
                result.setdefault('stages', []).append({'stage': stage, 'cpp': expected, 'rust': actual})
                if actual != expected or len(actual) != count:
                    raise AssertionError({'stage': stage, 'cpp': expected, 'rust': actual})
            result['passed'] = True
        except Exception as error:
            result['error'] = str(error)
    return outcomes


# Selected COALESCE is source-ordered combination, distinct from ordinary
# implicit function coercion. Keep the earlier 803 case identities unchanged.
for signed in ('TINYINT', 'SMALLINT', 'INTEGER', 'BIGINT', 'HUGEINT'):
    SQL.append(f"SELECT typeof(coalesce(1::UHUGEINT,1::{signed})),coalesce(1::UHUGEINT,1::{signed}),typeof(coalesce(NULL,1::{signed},1::UHUGEINT))")
for expression in (
    'coalesce(1,NULL,1::UHUGEINT)', 'coalesce(NULL,1,1::UHUGEINT)',
    'coalesce(1,1::UHUGEINT,NULL)', 'coalesce(1,1,1::UHUGEINT)',
    'coalesce(340282366920938463463374607431768211455,NULL,1)',
    'coalesce(340282366920938463463374607431768211455,340282366920938463463374607431768211455,1)',
    "coalesce(1::UHUGEINT,'bad'::INTEGER)", "coalesce('2',1::INTEGER)",
    'coalesce(NULL,TRUE,1::UTINYINT)', 'coalesce(NULL,NULL)',
    'coalesce([1::UHUGEINT],[1::INTEGER])', "coalesce(NULL,{'d':1.25})",
    "coalesce(NULL,TIMESTAMP '2024-01-02 03:04:05')",
    "coalesce(NULL,'x'::ENUM('x','y'))", "coalesce(NULL,'0101'::BIT)",
):
    SQL.append(f'SELECT typeof({expression}),{expression}')
SQL += [
    "SELECT coalesce(a,b::INTEGER,c) FROM (VALUES (1::UHUGEINT,'bad'::VARCHAR,2::INTEGER),(NULL,'3',4),(NULL,NULL,5))t(a,b,c)",
    'SELECT coalesce(a,b),count(*),sum(coalesce(a,b,0)) FROM (VALUES (1::UHUGEINT,2::INTEGER),(NULL,2),(NULL,NULL))t(a,b) GROUP BY coalesce(a,b) ORDER BY 1',
    'SELECT sum(coalesce(a,b,0)) OVER(ORDER BY i ROWS UNBOUNDED PRECEDING) FROM (VALUES (1,1::UHUGEINT,2::INTEGER),(2,NULL,2),(3,NULL,NULL))t(i,a,b) ORDER BY i',
]
ERROR_CASES += [(f'SELECT {expression}', 'Conversion Error') for expression in (
    'coalesce(340282366920938463463374607431768211455,1)',
    "coalesce('bad',1::INTEGER)",
    'TRY_CAST(coalesce(340282366920938463463374607431768211455,1) AS BIGINT)',
)]
ERROR_CASES += [("SELECT coalesce('1'::VARCHAR,1::INTEGER)", 'Binder Error')]

# NULLIF is the selected CASE/equality expansion, not an eager common-typed
# function. Preserve all preceding 830 case identities and error expectations.
for kind in ('TINYINT','SMALLINT','INTEGER','BIGINT','HUGEINT','UTINYINT','USMALLINT','UINTEGER','UBIGINT','UHUGEINT','FLOAT','DOUBLE','DECIMAL(4,2)','DECIMAL(38,3)','BIGNUM'):
    SQL.append(f'SELECT typeof(nullif(1::{kind},2::INTEGER)),nullif(1::{kind},2::INTEGER),nullif(1::{kind},1::INTEGER),nullif(NULL::{kind},2::INTEGER),nullif(1::{kind},NULL::INTEGER)')
for expression in (
    "nullif('2',2)", "nullif('3',2)", "nullif('3'::VARCHAR,2)",
    'nullif(NULL,NULL)', 'nullif([1::UHUGEINT],[2::INTEGER])',
    "nullif({'d':1.25},{'d':2.5})", "nullif('x'::ENUM('x','y'),'y')",
    "nullif('0101'::BIT,'0000'::BIT)", "nullif('abc'::BLOB,'abd'::BLOB)",
    "nullif('00000000-0000-0000-0000-000000000001'::UUID,'00000000-0000-0000-0000-000000000002'::UUID)",
    "nullif(DATE '2024-01-02',TIMESTAMP '2024-01-03 00:00:00')",
    "nullif(TIMESTAMP '2024-01-02 03:04:05',DATE '2024-01-03')",
    'nullif(340282366920938463463374607431768211455,2::UHUGEINT)',
):
    SQL.append(f'SELECT typeof({expression}),{expression}')
for op in ('=','<>','<','<=','>','>='):
    SQL += [
        f"SELECT NULL::INTEGER {op} CAST('bad' AS INTEGER)",
        f"SELECT a {op} CAST('bad' AS INTEGER) FROM (SELECT NULL::INTEGER a FROM range(3))t",
        f"SELECT i FROM (SELECT NULL::INTEGER a,i FROM range(3)t(i))t WHERE a {op} CAST('bad' AS INTEGER)",
    ]
    ERROR_CASES += [(f"SELECT CAST('bad' AS INTEGER) {op} NULL::INTEGER",'Conversion Error'),
                    (f"SELECT a {op} CAST('bad' AS INTEGER) FROM (VALUES(NULL::INTEGER),(NULL))t(a)",'Conversion Error')]
SQL += [
    "SELECT nullif(NULL::INTEGER,CAST('bad' AS INTEGER)),(SELECT NULL::INTEGER)=CAST('bad' AS INTEGER)",
    'SELECT nullif(a,b),count(*) FROM (VALUES(1::UHUGEINT,1::INTEGER),(2,9),(NULL,NULL))t(a,b) GROUP BY nullif(a,b) ORDER BY 1',
    'SELECT a.i,b.i FROM (VALUES(1,1::UHUGEINT,1::INTEGER),(2,2,9),(3,NULL,NULL))a(i,u,s) JOIN (VALUES(1,1::UHUGEINT,1::INTEGER),(2,2,9),(3,NULL,NULL))b(i,u,s) ON nullif(a.u,a.s)=nullif(b.u,b.s)',
    'SELECT nullif(u,s),sum(nullif(u,s)) OVER(ORDER BY i ROWS UNBOUNDED PRECEDING) FROM (VALUES(1,1::UHUGEINT,1::INTEGER),(2,2,9),(3,NULL,NULL))t(i,u,s) ORDER BY i',
]
ERROR_CASES += [(f'SELECT {expression}','Conversion Error') for expression in (
    'nullif(340282366920938463463374607431768211455,1)',
    'TRY_CAST(nullif(340282366920938463463374607431768211455,1) AS BIGINT)',
    "nullif('bad',1)", "nullif(CAST('bad' AS INTEGER),NULL::INTEGER)",
)]
ERROR_CASES += [(f'SELECT nullif({arguments})','Parser Error') for arguments in ('','1','1,2,3')]
# The original NULLIF report retains the initial wrong Binder expectation above.
# These cases exercise the separately repaired VALUES and reserved grammar paths.
for kind in ('UTINYINT','USMALLINT','UINTEGER','UBIGINT','UHUGEINT'):
    for values in (f'(1::{kind}),(2),(NULL)', f'(NULL),(1::{kind}),(2)',
                   f'(2),(1::{kind}),(NULL)', f'(1::{kind}),(2::INTEGER),(NULL)',
                   f'(1::{kind}),(2),(3)'):
        SQL.append(f'SELECT typeof(a),a FROM(VALUES {values})t(a)')
SQL += [
    'SELECT typeof(a),a FROM(VALUES(340282366920938463463374607431768211455),(1))t(a)',
    'SELECT typeof(a),a FROM(VALUES(1),(\'2\'))t(a)',
    'SELECT nullif FROM(VALUES(1))t(nullif)',
    'SELECT NuLlIf /* gap */ ((SELECT 1),2)::BIGINT',
]
ERROR_CASES += [
    ('SELECT a FROM(VALUES(1),(340282366920938463463374607431768211455))t(a)','Conversion Error'),
    # The immutable 935/936 checkpoint retains this initial Binder mismatch.
    # MaxLogicalType rejects it as NotImplementedException in both references.
    ("SELECT a FROM(VALUES('1'),(2))t(a)",'Not implemented Error'),
    ("SELECT a FROM(VALUES('1'),(2::INTEGER))t(a)",'Not implemented Error'),
    ("SELECT a FROM(VALUES('1'),(340282366920938463463374607431768211455))t(a)",'Not implemented Error'),
    ("SELECT a FROM(VALUES(1::INTEGER),(DATE '2024-01-01'))t(a)",'Not implemented Error'),
    ("SELECT a FROM(VALUES(DATE '2024-01-01'),(2::INTEGER))t(a)",'Not implemented Error'),
    ("SELECT a FROM(VALUES([1]),(2))t(a)",'Not implemented Error'),
    ("SELECT a FROM(VALUES(2),([1]))t(a)",'Not implemented Error'),
    ("SELECT CASE WHEN true THEN 1::INTEGER ELSE DATE '2024-01-01' END",'Binder Error'),
    ("SELECT coalesce(1::INTEGER,DATE '2024-01-01')",'Binder Error'),
]
ERROR_CASES += [(f'SELECT {expression}','Parser Error') for expression in (
    'nullif(1,2,)', 'nullif(DISTINCT 1,2)', 'nullif(1,2 ORDER BY 1)',
    'nullif(a:=1,b:=2)', 'nullif(1,2) FILTER(WHERE true)', 'nullif(1,2) OVER()',
)]

# IEEE policy belongs to the selected binding, not the evaluator's ambient
# context. Every new stateful case establishes its own mode; earlier 944 query
# identities and exact comparator behavior remain unchanged.
SQL += [
    "SELECT sqrt(-1)::VARCHAR,ln(0)::VARCHAR,log(-1)::VARCHAR,log2(0)::VARCHAR,pow(0,-1)::VARCHAR",
    "SELECT current_setting('ieee_floating_point_ops'),typeof(current_setting('ieee_floating_point_ops'))",
]
for mode in ('true', 'false', 'NULL'):
    prefix = f'SET ieee_floating_point_ops={mode}; '
    for name in ('sqrt', 'ln', 'log', 'log10', 'log2'):
        values = ('-0.0', '0.0') if name == 'sqrt' or mode != 'false' else ()
        values += ('4.9406564584124654e-324', '2.2250738585072014e-308',
                   '0.1', '0.5', '1.0', '2.0', '4.0', '10.0',
                   '1.7976931348623157e308', 'inf', 'nan')
        if mode != 'false':
            values += ('-1.0', '-inf')
        rows = ','.join(f"({index},'{value}'::DOUBLE)"
                        for index, value in enumerate(values)) + ',(100,NULL::DOUBLE)'
        SQL.append(prefix + f'SELECT x::VARCHAR,{name}(x)::VARCHAR,typeof({name}(x)) '
                   f'FROM(VALUES {rows})t(i,x) ORDER BY i')
        SQL.append(prefix + f'SELECT {name}(NULL),typeof({name}(NULL)),{name}(\'4\'),'
                   f'CASE WHEN false THEN {name}(-1) ELSE 1 END')
    for name in ('pow', 'power'):
        pairs = [('2', '10'), ('-2', '3'), ('-2', '4'), ('-1', '0.5'),
                 ('1e308', '2'), ('-0.0', '3'), ('-0.0', '2'),
                 ('nan', '0'), ('1', 'nan'), ('inf', '-1')]
        if mode != 'false':
            pairs += [('0', '-1'), ('-0.0', '-3'), ('-0.0', '-2')]
        rows = ','.join(f"({index},'{a}'::DOUBLE,'{b}'::DOUBLE)"
                        for index, (a, b) in enumerate(pairs)) + ',(100,NULL::DOUBLE,2)'
        SQL.append(prefix + f'SELECT a::VARCHAR,b::VARCHAR,{name}(a,b)::VARCHAR,'
                   f'typeof({name}(a,b)) FROM(VALUES {rows})t(i,a,b) ORDER BY i')
        SQL.append(prefix + f"SELECT {name}(NULL::DOUBLE,CAST('bad' AS DOUBLE)),"
                   f"{name}(NULL,NULL),typeof({name}(NULL,NULL))")
    SQL += [
        prefix + 'SELECT log(2,8),log(10,100),log(NULL::DOUBLE,-1),typeof(log(NULL,NULL))',
        prefix + "SELECT [sqrt(4),ln(1),NULL]::VARCHAR,{'p':pow(2,3),'l':log2(8)}::VARCHAR,concat(sqrt(4))",
        prefix + 'SELECT sqrt(x),count(*),sum(pow(sqrt(x),2)) FROM(VALUES(1.0),(4.0),(4.0),(NULL))t(x) GROUP BY sqrt(x) ORDER BY 1',
        prefix + 'SELECT a.x,sqrt(a.x),b.y FROM(VALUES(1),(4),(9),(NULL))a(x) JOIN(VALUES(1),(2),(3))b(y) ON sqrt(a.x)=b.y ORDER BY a.x',
        prefix + 'SELECT x,sqrt(x),lag(log2(x)) OVER(ORDER BY x),sum(pow(sqrt(x),2)) OVER(ORDER BY x ROWS UNBOUNDED PRECEDING) FROM(VALUES(1),(4),(16),(NULL))t(x) ORDER BY x',
    ]
    if mode != 'false':
        SQL.append(prefix + "SELECT log(1,10)::VARCHAR,log(0,10)::VARCHAR,log(-1,10)::VARCHAR,log(10,0)::VARCHAR,log(10,-1)::VARCHAR")
for kind in ('TINYINT', 'SMALLINT', 'INTEGER', 'BIGINT', 'HUGEINT',
             'UTINYINT', 'USMALLINT', 'UINTEGER', 'UBIGINT', 'UHUGEINT',
             'FLOAT', 'DOUBLE', 'DECIMAL(38,10)', 'BIGNUM'):
    SQL.append(f'SET ieee_floating_point_ops=true; SELECT sqrt(4::{kind}),ln(1::{kind}),log(10::{kind}),log2(8::{kind}),pow(2::{kind},3::{kind}),typeof(sqrt(4::{kind}))')
for name in ('sqrt', 'ln', 'log', 'log10', 'log2', 'pow', 'power'):
    suffix = ',2' if name in ('pow', 'power') else ''
    ERROR_CASES += [(f'SET ieee_floating_point_ops=true; SELECT {name}({argument}{suffix})', 'Binder Error')
                    for argument in ("'4'::VARCHAR", 'TRUE', "DATE '2024-01-01'", '[4]')]
    ERROR_CASES += [(f'SET ieee_floating_point_ops=true; SELECT {name}()', 'Binder Error'),
                    (f'SET ieee_floating_point_ops=true; SELECT {name}(1,2,3)', 'Binder Error')]
    ERROR_CASES.append((f"SET ieee_floating_point_ops=true; SELECT {name}('bad'{suffix})", 'Conversion Error'))
for expression in ('sqrt(-1)', "sqrt('-inf'::DOUBLE)", 'ln(-1)', 'ln(0)',
                   'log(-1)', 'log(0)', 'log10(-1)', 'log10(0)', 'log2(-1)', 'log2(0)',
                   'log(-1,0)', 'log(0,-1)', 'log(1,-1)', 'log(10,0)', 'log(10,-1)',
                   'pow(0,-1)', "power('-0.0'::DOUBLE,-3)"):
    ERROR_CASES.append((f'SET ieee_floating_point_ops=false; SELECT {expression}', 'Out of Range Error'))
ERROR_CASES += [
    ("SET ieee_floating_point_ops=true; SELECT pow(x,CAST('bad' AS DOUBLE)) FROM(VALUES(NULL::DOUBLE),(1::DOUBLE))t(x)", 'Conversion Error'),
    # Retain this known shared SET cast-origin category gap explicitly.
    ("SET ieee_floating_point_ops='bad'", 'Invalid Input Error'),
]
SQL += [
    "SET ieee_floating_point_ops=true; SELECT pow(CAST('bad' AS DOUBLE),NULL::DOUBLE)",
    "SET ieee_floating_point_ops=false; SELECT log(CAST('bad' AS DOUBLE),NULL::DOUBLE)",
    "SET ieee_floating_point_ops=false; SELECT log(NULL::DOUBLE,CAST('bad' AS DOUBLE))",
    "SET ieee_floating_point_ops=false; SELECT pow(NULL::DOUBLE,CAST('bad' AS DOUBLE))",
]


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
                trial['persistence'] += math_persistence(rust,Engine(cpp_path,False,serialize_json_rows=selected.serialize_json_rows),Path(scratch))
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
