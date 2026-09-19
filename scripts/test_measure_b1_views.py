import copy, json, subprocess, sys, tempfile, unittest
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).parent))
import measure_b1_views as m

STDERR="0.01 real 0.00 user 0.01 sys\n9 maximum resident set size\n2 block input operations\n3 block output operations\n"
def sample(engine,mode,workload,db,value=100):
 rows=[]
 for phase,sql in m.sqls(workload,mode).items():
  rows.append({"command":m.command(engine,db,mode,sql),"phase":phase,"returncode":0,"stdout":'[{"row_count":10000,"checksum":49995000}]' if phase=="reopen_query_drop" else "[]","stderr":STDERR,"wall_ns":value,"cpu_ns":10_000_000,"max_rss_bytes":9,"block_input":2,"block_output":3})
 return {"phases":rows,"aggregate":{"wall_ns":value*2,"cpu_ns":20_000_000,"max_rss_bytes":9,"block_input":4,"block_output":6},"seed_sha256":"seed","artifact_sizes":{"":1,".wal":0}}
def context():
 engines={t:{"kind":"rust" if t=="rust" else "cpp","binary":t} for t in m.TARGETS_ORDER}
 return {"manifest":{"x":1},"inputs":{"seed":{"sha256":"seed"}},"engines":engines,"output":"/out"}
def report():
 c=context(); results=[]
 for mode in m.EXPECTED["configurations"]:
  for workload in m.EXPECTED["workloads"]:
   warm={t:[] for t in m.TARGETS_ORDER}; obs={t:[] for t in m.TARGETS_ORDER}; sched=[]
   for r,t,db in m.schedule("/out",mode,workload):
    (warm if r<3 else obs)[t].append(sample(c["engines"][t],mode,workload,db,90 if t=="rust" else 100)); sched.append({"round":r,"target":t,"database":str(db)})
   results.append({"mode":mode,"workload":workload,"schedule":sched,"warmups":warm,"observations":obs})
 return {"status":"complete","passed":True,"manifest":c["manifest"],"inputs_before":c["inputs"],"inputs_after":c["inputs"],"results":results}

class Tests(unittest.TestCase):
 def test_parse_time_rejects_missing_metrics(self):
  self.assertEqual(m.parse_time(STDERR)["cpu_ns"],10_000_000)
  with self.assertRaises(ValueError):m.parse_time("0 real 0 user 0 sys\n")
 def test_commands_use_two_timed_phases_serial_and_fresh_create(self):
  self.assertEqual(tuple(m.sqls("view_cycle","wal")),m.PHASES)
  self.assertIn("SET threads=1",m.sqls("view_cycle","wal")["publish"])
  self.assertIn("CREATE VIEW",m.sqls("view_cycle","wal")["publish"])
  self.assertNotIn("OR REPLACE",m.sqls("direct_table_publication","wal")["publish"])
  self.assertIn("CHECKPOINT",m.sqls("view_cycle","checkpoint")["publish"])
  self.assertIn("disable_checkpoint",m.command({"kind":"cpp","binary":"cpp"},"db","wal","SELECT 1")[-1])
 def test_replay_rejects_command_binary_path_metric_and_population_tampering(self):
  value=report(); c=context(); self.assertTrue(m.replay(value,c)["passed"])
  for mutate in (lambda x:x["results"].pop(),lambda x:x["results"][0]["observations"]["rust"].pop(),lambda x:x["results"][0]["observations"]["rust"][0]["phases"][0]["command"].__setitem__(0,"evil"),lambda x:x["results"][0]["observations"]["rust"][0]["phases"][0].__setitem__("cpu_ns",1),lambda x:x.__setitem__("inputs_after",{})):
   bad=copy.deepcopy(value);mutate(bad)
   with self.subTest(mutate=mutate),self.assertRaises(ValueError):m.replay(bad,c)
 def test_replay_rejects_failed_or_wrong_seed(self):
  value=report();value["status"]="failed"
  with self.assertRaises(ValueError):m.replay(value,context())
  value=report();value["results"][0]["observations"]["rust"][0]["seed_sha256"]="wrong"
  with self.assertRaises(ValueError):m.replay(value,context())
 def test_json_and_absence_fail_closed(self):
  m.json_row('[{"row_count":10000,"checksum":49995000}]')
  for value in ("[]",'[{""row_count"":10000}]','[{""row_count"":"10000",""checksum"":49995000}]'):
   with self.assertRaises(ValueError):m.json_row(value)
  m.absent(subprocess.CompletedProcess([],1,"","Catalog Error: Table with name b1_public does not exist"))
  for result in (subprocess.CompletedProcess([],0,"", ""),subprocess.CompletedProcess([],1,"","disk full")):
   with self.assertRaises(ValueError):m.absent(result)
 def test_timed_failure_and_output_overwrite_protection(self):
  with patch.object(m.platform,"system",return_value="Darwin"):
   with self.assertRaises(m.SampleFailure):m.timed(["x"],"publish",execute=lambda *a,**k:subprocess.CompletedProcess(a[0],1,"","x"))
  with tempfile.TemporaryDirectory() as d:
   out=Path(d)/"out";out.mkdir(); args=type("A",(),{"output_dir":out})()
   with self.assertRaises(FileExistsError):m.run_campaign(args)
 def test_manifest_is_fixed(self):
  with tempfile.TemporaryDirectory() as d:
   p=Path(d)/"m";p.write_text(json.dumps(m.EXPECTED));self.assertEqual(m.manifest(p)["data"],m.EXPECTED)
   bad=copy.deepcopy(m.EXPECTED);bad["samples"]=9;p.write_text(json.dumps(bad))
   with self.assertRaises(ValueError):m.manifest(p)
if __name__=="__main__":unittest.main()
