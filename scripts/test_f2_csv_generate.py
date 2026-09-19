import importlib.util, json, tempfile, unittest
from pathlib import Path

SOURCE = Path(__file__).parents[1] / "test/performance/f2_csv_generate.py"
spec = importlib.util.spec_from_file_location("f2_csv_generate", SOURCE)
generator = importlib.util.module_from_spec(spec); spec.loader.exec_module(generator)

class GeneratorTest(unittest.TestCase):
    def test_deterministic_manifest_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            first = Path(directory) / "first"; second = Path(directory) / "second"
            a = generator.materialize(first); b = generator.materialize(second)
            self.assertEqual([(x["sha256"], x["expected"]) for x in a["files"]], [(x["sha256"], x["expected"]) for x in b["files"]])
            with self.assertRaises(FileExistsError): generator.materialize(first)
            self.assertEqual(json.loads((first / "manifest.json").read_text())["rows"], 100000)

if __name__ == "__main__": unittest.main()
