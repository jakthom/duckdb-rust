"""Typed ENUM expressions and native checkpoint/WAL interchange with both pinned references."""
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
    "SELECT TRY_CAST(1 AS ENUM('1')),TRY_CAST(true AS ENUM('true')),TRY_CAST('1'::BLOB AS ENUM('1'))",
    "SELECT 'a'::ENUM('z','a',''),''::ENUM('z','a',''),NULL::ENUM('z','a',''),typeof('a'::ENUM('z','a',''))",
    "SELECT 'a'::ENUM('z','a')<'z'::ENUM('z','a'),'a'::ENUM('z','a')<'z'::ENUM('a','z'),'a'::ENUM('z','a')='a'::ENUM('a','z')",
    "SELECT 'a'::ENUM('z','a')::ENUM('a','z'),TRY_CAST('z'::ENUM('z','a') AS ENUM('a')),TRY_CAST('bad' AS ENUM('z','a'))",
    "SELECT '12'::ENUM('12','-1')::INTEGER,'12.50'::ENUM('12.50')::DECIMAL(8,2),'2000-02-29'::ENUM('2000-02-29')::DATE,'01:02:03'::ENUM('01:02:03')::TIME",
    r"SELECT '\x00\xFF'::ENUM('\x00\xFF')::BLOB,'00112233445566778899aabbccddeeff'::ENUM('00112233445566778899aabbccddeeff')::UUID",
    "SELECT lower('UP'::ENUM('UP')),upper('up'::ENUM('up')),length('é'::ENUM('é')),hex('A'::ENUM('A')),encode('A'::ENUM('A'))",
    "SELECT enum_first(NULL::ENUM('z','a','')),enum_last(NULL::ENUM('z','a','')),enum_code('a'::ENUM('z','a','')),enum_code(NULL::ENUM('z','a',''))",
    "SELECT enum_range(NULL::ENUM('z','a')),enum_range_boundary(NULL,'a'::ENUM('z','a')),enum_range_boundary('a'::ENUM('z','a'),NULL),enum_range_boundary('a'::ENUM('z','a'),'z'::ENUM('z','a'))",
    "SELECT enum_range_boundary(k,NULL) FROM (VALUES ('z'::ENUM('z','a')),('a'::ENUM('z','a'))) t(k)",
    "SELECT k FROM (VALUES ('a'::ENUM('z','a')),('z'::ENUM('z','a')),(NULL)) t(k) ORDER BY k",
    "SELECT k,count(*) FROM (VALUES ('a'::ENUM('z','a')),('z'::ENUM('z','a')),('a'::ENUM('z','a')),(NULL)) t(k) GROUP BY k ORDER BY k",
    "SELECT min(k),max(k),count(DISTINCT k) FROM (VALUES ('a'::ENUM('z','a')),('z'::ENUM('z','a')),(NULL)) t(k)",
    "SELECT count(*) FROM (VALUES ('a'::ENUM('z','a')),('z'::ENUM('z','a'))) a(k) JOIN (VALUES ('a'::ENUM('a','z')),('z'::ENUM('a','z'))) b(k) ON a.k=b.k",
    "SELECT first_value(k) OVER(ORDER BY k),lag(k) OVER(ORDER BY k) FROM (VALUES ('a'::ENUM('z','a')),('z'::ENUM('z','a'))) t(k) ORDER BY k",
    "SELECT k FROM (VALUES ('a'::ENUM('z','a')),('z'::ENUM('z','a'))) t(k) UNION SELECT 'a'::ENUM('a','z') ORDER BY k",
    "SELECT coalesce('a'::ENUM('z','a'),'z'),typeof(coalesce('a'::ENUM('z','a'),'z'::ENUM('a','z')))",
    "SELECT ['a'::ENUM('z','a'),NULL]::VARCHAR[],struct_extract({'e':'a'::ENUM('z','a'),'d':1.25::DECIMAL(4,2)},'e')",
    "SELECT '12'::ENUM('12')=12,'2000-02-29'::ENUM('2000-02-29')=DATE '2000-02-29'",
    "SELECT trunc(1.5),trunc(-1.5),typeof(trunc(1::UHUGEINT)),trunc('340282366920938463463374607431768211455'::UHUGEINT),typeof(trunc(NULL))",
]
ERRORS = [
    ("SELECT 'bad'::ENUM('a','b')", "Conversion Error"),
    ("SELECT NULL::ENUM('a','a')", "Invalid Input Error"),
    ("SELECT NULL::ENUM()", "Binder Error"),
    ("SELECT enum_first('bad'::ENUM('z','a'))", "Conversion Error"),
    ("SELECT enum_code('a')", "Binder Error"),
    ("SELECT enum_range_boundary(NULL,NULL)", "Binder Error"),
    ("SELECT enum_range_boundary('a'::ENUM('a','b'),'a'::ENUM('b','a'))", "Binder Error"),
    ("SELECT 'bad'::ENUM('bad')::INTEGER", "Conversion Error"),
    ("SELECT 1::INTEGER::ENUM('1','2')", "Conversion Error"),
]


def persistence(rust, cpp, directory):
    results = []
    for count in (3, 256):
        labels = [f"label{i}" for i in range(count)]
        data_type = "ENUM(" + ",".join(f"'{label}'" for label in labels) + ")"
        definition = f"CREATE TABLE t(k {data_type} PRIMARY KEY DEFAULT 'label{count-1}', v {data_type}, b BLOB DEFAULT '\\x00\\xFF', u UUID DEFAULT '00112233445566778899aabbccddeeff', d DECIMAL(8,2) DEFAULT 1.25); INSERT INTO t DEFAULT VALUES; INSERT INTO t(k,v) VALUES ('label0','label{count-1}')"
        query = "SELECT k::VARCHAR AS k,v::VARCHAR AS v,enum_code(k) AS code,b::VARCHAR AS b,u::VARCHAR AS u,d::VARCHAR AS d FROM t ORDER BY k"
        for label, producer in [("cpp",cpp),("rust-checkpoint",rust),("rust-wal",Engine(rust.binary,True,("--durability","wal")))]:
            case={"producer":label,"dictionary_size":count,"passed":False}
            results.append(case)
            try:
                path=directory/f"enum-{count}-{label}.duckdb"
                command(producer,path,definition)
                case["checkpoint_sha256"]=digest(path)
                wal=Path(str(path)+".wal")
                if wal.exists(): case["wal_sha256"]=digest(wal)
                for stage, mutation in [("initial",None),("rust-mutation",f"BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET v='label0' WHERE k='label{count-1}'::{data_type}"),("cpp-mutation",f"DELETE FROM t WHERE k='label0'::{data_type}; CHECKPOINT")]:
                    if mutation: command(rust if stage=="rust-mutation" else cpp,path,mutation)
                    expected=command(cpp,path,query,json_output=True,readonly=True)
                    actual=command(rust,path,query,json_output=True,readonly=True)
                    case.setdefault("stages",[]).append({"stage":stage,"rust":actual,"cpp":expected})
                    if actual!=expected: raise AssertionError("typed native rows differ")
                case["passed"]=True
            except Exception as error:
                case["error"]=str(error)
    return results


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report",type=Path,required=True)
    args=parser.parse_args()
    if args.report.exists(): raise FileExistsError("Preserve prior evidence; choose a new report")
    before=source_fingerprint()
    build=["cargo","build","--offline","--release","--no-default-features","--bin","duckdb-rust","--bin","duckdb-rust-test-worker"]
    subprocess.run(build,cwd=ROOT,check=True)
    worker=ROOT/"target/release/duckdb-rust-test-worker"
    rust=Engine(ROOT/"target/release/duckdb-rust",True)
    report={"recorded_at":datetime.now(timezone.utc).isoformat(),"source_sha256":before,"build_command":build,"rust_worker_sha256":digest(worker),"rust_cli_sha256":digest(rust.binary),"script_sha256":digest(Path(__file__)),"targets":[],"full_parity":False,"scope":"Selected ENUM typed SQL, error categories, and native checkpoint/WAL/default/index/mutation paths. Development governs disagreements. No full scalar, diagnostic, performance or engine parity claim."}
    for target,selected in TARGETS.items():
        trial={"target":target,"sql":[],"persistence":[],"passed":False}
        report["targets"].append(trial)
        try:
            require_checkout(selected.source,target)
            cpp_path,trial["reference_identity"]=require_reference(target=target)
            library=selected.build/"src"/("libduckdb.dylib" if platform.system()=="Darwin" else "libduckdb.so")
            reference=ROOT/f"target/enum-reference-{target}"
            compile_command=["c++","-std=c++17","-O3","-DNDEBUG","-I"+str(selected.source/"src/include"),str(ROOT/"test/runner/reference.cpp"),str(library),"-Wl,-rpath,"+str(library.parent),"-o",str(reference)]
            subprocess.run(compile_command,check=True)
            trial.update(compile_command=compile_command,cpp_library_sha256=digest(library),cpp_worker_sha256=digest(reference))
            with tempfile.TemporaryDirectory(prefix="ddb-enum-reference-") as scratch:
                cpp=CppEngine(reference,scratch,time.monotonic()+180)
                actual=RustEngine(worker,scratch,time.monotonic()+180)
                try:
                    if not selected.revision.startswith(cpp.identity["source_id"]): raise ValueError("Loaded reference library identity differs")
                    trial["rust_adapters"]=actual.request({"operation":"describe"})
                    for sql,expected_error in [(sql,None) for sql in SQL]+ERRORS:
                        request={"operation":"query","sql":sql}
                        a,b=actual.request(request),cpp.request(request)
                        trial["sql"].append({"sql":sql,"expected_development_error":expected_error,"rust":a,"cpp":b,"passed":equivalent(a,b,expected_error)})
                finally:
                    actual.close()
                    cpp.close()
                trial["persistence"]=persistence(rust,Engine(cpp_path,False,serialize_json_rows=selected.serialize_json_rows),Path(scratch))
            trial["passed"]=all(case["passed"] for case in trial["sql"]+trial["persistence"])
        except Exception as error:
            trial["error"]=str(error)
    report["source_unchanged"]=before==source_fingerprint()
    report["passed"]=report["source_unchanged"] and all(trial["passed"] for trial in report["targets"])
    args.report.parent.mkdir(parents=True,exist_ok=True)
    args.report.write_text(json.dumps(report,indent=2)+"\n")
    print(json.dumps({"passed":report["passed"],"report":str(args.report),"targets":[{"target":trial["target"],"failures":[case for case in trial["sql"]+trial["persistence"] if not case["passed"]],"error":trial.get("error")} for trial in report["targets"]]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__=="__main__":
    main()
