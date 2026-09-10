"""Typed sequence concat against pinned development; correctness, not performance."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import tempfile

from reference_version import TARGETS, require_checkout, require_reference
from session_reference import source_fingerprint
from upstream_suite import ROOT, digest
from verify_reference import Engine, command


QUERIES = [
    "SELECT concat([1,NULL],[2],NULL)::VARCHAR value,typeof(concat([1],[2])) type",
    "SELECT concat(NULL::INTEGER[],NULL::INTEGER[])::VARCHAR value,concat(NULL,NULL) scalar_value,typeof(concat([],NULL)) empty_type",
    "SELECT concat([true],[1])::VARCHAR value,typeof(concat([true],[1])) type",
    "SELECT concat([{'b':true}],[{'b':2::UTINYINT}])::VARCHAR value,typeof(concat([{'b':true}],[{'b':2::UTINYINT}])) type",
    "SELECT concat([1]::INTEGER[1],[2]::BIGINT[1],NULL)::VARCHAR value,typeof(concat([1]::INTEGER[1],[2]::BIGINT[1])) type",
    "SELECT concat([{'a':1}],[{'b':2}])::VARCHAR value",
    "SELECT concat(NULL::INTEGER[2],[1]::INTEGER[1])::VARCHAR value,concat([1],NULL::INTEGER[]) IS NULL missing",
    "SELECT typeof(concat([make_timestamp_ns(-9223372036854775806)])) type",
    "SELECT concat([1.25::DECIMAL(12,2)],[NULL],[2.50::DECIMAL(12,2)])::VARCHAR value",
    "SELECT concat(['101'::BIT],[NULL],['0'::BIT])::VARCHAR value",
]
ERRORS = ["SELECT concat([1],1)", "SELECT concat([1],'[2]')", "SELECT concat([1],['2'])",
          "SELECT concat([true],[1.0::DOUBLE])", "SELECT concat([true],[1.2::DECIMAL(2,1)])",
          "SELECT concat([1],{'n':2})"]
SETUP = """CREATE TABLE t(id INTEGER PRIMARY KEY,xs STRUCT(n DECIMAL(12,2),ts TIMESTAMP_NS,b BIT)[]);
INSERT INTO t VALUES(1,concat([{'n':1.25,'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789','b':'101'::BIT}],NULL,[{'n':NULL,'ts':NULL,'b':NULL}])),
(2,concat(NULL::STRUCT(n DECIMAL(12,2),ts TIMESTAMP_NS,b BIT)[],[]));"""
QUERY = "SELECT id,xs::VARCHAR value FROM t ORDER BY id"


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust",type=Path,default=ROOT/"target/debug/duckdb-rust")
    parser.add_argument("--report",type=Path,required=True)
    args=parser.parse_args()
    if args.report.exists(): raise FileExistsError("Preserve earlier evidence; choose a new report path")
    require_checkout(TARGETS["development"].source,"development")
    binary,identity=require_reference(target="development")
    before=source_fingerprint()
    rust=Engine(args.rust,True)
    cpp=Engine(binary,False,serialize_json_rows=False)
    report={"recorded_at":datetime.now(timezone.utc).isoformat(),"source_sha256":before,
            "rust_binary_sha256":digest(args.rust),"script_sha256":digest(Path(__file__)),
            "reference_identity":identity,"sql":[],"native":[],"full_parity":False,
            "scope":"Selected typed LIST/ARRAY concat SQL and mixed DECIMAL/TIMESTAMP_NS/BIT native checkpoint/mutation/reopen. No ||/alias, benchmark or full nested parity claim."}
    for sql in QUERIES+ERRORS:
        case={"sql":sql,"expected_error":"Binder Error" if sql in ERRORS else None}
        report["sql"].append(case)
        for label,engine in [("rust",rust),("development",cpp)]:
            try: case[label]={"rows":command(engine,":memory:",sql,json_output=True)}
            except Exception as error: case[label]={"error":str(error)}
        case["passed"]=(all("Binder Error" in case[label].get("error","") for label in ["rust","development"])
                        if sql in ERRORS else "rows" in case["rust"] and case["rust"]==case["development"])
    with tempfile.TemporaryDirectory(prefix="nested-concat-reference-") as directory:
        for producer,engine in [("rust",rust),("development",cpp)]:
            case={"producer":producer,"passed":False,"stages":[]}
            report["native"].append(case)
            try:
                path=Path(directory)/f"{producer}.duckdb"
                command(engine,path,SETUP)
                case["initial_checkpoint_sha256"]=digest(path)
                for stage,writer,sql in [("initial",None,None),
                    ("rust_mutation",rust,"BEGIN; DELETE FROM t; ROLLBACK; UPDATE t SET xs=concat(xs,[NULL]) WHERE id=1"),
                    ("development_mutation",cpp,"UPDATE t SET xs=concat(xs,[NULL]) WHERE id=2; CHECKPOINT")]:
                    if writer: command(writer,path,sql)
                    actual=command(rust,path,QUERY,json_output=True,readonly=True)
                    expected=command(cpp,path,QUERY,json_output=True,readonly=True)
                    case["stages"].append({"stage":stage,"rust":actual,"development":expected})
                    if actual!=expected: raise AssertionError("Typed concat native rows differ")
                case["passed"]=True
            except Exception as error: case["error"]=str(error)
    report["source_unchanged"]=before==source_fingerprint()
    report["passed"]=report["source_unchanged"] and all(case["passed"] for case in report["sql"]+report["native"])
    args.report.write_text(json.dumps(report,indent=2,ensure_ascii=False)+"\n")
    print(json.dumps({"report":str(args.report),"passed":report["passed"],"sql_matches":sum(case["passed"] for case in report["sql"]),"sql_total":len(report["sql"]),"native":report["native"]}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__=="__main__": main()
