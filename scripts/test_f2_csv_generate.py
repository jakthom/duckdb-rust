import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import sqllogic

SOURCE = Path(__file__).parents[1] / "test/performance/f2_csv_generate.py"
spec = importlib.util.spec_from_file_location("f2_csv_generate", SOURCE)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)

class GeneratorTest(unittest.TestCase):
    def test_deterministic_oracles_adapter_manifests_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            first = Path(directory) / "first'quoted"
            second = Path(directory) / "second"
            a = generator.materialize(first)
            b = generator.materialize(second)
            self.assertEqual([(x["sha256"], x["expected"]) for x in a["files"]], [(x["sha256"], x["expected"]) for x in b["files"]])
            with self.assertRaises(FileExistsError):
                generator.materialize(first)
            self.assertEqual(generator.verify(first), a)
            native = json.loads((first / "native-workloads.json").read_text())
            process = json.loads((first / "process-workloads.json").read_text())
            self.assertEqual(len(native["workloads"]), 2)
            self.assertEqual(len(process["workloads"]), 2)
            for entry, case, process_case in zip(a["files"], native["workloads"], process["workloads"]):
                self.assertEqual(process_case["path"], "test/" + entry["id"] + ".test")
                self.assertTrue((first / process_case["path"]).is_file())
                self.assertIn("first''quoted", case["sql"])
                self.assertIn("auto_detect=false", case["sql"])
                self.assertEqual(case["rows"], 1)
                expected = entry["expected"]
                vector = [expected["count"], expected["sum_id"],
                          *expected["length_sums"], *expected["non_null_counts"]]
                self.assertEqual(case["sum"], str(sum(vector)))
                records = sqllogic.parse((first / process_case["path"]).read_text())
                self.assertEqual(len(records), 16)
                for record in records:
                    self.assertEqual(record.words[0], "query")
                    self.assertEqual(len(record.words[1]), len(vector))
                    self.assertEqual(record.sql, entry["query"])
                    self.assertEqual(record.expected, ("\t".join(map(str, vector)),))
            metadata = first / "manifest.json"
            for mutate in (
                lambda data: data.update(generator_sha256="0" * 64),
                lambda data: data["files"].pop(),
                lambda data: data["files"].append(data["files"][0]),
                lambda data: data["files"][0]["expected"].update(count=0),
            ):
                modified = copy.deepcopy(a)
                mutate(modified)
                metadata.write_text(json.dumps(modified))
                with self.assertRaises(ValueError):
                    generator.verify(first)
            metadata.write_text(json.dumps(a))
            adapter = first / "native-workloads.json"
            original_adapter = adapter.read_bytes()
            adapter.write_bytes(original_adapter + b" ")
            with self.assertRaisesRegex(ValueError, "adapter manifest"):
                generator.verify(first)
            adapter.write_bytes(original_adapter)
            fixture = first / process["workloads"][0]["path"]
            original_fixture = fixture.read_bytes()
            fixture.write_bytes(original_fixture + b"\n# tampered\n")
            with self.assertRaisesRegex(ValueError, "changed F2"):
                generator.verify(first)
            fixture.write_bytes(original_fixture)
            path = Path(a["files"][0]["path"])
            with path.open("ab") as file:
                file.write(b"tampered\n")
            with self.assertRaisesRegex(ValueError, "changed F2"):
                generator.verify(first)

    def test_invalid_spec_rejected_before_creating_output(self):
        original = json.loads(generator.SPEC.read_text())
        mutations = [
            lambda s: s.update(version=2), lambda s: s.update(rows=True),
            lambda s: s.update(rows=0), lambda s: s.update(process_repetitions=1),
            lambda s: s["dialect"].update(auto_detect=True),
            lambda s: s["dialect"].update(threads=True),
            lambda s: s["workloads"].append(s["workloads"][0]),
            lambda s: s["workloads"][0].update(id="../escape"),
            lambda s: s["workloads"][0].update(columns=4),
            lambda s: s["workloads"][0].update(columns=3.0),
        ]
        with tempfile.TemporaryDirectory() as directory:
            spec_path = Path(directory) / "spec.json"
            output = Path(directory) / "output"
            for mutate in mutations:
                changed = copy.deepcopy(original)
                mutate(changed)
                spec_path.write_text(json.dumps(changed))
                with self.subTest(mutate=mutate), patch.object(generator, "SPEC", spec_path):
                    with self.assertRaises(ValueError):
                        generator.materialize(output)
                    self.assertFalse(output.exists())

if __name__ == "__main__":
    unittest.main()
