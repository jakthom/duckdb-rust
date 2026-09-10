"""Typed BIT/numeric-bitwise SQL and native interchange against both pinned references."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import platform
import subprocess
import tempfile
import time

from numeric_reference import equivalent
from reference_version import TARGETS, require_checkout, require_reference
from run_upstream import RustEngine
from session_reference import CppEngine, source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


SQL = [
    "SELECT '1'::BIT,'001'::BIT,''::BIT,'xAbC'::BIT,NULL::BIT,typeof('1'::BIT(19))",
    "SELECT typeof('1'::BITSTRING(-1))",
    "SELECT (-1::TINYINT)::BIT,(-1::HUGEINT)::BIT,'340282366920938463463374607431768211455'::UHUGEINT::BIT,true::BIT",
    "SELECT '1'::BIT::TINYINT,'11111111'::BIT::TINYINT,'1111111'::BIT::TINYINT,'100000000'::BIT::SMALLINT,'1'::BIT::BOOLEAN",
    "SELECT (-1.5::FLOAT)::BIT,(-1.5::DOUBLE)::BIT,((-1.5::FLOAT)::BIT)::FLOAT,((-1.5::DOUBLE)::BIT)::DOUBLE",
    r"SELECT '\x00\xFF'::BLOB::BIT,'001'::BIT::BLOB,TRY_CAST(''::BLOB AS BIT)",
    "SELECT TRY_CAST('x' AS BIT),TRY_CAST('2' AS BIT),TRY_CAST('000000000'::BIT AS TINYINT),TRY_CAST('1'::BIT AS UUID),TRY_CAST('1'::BIT AS DECIMAL(4,0))",
    "SELECT '001'::ENUM('001','1')::BIT,TRY_CAST('0'::BIT AS ENUM('1'))",
    "SELECT bitstring('101',5),bitstring('101'::BIT,5),bit_length('é'),bit_count('001'::BIT),get_bit('101'::BIT,1),set_bit('101'::BIT,1,1)",
    "SELECT length('001'::BIT),len('001'::BIT),char_length('001'::BIT),character_length('001'::BIT),octet_length('001'::BIT),hex(bitstring_byte_comparable('001'::BIT))",
    "SELECT bit_position('001'::BIT,'0001'::BIT),bit_position('101'::BIT,'1101'::BIT),bit_position('11'::BIT,'111'::BIT),bit_position('1111'::BIT,'111'::BIT)",
    "SELECT '101'::BIT & '011'::BIT,'101'::BIT | '011'::BIT,xor('101'::BIT,'011'::BIT),~'101'::BIT,'101'::BIT << 1,'101'::BIT >> 1,'101'::BIT >> -1,'101'::BIT << 3",
    "SELECT bit_count(-1::TINYINT),bit_count(-1::SMALLINT),bit_count(-1::INTEGER),bit_count(-1::BIGINT),bit_count(-1::HUGEINT),bit_count(255::UTINYINT),bit_count(65535::USMALLINT),bit_count(4294967295::UINTEGER),bit_count(18446744073709551615::UBIGINT)",
    "SELECT typeof(bit_count(NULL)),typeof(bit_length(NULL)),typeof(bitstring(NULL,3)),typeof(bit_and(NULL)),typeof(xor(NULL,NULL))",
    "SELECT typeof(xor(1::UTINYINT,1)),typeof(xor(1::UTINYINT,1::INTEGER)),typeof(xor(1::UTINYINT,CASE WHEN true THEN 1 ELSE 2 END))",
    "SELECT typeof(1::UTINYINT << 1),typeof(1::UTINYINT << 1::INTEGER),(-1::TINYINT) >> 1,(-1::TINYINT) >> -1,(128::UTINYINT) << 0,(0::HUGEINT) << 127",
    "SELECT k FROM (VALUES ('1'::BIT),('01'::BIT),('0'::BIT),('00'::BIT),('010'::BIT),(NULL)) t(k) ORDER BY k",
    "SELECT k,count(*) FROM (VALUES ('1'::BIT),('01'::BIT),('0'::BIT),('00'::BIT),('01'::BIT),(NULL)) t(k) GROUP BY k ORDER BY k",
    "SELECT min(k),max(k),count(DISTINCT k) FROM (VALUES ('1'::BIT),('01'::BIT),('0'::BIT),('00'::BIT),('01'::BIT),(NULL)) t(k)",
    "SELECT count(*) FROM (VALUES ('1'::BIT),('01'::BIT),('0'::BIT),('00'::BIT),(NULL)) a(k) JOIN (VALUES ('1'::BIT),('01'::BIT),('0'::BIT),('00'::BIT),(NULL)) b(k) ON a.k=b.k",
    "SELECT bit_and(k),bit_or(k),bit_xor(k),bit_xor(DISTINCT k) FROM (VALUES ('001'::BIT),('010'::BIT),('111'::BIT),('001'::BIT),(NULL)) t(k)",
    "SELECT bit_xor(k) OVER(ORDER BY n ROWS UNBOUNDED PRECEDING),lag(k) OVER(ORDER BY n) FROM (VALUES (1,'001'::BIT),(2,'010'::BIT),(3,'111'::BIT),(4,NULL)) t(n,k) ORDER BY n",
    "SELECT bit_xor('101'::BIT),bit_and('101'::BIT),bit_or('101'::BIT) FROM (VALUES (1),(2),(3),(4)) t(n)",
    "SELECT bit_xor(k),bit_and(k),bit_or(k) FROM (VALUES ('001'::BIT)) t(k) WHERE false",
    "SELECT k FROM (VALUES ('1'::BIT),('01'::BIT),(NULL)) t(k) UNION SELECT '00'::BIT ORDER BY k",
    "SELECT ['1'::BIT,NULL,'001'::BIT]::VARCHAR[],struct_extract({'k':'001'::BIT,'d':1.25::DECIMAL(4,2)},'k'),coalesce(NULL::BIT,'01'::BIT)",
]
for kind, width in [("TINYINT",8),("SMALLINT",16),("INTEGER",32),("BIGINT",64),("HUGEINT",128),
                    ("UTINYINT",8),("USMALLINT",16),("UINTEGER",32),("UBIGINT",64),("UHUGEINT",128)]:
    signed = not kind.startswith("U")
    maximum = (1 << (width - int(signed))) - 1
    SQL.append(f"SELECT ~0::{kind},xor('{maximum}'::{kind},1::{kind}),'{maximum}'::{kind} & 1::{kind},'{maximum}'::{kind} | 1::{kind},1::{kind} << {width-1-int(signed)}::{kind},'{maximum}'::{kind} >> 1::{kind}")

ERRORS = [
    ("SELECT -1::TINYINT::BIT", "Binder Error"),
    ("SELECT '1'::BIT::ENUM('1')", "Conversion Error"),
    ("SELECT ''::BLOB::BIT", "Conversion Error"),
    ("SELECT 'x'::BIT", "Conversion Error"),
    ("SELECT '2'::BIT", "Conversion Error"),
    ("SELECT '000000000'::BIT::TINYINT", "Conversion Error"),
    ("SELECT bitstring('',1)", "Conversion Error"),
    ("SELECT bitstring('xF',9)", "Conversion Error"),
    ("SELECT bitstring('1',0)", "Invalid Input Error"),
    ("SELECT bitstring('1',-1)", "Invalid Input Error"),
    ("SELECT get_bit('1'::BIT,-1)", "Out of Range Error"),
    ("SELECT get_bit('1'::BIT,1)", "Out of Range Error"),
    ("SELECT set_bit('1'::BIT,0,2)", "Invalid Input Error"),
    ("SELECT '1'::BIT & '01'::BIT", "Invalid Input Error"),
    ("SELECT bit_and(k) FROM (VALUES ('1'::BIT),('01'::BIT)) t(k)", "Invalid Input Error"),
    ("SELECT '1'::BIT << -1", "Out of Range Error"),
    ("SELECT (-1::TINYINT) << 0", "Out of Range Error"),
    ("SELECT (0::TINYINT) << -1", "Out of Range Error"),
    ("SELECT (1::TINYINT) << 7", "Out of Range Error"),
    ("SELECT (1::UTINYINT) << 8", "Out of Range Error"),
    ("SELECT bit_count(1::UHUGEINT)", "Binder Error"),
    ("SELECT bit_count(1.0)", "Binder Error"),
    ("SELECT '1'::BIT + '1'::BIT", "Binder Error"),
]


def persistence(rust, cpp, directory):
    outcomes = []
    definition = "CREATE TABLE t(k BIT PRIMARY KEY DEFAULT '0',v BIT DEFAULT '001',d DECIMAL(8,2) DEFAULT 1.25,u UUID DEFAULT '00112233445566778899aabbccddeeff'); INSERT INTO t DEFAULT VALUES; INSERT INTO t(k,v) VALUES ('00','101'),('01',NULL),('1','111111111')"
    query = "SELECT k::VARCHAR AS k,v::VARCHAR AS v,d::VARCHAR AS d,u::VARCHAR AS u,typeof(k) AS kt,bit_length(v) AS vl FROM t ORDER BY k"
    for label, producer in [("cpp",cpp),("rust-checkpoint",rust),("rust-wal",Engine(rust.binary,True,("--durability","wal")))]:
        case = {"producer":label,"passed":False}
        outcomes.append(case)
        try:
            path = directory / f"bit-{label}.duckdb"
            command(producer,path,definition)
            case["checkpoint_sha256"] = digest(path)
            wal = Path(str(path)+".wal")
            if wal.exists(): case["wal_sha256"] = digest(wal)
            for stage, mutation in [("initial",None),("rust-mutation","BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET v=set_bit(v,1,1) WHERE k='0'::BIT"),("cpp-mutation","DELETE FROM t WHERE k='01'::BIT; CHECKPOINT")]:
                if mutation: command(rust if stage=="rust-mutation" else cpp,path,mutation)
                expected = command(cpp,path,query,json_output=True,readonly=True)
                actual = command(rust,path,query,json_output=True,readonly=True)
                lookup = "SELECT k::VARCHAR AS k,v::VARCHAR AS v FROM t WHERE k='00'::BIT"
                cpp_lookup = command(cpp,path,lookup,json_output=True,readonly=True)
                rust_lookup = command(rust,path,lookup,json_output=True,readonly=True)
                case.setdefault("stages",[]).append({"stage":stage,"rust":actual,"cpp":expected,"rust_lookup":rust_lookup,"cpp_lookup":cpp_lookup})
                if actual != expected or cpp_lookup != rust_lookup or len(rust_lookup) != 1:
                    raise AssertionError("typed native rows or indexed lookup differ")
            case["passed"] = True
        except Exception as error:
            case["error"] = str(error)
    return outcomes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report",type=Path,required=True)
    args = parser.parse_args()
    if args.report.exists(): raise FileExistsError("Preserve earlier evidence; choose a new report")
    before = source_fingerprint()
    build = ["cargo","build","--offline","--release","--no-default-features","--bin","duckdb-rust","--bin","duckdb-rust-test-worker"]
    subprocess.run(build,cwd=ROOT,check=True)
    worker = ROOT/"target/release/duckdb-rust-test-worker"
    rust = Engine(ROOT/"target/release/duckdb-rust",True)
    report = {"recorded_at":datetime.now(timezone.utc).isoformat(),"source_sha256":before,"build_command":build,"rust_worker_sha256":digest(worker),"rust_cli_sha256":digest(rust.binary),"script_sha256":digest(Path(__file__)),"targets":[],"full_parity":False,"scope":"Selected typed BIT, full-width numeric bitwise SQL, error categories, and native checkpoint/WAL/default/index/mutation paths. Development governs disagreements. No full scalar, diagnostic, performance or engine parity claim."}
    for target,selected in TARGETS.items():
        trial = {"target":target,"sql":[],"persistence":[],"passed":False}
        report["targets"].append(trial)
        try:
            require_checkout(selected.source,target)
            cpp_path,trial["reference_identity"] = require_reference(target=target)
            library = selected.build/"src"/("libduckdb.dylib" if platform.system()=="Darwin" else "libduckdb.so")
            reference = ROOT/f"target/bit-reference-{target}"
            compile_command = ["c++","-std=c++17","-O3","-DNDEBUG","-I"+str(selected.source/"src/include"),str(ROOT/"test/runner/reference.cpp"),str(library),"-Wl,-rpath,"+str(library.parent),"-o",str(reference)]
            subprocess.run(compile_command,check=True)
            trial.update(compile_command=compile_command,cpp_library_sha256=digest(library),cpp_worker_sha256=digest(reference))
            with tempfile.TemporaryDirectory(prefix="ddb-bit-reference-") as scratch:
                cpp = CppEngine(reference,scratch,time.monotonic()+180)
                actual = RustEngine(worker,scratch,time.monotonic()+180)
                try:
                    if not selected.revision.startswith(cpp.identity["source_id"]): raise ValueError("Loaded reference library identity differs")
                    trial["rust_adapters"] = actual.request({"operation":"describe"})
                    for sql,expected_error in [(sql,None) for sql in SQL]+ERRORS:
                        request = {"operation":"query","sql":sql}
                        a,b = actual.request(request),cpp.request(request)
                        trial["sql"].append({"sql":sql,"expected_development_error":expected_error,"rust":a,"cpp":b,"passed":equivalent(a,b,expected_error)})
                finally:
                    actual.close()
                    cpp.close()
                trial["persistence"] = persistence(rust,Engine(cpp_path,False,serialize_json_rows=selected.serialize_json_rows),Path(scratch))
            trial["passed"] = all(case["passed"] for case in trial["sql"]+trial["persistence"])
        except Exception as error:
            trial["error"] = str(error)
    report["source_unchanged"] = before==source_fingerprint()
    report["passed"] = report["source_unchanged"] and all(trial["passed"] for trial in report["targets"])
    args.report.parent.mkdir(parents=True,exist_ok=True)
    args.report.write_text(json.dumps(report,indent=2)+"\n")
    print(json.dumps({"passed":report["passed"],"report":str(args.report),"targets":[{"target":trial["target"],"sql_passed":sum(case["passed"] for case in trial["sql"]),"sql_total":len(trial["sql"]),"persistence":[{"producer":case["producer"],"passed":case["passed"],"error":case.get("error")} for case in trial["persistence"]],"error":trial.get("error")} for trial in report["targets"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__=="__main__":
    main()
