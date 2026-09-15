import unittest
import sys
from pathlib import Path
sys.path.insert(0,str(Path(__file__).parent))
from measure_g01_api_lifecycle import MARKER, gate
class GateTests(unittest.TestCase):
 def test_marker_and_faster_reference_gate(self):
  self.assertEqual(MARKER,"G01_API_LIFECYCLE_PASS 3")
  sample=lambda n:{"wall_ns":n,"cpu_ns":n,"max_rss_bytes":n,"block_input":0,"block_output":0}
  self.assertTrue(gate({"release":[sample(2)]*9,"development":[sample(3)]*9},{"release":[sample(2)]*9,"development":[sample(2)]*9})["passed"])
 def test_requires_nine(self):
  with self.assertRaises(ValueError): gate({"release":[],"development":[]},{"release":[],"development":[]})
if __name__=='__main__': unittest.main()
