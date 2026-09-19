"""Fail-closed, dual-pin B1 durable process measurement (only runs with --run)."""
import argparse, hashlib, json, platform, re, shutil, statistics, subprocess, time
from datetime import datetime, timezone
from pathlib import Path

from native_version_reference import header
from reference_version import ROOT, TARGETS, require_checkout, require_reference

METRICS = ("wall_ns", "cpu_ns", "max_rss_bytes", "block_input", "block_output")
TARGETS_ORDER = ("release", "development", "rust")
PHASES = ("publish", "reopen_query_drop")
EXPECTED = {"schema": 1, "id": "b1_view_durable_native", "rows": 10000, "checksum": 49995000,
 "samples": 21, "warmups": 3, "metrics": [*METRICS, "throughput"], "seed_table": "b1_seed",
 "column": "i", "configurations": ["checkpoint", "wal"], "workloads": ["view_cycle", "direct_table_publication"]}
HELPERS = ("measure_b1_views.py", "reference_version.py", "native_version_reference.py")

class SampleFailure(RuntimeError):
 def __init__(self, message, observation): super().__init__(message); self.observation = observation

def digest(path):
 with Path(path).open("rb") as f: return hashlib.file_digest(f, "sha256").hexdigest()
def file_id(path):
 path = Path(path).resolve(strict=True)
 return {"path": str(path), "sha256": digest(path), "bytes": path.stat().st_size}
def manifest(path):
 path = Path(path).resolve(strict=True); data = json.loads(path.read_text())
 if data != EXPECTED: raise ValueError("B1 durable workload manifest is changed or malformed")
 return {"path": str(path), "sha256": digest(path), "data": data}
def source_digest():
 paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "tools/shell/main.rs", *(ROOT / "src").rglob("*.rs"), *(ROOT / "vendor").rglob("*")]
 out = hashlib.sha256()
 for path in sorted(p for p in paths if p.is_file()): out.update(str(path.relative_to(ROOT)).encode()+b"\0"); out.update(path.read_bytes())
 return out.hexdigest()

def parse_time(stderr):
 match = re.search(r"(?m)^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys\s*$", stderr)
 if not match: raise ValueError("/usr/bin/time -l omitted CPU timing")
 out = {"cpu_ns": int((float(match.group(2))+float(match.group(3)))*1_000_000_000)}
 for key,label in (("max_rss_bytes","maximum resident set size"),("block_input","block input operations"),("block_output","block output operations")):
  value = re.search(rf"(?m)^\s*(\d+)\s+{re.escape(label)}\s*$", stderr)
  if not value: raise ValueError("/usr/bin/time -l omitted "+key)
  out[key] = int(value.group(1))
 return out
def timed(command, phase, execute=subprocess.run):
 if platform.system() != "Darwin": raise RuntimeError("durable acceptance requires macOS /usr/bin/time -l")
 command = list(map(str, command)); start = time.perf_counter_ns()
 result = execute(["/usr/bin/time", "-l", *command], text=True, capture_output=True)
 row = {"command":command,"phase":phase,"returncode":result.returncode,"stdout":result.stdout,"stderr":result.stderr,"wall_ns":time.perf_counter_ns()-start}
 if result.returncode: raise SampleFailure(phase+" CLI failed",row)
 try: row.update(parse_time(result.stderr))
 except ValueError as error: raise SampleFailure(str(error),row) from error
 return row

def sqls(workload, mode):
 create = "CREATE VIEW b1_public AS SELECT i FROM b1_seed" if workload == "view_cycle" else "CREATE TABLE b1_public AS SELECT i FROM b1_seed" if workload == "direct_table_publication" else None
 if create is None: raise ValueError("unknown durable workload")
 tail = " CHECKPOINT;" if mode == "checkpoint" else ""
 return {"publish":"SET threads=1; BEGIN TRANSACTION; "+create+"; COMMIT;"+tail,
  "reopen_query_drop":"SET threads=1; SELECT count(*) AS row_count, coalesce(sum(i),0) AS checksum FROM b1_public; BEGIN TRANSACTION; "+("DROP VIEW" if workload == "view_cycle" else "DROP TABLE")+" b1_public; COMMIT;"}
def absent_sql(): return "SET threads=1; SELECT * FROM b1_public"
def command(engine, db, mode, sql, readonly=False):
 if engine["kind"] == "cpp": return [engine["binary"],str(db),"-json",*( ["-readonly"] if readonly else []),"-c",("PRAGMA disable_checkpoint_on_shutdown; " if mode == "wal" else "")+sql]
 return [engine["binary"],str(db),*( ["--read-only"] if readonly else ["--durability",mode]),"--json","-c",sql]
def json_row(stdout):
 try: data=json.loads(stdout)
 except json.JSONDecodeError as error: raise ValueError("CLI did not emit JSON") from error
 if not isinstance(data,list) or len(data)!=1 or not isinstance(data[0],dict) or data[0]!={"row_count":10000,"checksum":49995000}: raise ValueError("unexpected checksum JSON")
def absent(result):
 if result.returncode == 0: raise ValueError("dropped publication remained readable")
 # Exact allowed classes, one per known CLI family; arbitrary errors never count.
 if not re.search(r"(?i)(catalog error:.*does not exist|binder error:.*not found|table .*does not exist|view .*does not exist)", result.stderr): raise ValueError("read-only absence failed with an unexpected error")
def seed_header(seed):
 seed=Path(seed).resolve(strict=True); wal=seed.with_name(seed.name+".wal")
 if wal.exists(): raise ValueError("seed must not have a WAL sidecar")
 value=header(seed)
 if value["effective"] != 64: raise ValueError("seed must use common storage version 64")
 return {**file_id(seed),"header":value,"wal_absent":True}
def seed_check(engine, db):
 result=subprocess.run(command(engine,db,"checkpoint","SET threads=1; SELECT count(*) AS row_count, coalesce(sum(i),0) AS checksum FROM b1_seed"),text=True,capture_output=True)
 if result.returncode: raise SampleFailure("seed verification failed",{"command":result.args,"returncode":result.returncode,"stdout":result.stdout,"stderr":result.stderr})
 json_row(result.stdout)

def one_sample(engine,mode,workload,seed,db):
 shutil.copyfile(seed,db); seed_check(engine,db); rows=[]
 for phase,sql in sqls(workload,mode).items():
  row=timed(command(engine,db,mode,sql),phase); rows.append(row)
  if phase=="publish" and mode=="wal" and not db.with_name(db.name+".wal").is_file(): raise SampleFailure("WAL was not retained before reopen",row)
  if phase=="reopen_query_drop": json_row(row["stdout"])
 result=subprocess.run(command(engine,db,mode,absent_sql(),True),text=True,capture_output=True); absent(result)
 aggregate={m:(max(r[m] for r in rows) if m=="max_rss_bytes" else sum(r[m] for r in rows)) for m in METRICS}
 return {"phases":rows,"aggregate":aggregate,"seed_sha256":digest(db),"artifact_sizes":{s:(db.with_name(db.name+s).stat().st_size if db.with_name(db.name+s).exists() else 0) for s in ("", ".wal")}}

def validate_sample(sample,engine,mode,workload,db):
 if set(sample)!={"phases","aggregate","seed_sha256","artifact_sizes"}: raise ValueError("sample fields changed")
 # caller verifies seed identity; raw command, metrics and stderr are independently rederived here.
 if not isinstance(sample.get("phases"),list) or [r.get("phase") for r in sample["phases"]] != list(PHASES): raise ValueError("missing/duplicate/reordered timed phases")
 wanted=sqls(workload,mode)
 for row in sample["phases"]:
  if row.get("returncode")!=0 or row.get("command") != command(engine,db,mode,wanted[row["phase"]]) or not isinstance(row.get("stdout"),str) or not isinstance(row.get("stderr"),str): raise ValueError("observation command/output is tampered")
  parsed=parse_time(row["stderr"])
  if row.get("wall_ns",0)<=0 or any(row.get(k)!=parsed[k] or parsed[k]<0 for k in METRICS if k != "wall_ns"): raise ValueError("observation metrics are missing or tampered")
 json_row(sample["phases"][1]["stdout"])
 recomputed={m:(max(r[m] for r in sample["phases"]) if m=="max_rss_bytes" else sum(r[m] for r in sample["phases"])) for m in METRICS}
 if sample.get("aggregate") != recomputed: raise ValueError("aggregate is tampered")
def gate(populations):
 if set(populations)!=set(TARGETS_ORDER) or any(len(v)!=21 for v in populations.values()): raise ValueError("requires exact 21 populations")
 med={t:{m:statistics.median(x["aggregate"][m] for x in rows) for m in METRICS} for t,rows in populations.items()}; best={m:min(med["release"][m],med["development"][m]) for m in METRICS}
 ratio=lambda v,b:1.0 if v==b==0 else float("inf") if b==0 else v/b
 ratios={m:ratio(med["rust"][m],best[m]) for m in METRICS}; cpp=max(1/med[t]["wall_ns"] for t in ("release","development")); rust=1/med["rust"]["wall_ns"]
 return {"medians":med,"cpp_fastest":best,"rust_over_fastest":ratios,"cpp_throughput":cpp,"rust_throughput":rust,"passed":all(v<=1 for v in ratios.values()) and rust>=cpp}

def schedule(output,mode,workload):
 return [(round_number,target,Path(output)/f"{mode}-{workload}-{target}-{round_number}.duckdb") for round_number in range(24) for target in (list(TARGETS_ORDER)[round_number%3:]+list(TARGETS_ORDER)[:round_number%3])]
def replay(report, context):
 if report.get("status")!="complete" or report.get("passed") is not True or report.get("manifest")!=context["manifest"] or report.get("inputs_before")!=context["inputs"] or report.get("inputs_after")!=context["inputs"]: raise ValueError("stale, failed, or tampered identity")
 expected={(m,w) for m in EXPECTED["configurations"] for w in EXPECTED["workloads"]}; results=report.get("results")
 if not isinstance(results,list) or len(results)!=4 or {(x.get("mode"),x.get("workload")) for x in results}!=expected: raise ValueError("invalid result population")
 gates=[]
 for item in results:
  expected_schedule=schedule(context["output"],item["mode"],item["workload"]); actual=item.get("schedule")
  if actual != [{"round":r,"target":t,"database":str(db)} for r,t,db in expected_schedule]: raise ValueError("schedule changed")
  for key,count in (("warmups",3),("observations",21)):
   if set(item.get(key,{}))!=set(TARGETS_ORDER) or any(len(item[key][t])!=count for t in TARGETS_ORDER): raise ValueError("missing population")
  for r,t,db in expected_schedule:
   bucket="warmups" if r<3 else "observations"; sample=item[bucket][t][r if r<3 else r-3]
   validate_sample(sample,context["engines"][t],item["mode"],item["workload"],db)
   if sample["seed_sha256"] != context["inputs"]["seed"]["sha256"]: raise ValueError("seed copy identity changed")
  gates.append({"mode":item["mode"],"workload":item["workload"],"gate":gate(item["observations"])})
 return {"results":gates,"passed":all(x["gate"]["passed"] for x in gates)}

def cpp_identity(label,source,build,binary):
 source,build=Path(source).resolve(strict=True),Path(build).resolve(strict=True); cache=build/"CMakeCache.txt"
 if "CMAKE_BUILD_TYPE:STRING=Release" not in cache.read_text(errors="replace"): raise ValueError("C++ build is not Release")
 require_checkout(source,label); _,cli=require_reference(binary,target=label)
 return {"source":str(source),"revision":TARGETS[label].revision,"build":file_id(cache),"cli":cli}
def context(args):
 spec=manifest(args.manifest); rust=Path(args.rust).resolve(strict=True); seed=seed_header(args.seed)
 refs={"release":cpp_identity("release",args.release_source,args.release_build,args.release),"development":cpp_identity("development",args.development_source,args.development_build,args.development)}
 engines={"release":{"kind":"cpp","binary":refs["release"]["cli"]["path"]},"development":{"kind":"cpp","binary":refs["development"]["cli"]["path"]},"rust":{"kind":"rust","binary":str(rust)}}
 inputs={"manifest":spec,"seed":seed,"helpers":{n:file_id(ROOT/"scripts"/n) for n in HELPERS},"references":refs,"rust":{"binary":file_id(rust),"source_sha256":source_digest()}}
 return {"manifest":spec,"inputs":inputs,"engines":engines,"output":str(Path(args.output_dir).resolve())}
def save(path,report): Path(path).write_text(json.dumps(report,indent=2)+"\n")
def run_campaign(args):
 output=Path(args.output_dir)
 if output.exists(): raise FileExistsError("preserve prior evidence: output exists")
 output.mkdir(parents=True); report={"status":"running","passed":False,"manifest":None,"inputs_before":None,"inputs_after":None,"results":[]}; path=output/"report.json"; save(path,report)
 try:
  c=context(args); report.update(manifest=c["manifest"],inputs_before=c["inputs"],schedule_context={"output":c["output"]})
  if not args.run: report.update(status="prepared",inputs_after=context(args)["inputs"]); return report
  for mode in EXPECTED["configurations"]:
   for workload in EXPECTED["workloads"]:
    item={"mode":mode,"workload":workload,"schedule":[{"round":r,"target":t,"database":str(db)} for r,t,db in schedule(c["output"],mode,workload)],"warmups":{t:[] for t in TARGETS_ORDER},"observations":{t:[] for t in TARGETS_ORDER}}; report["results"].append(item); save(path,report)
    for r,t,db in schedule(c["output"],mode,workload):
     try: row=one_sample(c["engines"][t],mode,workload,args.seed,db)
     except SampleFailure as error: item["failed_sample"]={"round":r,"target":t,"observation":error.observation}; raise
     item["warmups" if r<3 else "observations"][t].append(row); save(path,report)
  report["inputs_after"]=context(args)["inputs"]; report["status"]="complete"; report["passed"]=True; report["gate"]=replay(report,c); report["passed"]=report["gate"]["passed"]
 except Exception as error: report.update(status="failed",passed=False,error=str(error))
 finally: save(path,report)
 return report
def main():
 p=argparse.ArgumentParser(description=__doc__); p.add_argument("--manifest",type=Path,default=ROOT/"benchmark/b1_view_durable_workloads.json"); p.add_argument("--output-dir",type=Path,required=True); p.add_argument("--seed",type=Path,required=True); p.add_argument("--rust",type=Path,required=True); p.add_argument("--release",type=Path,default=TARGETS["release"].binary); p.add_argument("--development",type=Path,default=ROOT.parent/"duckdb/build/engine-walkthrough/duckdb"); p.add_argument("--release-source",type=Path,default=TARGETS["release"].source); p.add_argument("--release-build",type=Path,default=TARGETS["release"].build); p.add_argument("--development-source",type=Path,default=ROOT/"target/reference-source-development"); p.add_argument("--development-build",type=Path,default=ROOT.parent/"duckdb/build/engine-walkthrough"); p.add_argument("--run",action="store_true"); p.add_argument("--validate",type=Path)
 a=p.parse_args()
 if a.validate:
  report=json.loads(a.validate.read_text()); c=context(argparse.Namespace(**report["requested_arguments"])); result=replay(report,c); print(json.dumps(result)); raise SystemExit(0 if result["passed"] else 1)
 report=run_campaign(a); report["requested_arguments"]={k:str(v) for k,v in vars(a).items() if k not in ("run","validate")}; save(Path(a.output_dir)/"report.json",report); print(json.dumps({"status":report["status"],"passed":report["passed"]})); raise SystemExit(0 if report["status"]=="prepared" or report["passed"] else 1)
if __name__ == "__main__": main()
