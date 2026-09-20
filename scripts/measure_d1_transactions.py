"""Prepare and measure fresh-process D1 two-connection workers against both pins."""
import argparse, hashlib, json, platform, statistics, subprocess, sys, time
from pathlib import Path
from measure_sqllogic_performance import METRICS, parse_time
from reference_version import ROOT, TARGETS, require_checkout, require_reference

SAMPLES, WARMUPS = 21, 3
EXPECTED_IDS = ("d1-disjoint-row-writers", "d1-contended-row-writers", "d1-catalog-disjoint-and-contended", "d1-retained-reader-publication")

def digest(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def identity(path):
    path=Path(path).resolve(strict=True); return {"path":str(path),"sha256":digest(path),"bytes":path.stat().st_size}
def workloads(path):
    data=json.loads(Path(path).read_text()); ids=tuple(item.get("id") for item in data.get("workloads",[]))
    if data.get("schema")!="d1-transaction-v1" or data.get("samples")!=SAMPLES or data.get("warmups")!=WARMUPS or ids!=EXPECTED_IDS: raise ValueError("invalid D1 workload population")
    return data
def compile_cpp(target, source, build, output):
    source,build=Path(source).resolve(strict=True),Path(build).resolve(strict=True); require_checkout(source,target)
    library=build/"src"/("libduckdb.dylib" if platform.system()=="Darwin" else "libduckdb.so")
    command=["c++","-std=c++17","-O3","-DNDEBUG","-I"+str(source/"src/include"),str(ROOT/"benchmark/d1_reference.cpp"),str(library),"-Wl,-rpath,"+str(library.parent),"-o",str(output)]
    subprocess.run(command,check=True); return {"command":command,"worker":identity(output),"library":identity(library),"cache":identity(build/"CMakeCache.txt")}
def require_fullsync(build):
    cache=(Path(build).resolve(strict=True)/"CMakeCache.txt").read_text()
    if "HAVE_FULLFSYNC=1" not in cache: raise ValueError("durable D1 retained-reader run requires full-sync C++ reference build")
def timed(command):
    if platform.system()!="Darwin": raise RuntimeError("D1 acceptance requires macOS /usr/bin/time -l")
    start=time.perf_counter_ns(); run=subprocess.run(["/usr/bin/time","-l",*map(str,command)],text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False); wall=time.perf_counter_ns()-start
    if run.returncode: raise RuntimeError(run.stderr[:4000])
    result=json.loads(run.stdout); metrics=parse_time(run.stderr)
    if result.get("schema")!="d1-worker-v1" or result.get("id")!=command[-1] or wall<=0: raise ValueError("invalid D1 worker result")
    return {**metrics,"wall_ns":wall,"throughput":1_000_000_000 / wall,"result":result}
def gate(observations):
    out={}; passed=True
    for item in observations:
        costs=(*METRICS,"wall_ns"); rust={key:statistics.median(row[key] for row in item["rust"]) for key in costs}; refs={name:{key:statistics.median(row[key] for row in item[name]) for key in costs} for name in TARGETS}; fastest={key:min(refs["release"][key],refs["development"][key]) for key in costs}; throughput={name:statistics.median(row["throughput"] for row in item[name]) for name in (*TARGETS,"rust")}; checks={key:rust[key]<=fastest[key] for key in costs}; checks["throughput"]=throughput["rust"]>=max(throughput["release"],throughput["development"]); passed &= all(checks.values()); out[item["id"]]={"rust":rust,"references":refs,"fastest":fastest,"throughput":throughput,"checks":checks}
    return {"workloads":out,"passed":passed}
def main():
    parser=argparse.ArgumentParser(description=__doc__); parser.add_argument("--workloads",type=Path,required=True); parser.add_argument("--report",type=Path,required=True); parser.add_argument("--prepare",action="store_true"); parser.add_argument("--run",action="store_true"); parser.add_argument("--rust",type=Path); parser.add_argument("--release-source",type=Path,required=True); parser.add_argument("--release-build",type=Path,required=True); parser.add_argument("--development-source",type=Path,required=True); parser.add_argument("--development-build",type=Path,required=True)
    args=parser.parse_args(); data=workloads(args.workloads)
    if args.report.exists(): raise FileExistsError("choose fresh D1 report")
    if args.prepare:
        subprocess.run(["cargo","build","--offline","--release","--no-default-features","--bin","duckdb-rust-d1-measure"],cwd=ROOT,check=True); rust=ROOT/"target/release/duckdb-rust-d1-measure"; prepared={"rust":identity(rust)}
        for target in TARGETS: prepared[target]=compile_cpp(target,getattr(args,f"{target}_source"),getattr(args,f"{target}_build"),ROOT/f"target/reference-d1-{target}")
        args.report.parent.mkdir(parents=True,exist_ok=True); args.report.write_text(json.dumps({"schema":"d1-prepare-v1","workloads":identity(args.workloads),"prepared":prepared},indent=2)+"\n"); return
    if not args.run or args.rust is None: raise ValueError("use --prepare or --run with attested --rust worker")
    for target in TARGETS: require_fullsync(getattr(args,f"{target}_build"))
    rust=Path(args.rust).resolve(strict=True); workers={"rust":rust,**{target:ROOT/f"target/reference-d1-{target}" for target in TARGETS}}; before={name:identity(path) for name,path in workers.items()}; raw=[]
    for item in data["workloads"]:
        samples={name:[] for name in workers}; expected=None
        for round_ in range(WARMUPS+SAMPLES):
            order=("rust","release","development") if round_%2==0 else ("development","release","rust")
            for name in order:
                sample=timed([workers[name],item["id"]]); result=sample["result"]; comparable=(result["rows"],result["checksum"],result["conflicts"])
                if expected is None: expected=comparable
                if comparable!=expected: raise ValueError("D1 worker results differ")
                if round_>=WARMUPS:samples[name].append(sample)
        raw.append({"id":item["id"],**samples})
    after={name:identity(path) for name,path in workers.items()}
    if before!=after: raise ValueError("D1 worker changed during measurement")
    report={"schema":"d1-measure-v1","samples":SAMPLES,"warmups":WARMUPS,"workloads":identity(args.workloads),"workers_before":before,"workers_after":after,"observations":raw,"gate":gate(raw)}; args.report.parent.mkdir(parents=True,exist_ok=True);args.report.write_text(json.dumps(report,indent=2)+"\n")
    if not report["gate"]["passed"]:raise SystemExit(1)
if __name__=="__main__":main()
