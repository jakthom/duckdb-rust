"""Prepared dual-pin F2 COPY CSV process/resource measurement (source only)."""
import argparse,json,platform,subprocess,time,statistics,hashlib,re,math
from pathlib import Path
from reference_version import TARGETS,require_checkout,require_reference
from upstream_suite import ROOT,digest
from run_upstream import worker_source_digest
WARMUPS,SAMPLES=3,21; METRICS=("wall_ns","cpu_ns","max_rss_bytes","block_input","block_output")
FULLSYNC=Path("/Users/jacobthomas/code/ddb/duckdb-rust/target/a2-fullfsync-reference")
def ident(p):return {"path":str(Path(p).resolve(strict=True)),"sha256":digest(p)}
def timed(cmd):
 if platform.system()!="Darwin":raise RuntimeError("F2 resources require Darwin /usr/bin/time -l")
 start=time.perf_counter_ns();r=subprocess.run(["/usr/bin/time","-l",*map(str,cmd)],text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE);out={"command":list(map(str,cmd)),"stdout":r.stdout,"stderr":r.stderr,"returncode":r.returncode,"wall_ns":time.perf_counter_ns()-start,"ok":False}
 try:
  if r.returncode:raise RuntimeError(f"timed child exit {r.returncode}")
  m=re.search(r"(?m)^\s*[0-9.]+\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys",r.stderr)
  if not m:raise ValueError("missing time CPU observation")
  out["cpu_ns"]=int((float(m[1])+float(m[2]))*1e9)
  for label,key in (("maximum resident set size","max_rss_bytes"),("block input operations","block_input"),("block output operations","block_output")):
   found=[x for x in r.stderr.splitlines() if x.strip().endswith(label)]
   if not found:raise ValueError(f"missing time {label}")
   out[key]=int(float(found[-1].strip()[:-len(label)].strip().split()[0]))
  out["payload"]=json.loads(r.stdout);out["ok"]=True
 except Exception as e:out["error"]=str(e)
 return out
def prepare(a):
 if a.receipt.exists():raise FileExistsError(a.receipt)
 build=(FULLSYNC/a.target).resolve(strict=True);prov=json.loads((build/"provenance.json").read_text());src=Path(prov["source"]).resolve(strict=True)
 if a.cpp_source and a.cpp_source.resolve(strict=True)!=src:raise ValueError("source differs from attested full-sync provenance")
 if a.cpp_build and a.cpp_build.resolve(strict=True)!=build:raise ValueError("build differs from attested full-sync provenance")
 rev=require_checkout(src,a.target);cli,_=require_reference(build/"duckdb",target=a.target);lib=build/"src"/"libduckdb.dylib";cache=(build/"CMakeCache.txt").read_text();commands=(build/"compile_commands.json").read_text()
 if "-DHAVE_FULLFSYNC=1" not in (prov.get("compile_command","")+commands):raise ValueError("effective full-sync flag missing")
 a.receipt.parent.mkdir(parents=True,exist_ok=True);ref=a.receipt.parent/f"f2-ref-{a.target}";cc=["c++","-std=c++17","-O3","-I"+str(src/"src/include"),str(ROOT/"benchmark/f2_copy_csv_reference.cpp"),str(lib),"-Wl,-rpath,"+str(lib.parent),"-o",str(ref)];subprocess.run(cc,check=True);before=worker_source_digest();subprocess.run(["cargo","build","--offline","--release","--no-default-features","--bin","duckdb-rust-f2-copy-csv"],cwd=ROOT,check=True);after=worker_source_digest()
 if before!=after:raise ValueError("Rust source changed during build")
 rust=ROOT/"target/release/duckdb-rust-f2-copy-csv";w=a.workloads.resolve(strict=True)
 if not isinstance(json.loads(w.read_text()).get("cases"),list) or not json.loads(w.read_text())["cases"]:raise ValueError("empty workload spec")
 a.receipt.write_text(json.dumps({"schema":"f2-prepared-v2","target":a.target,"revision":rev,"provenance":ident(build/"provenance.json"),"cache":ident(build/"CMakeCache.txt"),"compile_commands":ident(build/"compile_commands.json"),"cli":ident(cli),"library":ident(lib),"cpp":ident(ref),"rust":ident(rust),"sources":{x:ident(ROOT/x) for x in ("Cargo.toml","src/function/csv_writer.rs","src/main/client_context.rs","src/planner/binder/statement.rs","benchmark/f2_copy_csv_native.rs","benchmark/f2_copy_csv_reference.cpp","scripts/measure_f2_copy_csv.py")},"workloads":ident(w),"fingerprint_before":before,"fingerprint":after},indent=2)+"\n")
def verify(r,w,target):
 if r.get("schema")!="f2-prepared-v2" or r.get("target")!=target or ident(w)!=r.get("workloads") or worker_source_digest()!=r.get("fingerprint"):raise ValueError("stale F2 receipt")
 for key in ("cli","library","cpp","rust","provenance","cache","compile_commands"):
  if ident(r[key]["path"])!=r[key]:raise ValueError(f"changed {key}")
 for path,value in r["sources"].items():
  if ident(ROOT/path)!=value:raise ValueError(f"changed source {path}")
def ratio(a,b):return 1 if a==b==0 else float("inf") if b==0 else a/b
def measure(a):
 r=json.loads(a.receipt.read_text());w=a.workloads.resolve(strict=True);verify(r,w,a.target)
 if a.report.exists():raise FileExistsError(a.report)
 spec=json.loads(w.read_text()); report={"schema":"f2-process-v2","prepared":r,"warmups":WARMUPS,"samples":SAMPLES,"metrics":METRICS,"workloads":[],"passed":False}
 try:
  d=a.report.parent/"fixtures";d.mkdir(parents=True,exist_ok=True)
   for case in spec["cases"]:
    engines={}
    for name,key in (("cpp","cpp"),("rust","rust")):
     observed=[];warmups=[]
     for i in range(WARMUPS+SAMPLES):
      path=Path(d)/f"{case['name']}-{name}-{i}.csv"; record=timed([r[key]["path"],path,spec["rows"],case["options"]])
      if not record.get("ok"): (warmups if i<WARMUPS else observed).append(record);raise RuntimeError(record["error"])
      payload=record["payload"];data=path.read_bytes(); expected_hash=0
      for b in data:expected_hash=(expected_hash*257+b)&((1<<64)-1)
      if payload.get("rows")!=spec["rows"] or payload.get("written")!=spec["rows"] or payload.get("bytes")!=len(data) or payload.get("hash")!=expected_hash:raise ValueError("COPY result/byte oracle mismatch")
      if name=="cpp" and (not isinstance(payload.get("source_id"),str) or not r["revision"].startswith(payload["source_id"])):raise ValueError("runtime source ID mismatch")
      # The independent reader oracle is the opposite engine's CSV reader; it
      # is run after timing by the final functional adapter selection.
      record["bytes_sha256"]=hashlib.sha256(data).hexdigest();record["fixture"]=str(path)
      (warmups if i<WARMUPS else observed).append(record)
     engines[name]={"warmups":warmups,"samples":observed}
    # Exact bytes, not matching hashes alone: pair each fresh output and retain both fixtures.
    for i in range(WARMUPS+SAMPLES):
     left=(engines["cpp"]["warmups"]+engines["cpp"]["samples"])[i];right=(engines["rust"]["warmups"]+engines["rust"]["samples"])[i]
     if Path(left["fixture"]).read_bytes()!=Path(right["fixture"]).read_bytes():raise ValueError("cross-engine CSV byte mismatch")
    median={e:{m:statistics.median(x[m] for x in v["samples"]) for m in METRICS} for e,v in engines.items()};inner={e:statistics.median(x["payload"]["elapsed_ns"] for x in v["samples"]) for e,v in engines.items()};throughput={e:spec["rows"]*1e9/inner[e] for e in engines};ratios={m:ratio(median["rust"][m],median["cpp"][m]) for m in METRICS};passed=all(x<=1 for x in ratios.values()) and ratio(inner["rust"],inner["cpp"])<=1 and throughput["rust"]>=throughput["cpp"]
    report["workloads"].append({**case,"cpp":engines["cpp"],"rust":engines["rust"],"median":median,"inner":inner,"throughput":throughput,"ratios":ratios,"passed":passed})
  verify(r,w,a.target);report["passed"]=all(x["passed"] for x in report["workloads"])
 except Exception as e:report["error"]=str(e)
 a.report.parent.mkdir(parents=True,exist_ok=True);a.report.write_text(json.dumps(report,indent=2)+"\n");raise SystemExit(0 if report["passed"] else 1)
def gate(a):
 reports=[json.loads(p.read_text()) for p in a.reports]
 if len(reports)!=2 or {x["prepared"]["target"] for x in reports}!={"release","development"}:raise ValueError("need exact two-pin reports")
 if any(x.get("schema")!="f2-process-v2" or x.get("warmups")!=WARMUPS or x.get("samples")!=SAMPLES or not x.get("passed") or not x.get("workloads") for x in reports):raise ValueError("incomplete/failed/empty report")
 for key in ("rust","sources","workloads","fingerprint"):
  if reports[0]["prepared"].get(key)!=reports[1]["prepared"].get(key):raise ValueError(f"Rust input differs: {key}")
 rows=[]
 for pair in zip(reports[0]["workloads"],reports[1]["workloads"],strict=True):
  if pair[0]["name"]!=pair[1]["name"]:raise ValueError("workload mismatch")
  for x in pair:
   for engine in ("cpp","rust"):
    if len(x[engine].get("warmups",[]))!=WARMUPS or len(x[engine].get("samples",[]))!=SAMPLES:raise ValueError("incomplete raw population")
    for sample in x[engine]["warmups"]+x[engine]["samples"]:
     if not sample.get("ok") or any(not isinstance(sample.get(m),(int,float)) or not math.isfinite(sample[m]) or sample[m]<0 for m in METRICS):raise ValueError("invalid raw observation")
  med=lambda row,engine,m:statistics.median(x[m] for x in row[engine]["samples"])
  inn=lambda row,engine:statistics.median(x["payload"]["elapsed_ns"] for x in row[engine]["samples"])
  fastest={m:min(med(x,"cpp",m) for x in pair) for m in METRICS};ci=min(inn(x,"cpp") for x in pair);ct=max(x["cpp"]["samples"][0]["payload"]["rows"]*1e9/inn(x,"cpp") for x in pair);oks=[]
  for j,x in enumerate(pair):
   ri=inn(x,"rust");rt=x["rust"]["samples"][0]["payload"]["rows"]*1e9/ri;oks.append(ratio(ri,ci)<=1 and rt>=ct and all(ratio(med(x,"rust",m),fastest[m])<=1 for m in METRICS))
  rows.append({"name":pair[0]["name"],"passed":all(oks)})
 if a.report.exists():raise FileExistsError(a.report)
 a.report.parent.mkdir(parents=True,exist_ok=True);a.report.write_text(json.dumps({"schema":"f2-fastest-v1","reports":[str(p) for p in a.reports],"workloads":rows,"passed":all(x["passed"] for x in rows)},indent=2));raise SystemExit(0 if all(x["passed"] for x in rows) else 1)
def main():
 p=argparse.ArgumentParser();p.add_argument("--prepare",action="store_true");p.add_argument("--measure",action="store_true");p.add_argument("--gate",action="store_true");p.add_argument("--target",choices=TARGETS);p.add_argument("--cpp-source",type=Path);p.add_argument("--cpp-build",type=Path);p.add_argument("--receipt",type=Path);p.add_argument("--report",type=Path);p.add_argument("--reports",nargs=2,type=Path);p.add_argument("--workloads",type=Path,default=ROOT/"benchmark/f2_copy_csv_workloads.json");a=p.parse_args()
 if a.gate:
  if a.prepare or a.measure or not a.report or not a.reports:p.error("gate needs reports/output")
  gate(a);return
 if a.prepare==a.measure or not a.target or not a.receipt:p.error("choose prepare or measure")
 if a.prepare:prepare(a)
 elif a.report:measure(a)
 else:p.error("measure needs report")
if __name__=="__main__":main()
