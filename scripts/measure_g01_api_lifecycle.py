"""Serial, fail-closed Gate P measurement for the mapped API destruction lifecycle."""
import argparse, hashlib, json, os, platform, statistics, subprocess, time
from pathlib import Path

PINS={"development":"99063af2bd7092aff02e14184a20e24699d34d71","release":"d8cdaa33fda8df955cc76ef58a280f68f4cd43fa"}
METRICS=("wall_ns","cpu_ns","max_rss_bytes","block_input","block_output")
MARKER="G01_API_LIFECYCLE_PASS 3"

def digest(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def require(root,label):
    if subprocess.check_output(["git","rev-parse","HEAD"],cwd=root,text=True).strip()!=PINS[label]: raise ValueError("wrong "+label+" source identity")
    if subprocess.check_output(["git","status","--porcelain"],cwd=root,text=True): raise ValueError("dirty "+label+" source")
    build=root/"build"/({"development":"engine-walkthrough","release":"rewrite-reference"}[label])
    cache=build/"CMakeCache.txt"; library=build/"src/libduckdb.dylib"
    if not cache.is_file() or "CMAKE_BUILD_TYPE:STRING=Release" not in cache.read_text() or not library.is_file(): raise ValueError("missing release build/library for "+label)
    return build,library
def compile_reference(root,label,out):
    build,library=require(root,label); source=Path(__file__).with_name("g01_api_lifecycle_reference.cpp")
    command=["c++","-O3","-DNDEBUG","-std=c++17","-I",str(root/"src/include"),str(source),"-L",str(library.parent),"-lduckdb","-Wl,-rpath,"+str(library.parent),"-o",str(out)]
    result=subprocess.run(command,text=True,capture_output=True)
    if result.returncode: raise RuntimeError("C++ compile failed: "+result.stderr)
    return {"command":command,"source_sha256":digest(source),"library_sha256":digest(library),"binary_sha256":digest(out)}
def timed(command):
    if platform.system()!="Darwin": raise RuntimeError("requires macOS /usr/bin/time -l")
    start=time.perf_counter_ns(); run=subprocess.run(["/usr/bin/time","-l",*map(str,command)],text=True,capture_output=True); wall=time.perf_counter_ns()-start
    if run.returncode or run.stdout.strip()!=MARKER: raise RuntimeError("candidate failed: "+run.stderr+run.stdout)
    text=run.stderr; import re
    usage=re.search(r"(?m)^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys\s*$",text)
    labels={"max_rss_bytes":"maximum resident set size","block_input":"block input operations","block_output":"block output operations"}
    if not usage: raise RuntimeError("missing CPU time")
    sample={"command":[str(x) for x in command],"stdout":run.stdout,"stderr":text,"wall_ns":wall,"cpu_ns":int((float(usage.group(2))+float(usage.group(3)))*1e9)}
    for key,label in labels.items():
        match=re.search(r"(?m)^\s*(\d+)\s+"+re.escape(label)+r"\s*$",text)
        if not match: raise RuntimeError("missing "+key)
        sample[key]=int(match.group(1))
    return sample
def gate(cpp, rust):
    if any(len(value)!=9 for value in [*cpp.values(),*rust.values()]): raise ValueError("requires exactly 9 paired samples")
    cmed={key:{metric:statistics.median(x[metric] for x in rows) for metric in METRICS} for key,rows in cpp.items()}; rmed={key:{metric:statistics.median(x[metric] for x in rows) for metric in METRICS} for key,rows in rust.items()}
    base={metric:min(cmed["release"][metric],cmed["development"][metric]) for metric in METRICS}
    ratios={key:{metric:(1 if base[metric]==value[metric]==0 else float("inf") if base[metric]==0 else value[metric]/base[metric]) for metric in METRICS} for key,value in rmed.items()}
    throughput={key:1/rmed[key]["wall_ns"] for key in rmed}; cpp_throughput=max(1/cmed[key]["wall_ns"] for key in cmed); passed=all(v<=1 for row in ratios.values() for v in row.values()) and all(v>=cpp_throughput for v in throughput.values())
    return {"cpp_fastest":base,"rust":rmed,"ratios":ratios,"throughput":throughput,"passed":passed}
def main():
    p=argparse.ArgumentParser(); p.add_argument("--development-root",type=Path,required=True); p.add_argument("--release-root",type=Path,required=True); p.add_argument("--output-dir",type=Path,required=True); p.add_argument("--run",action="store_true"); a=p.parse_args()
    if a.output_dir.exists(): raise ValueError("output exists")
    a.output_dir.mkdir(parents=True); refs={}
    for label,root in (("development",a.development_root),("release",a.release_root)): refs[label]=compile_reference(root,label,a.output_dir/(label+"-reference"))
    rust=["cargo","build","--release","--no-default-features","--bin","g01-api-lifecycle"]; build=subprocess.run(rust,text=True,capture_output=True)
    if build.returncode: raise RuntimeError(build.stderr)
    candidate=Path("target/release/g01-api-lifecycle"); identity={"rust_build":rust,"rust_binary_sha256":digest(candidate),"references":refs}
    if not a.run: print(json.dumps({"prepared":True,**identity},sort_keys=True)); return
    for _ in range(3):
        for path in [a.output_dir/"release-reference",a.output_dir/"development-reference",candidate]: timed(path)
    samples={"release":[],"development":[],"rust_release":[],"rust_development":[]}
    for index in range(9):
        order = (("release",a.output_dir/"release-reference"),("rust_release",candidate),("development",a.output_dir/"development-reference"),("rust_development",candidate)) if index % 2 == 0 else (("development",a.output_dir/"development-reference"),("rust_development",candidate),("release",a.output_dir/"release-reference"),("rust_release",candidate))
        for label,path in order: samples[label].append(timed(path))
    report={"identity":identity,"samples":samples,"gate":gate({"release":samples["release"],"development":samples["development"]},{"release":samples["rust_release"],"development":samples["rust_development"]})}; (a.output_dir/"report.json").write_text(json.dumps(report,indent=2)+"\n")
    if not report["gate"]["passed"]: raise SystemExit(1)
if __name__=="__main__": main()
