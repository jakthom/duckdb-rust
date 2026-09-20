import importlib.util,unittest
from pathlib import Path
s=importlib.util.spec_from_file_location("f2",Path(__file__).with_name("measure_f2_copy_csv.py"));f2=importlib.util.module_from_spec(s);s.loader.exec_module(f2)
class Mutations(unittest.TestCase):
 def test_zero_reference_does_not_hide_cost(self):self.assertEqual(f2.ratio(1,0),float("inf"))
 def test_population_constants_are_fixed(self):self.assertEqual((f2.WARMUPS,f2.SAMPLES),(3,21))
 def test_timed_failure_keeps_raw_child_output(self):
  class Run: returncode=7;stdout="child stdout";stderr="child stderr"
  old=f2.platform.system;f2.platform.system=lambda:"Darwin"
  oldrun=f2.subprocess.run;f2.subprocess.run=lambda *a,**k:Run()
  try:
   result=f2.timed(["worker"]);self.assertFalse(result["ok"]);self.assertEqual(result["stdout"],"child stdout");self.assertEqual(result["stderr"],"child stderr")
  finally:f2.platform.system=old;f2.subprocess.run=oldrun
if __name__=="__main__":unittest.main()
