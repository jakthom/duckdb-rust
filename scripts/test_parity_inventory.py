"""Inventory counts are evidence of presence, never execution or completeness."""
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from compiled_registry import enumerate_registry, source_parameterizations
from parity_inventory import native_registry, source_inventory


class InventoryTests(unittest.TestCase):
    def test_source_counts_keep_configurations_separate(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = {
                "test/sql/a.test": "statement ok\nSELECT 1;\n",
                "test/native.cpp": 'TEST_CASE("one", "[a]") {}\n',
                "test/configs/a.json": "{}",
                ".github/workflows/ci.yml": "name: test",
                "data/binary.test": "PAR1",
            }
            for name, content in fixtures.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            tracked = ("\0".join(fixtures) + "\0").encode()
            with patch("parity_inventory.subprocess.check_output", return_value=tracked):
                result = source_inventory(root)
            self.assertEqual(result["tracked_assets"], 5)
            self.assertEqual(result["counts"], {"native_declaration": 1, "sqllogictest": 2})
            self.assertEqual(result["sql_discovery"]["core_candidates"], ["test/sql/a.test"])
            self.assertEqual(result["sql_discovery"]["outside_core_roots"], ["data/binary.test"])
            self.assertEqual(result["configuration_files"], ["test/configs/a.json"])
            self.assertEqual(result["ci_workflows"], [".github/workflows/ci.yml"])

    def test_missing_native_runner_is_not_a_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = native_registry(SimpleNamespace(build=root, source=root), root)
            self.assertEqual(result["status"], "unavailable")

    def test_registry_requires_source_matched_cmake_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "test/unittest"
            binary.parent.mkdir()
            binary.write_bytes(b"fake test executable")
            result = enumerate_registry(root, binary, root)
            self.assertEqual(result["status"], "setup_failure")
            self.assertIn("CMakeCache", result["reason"])

    def test_registry_rejects_mismatched_cmake_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "build/test/unittest"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"fake")
            (root / "build/CMakeCache.txt").write_text(
                "CMAKE_HOME_DIRECTORY:INTERNAL=/wrong/pinned/source\nCMAKE_BUILD_TYPE:STRING=Release\n")
            result = enumerate_registry(root, binary, root)
            self.assertEqual(result["status"], "setup_failure")
            self.assertIn("does not match", result["reason"])

    def test_registry_accounts_hidden_sql_and_unique_ids(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "test/sql").mkdir(parents=True)
            (root / "third_party/sqllogictest/test").mkdir(parents=True)
            (root / "test/sql/a.test").write_text("SELECT 1")
            (root / "third_party/sqllogictest/test/b.test").write_text("SELECT 1")
            binary = root / "build/test/unittest"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"fake")
            (root / "build/CMakeCache.txt").write_text(
                f"CMAKE_HOME_DIRECTORY:INTERNAL={root.resolve()}\nCMAKE_BUILD_TYPE:STRING=Release\n")
            command = [str(binary), "*", "--list-tests"]
            listing = "name\tgroup\ntest/sql/a.test\t[sql]\nthird_party/sqllogictest/test/b.test\t[.][sqlitelogic]\nNative\t[native]\n"
            with patch("compiled_registry.subprocess.run",
                       return_value=subprocess.CompletedProcess(command, 0, listing, "")) as run:
                result = enumerate_registry(root, binary, root)
            self.assertEqual(result["status"], "enumerated_not_executed")
            self.assertEqual(result["hidden_cases"], 1)
            self.assertEqual(result["sql_file_cases"], 2)
            self.assertEqual(run.call_args.args[0], command)

    def test_registry_rejects_false_green_duplicate_or_missing_source_id(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "test/sql").mkdir(parents=True)
            (root / "test/sql/a.test").write_text("SELECT 1")
            binary = root / "build/test/unittest"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"fake")
            (root / "build/CMakeCache.txt").write_text(
                f"CMAKE_HOME_DIRECTORY:INTERNAL={root.resolve()}\nCMAKE_BUILD_TYPE:STRING=Release\n")
            command = [str(binary), "*", "--list-tests"]
            duplicate = "name\tgroup\ntest/sql/a.test\t[sql]\ntest/sql/a.test\t[sql]\n"
            with patch("compiled_registry.subprocess.run",
                       return_value=subprocess.CompletedProcess(command, 0, duplicate, "")):
                self.assertEqual(enumerate_registry(root, binary, root)["status"], "setup_failure")
            missing = "name\tgroup\nNative\t[native]\n"
            with patch("compiled_registry.subprocess.run",
                       return_value=subprocess.CompletedProcess(command, 0, missing, "")):
                self.assertEqual(enumerate_registry(root, binary, root)["status"], "setup_failure")

    def test_source_parameterizations_do_not_claim_runtime_instances(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "test/a.cpp"
            source.parent.mkdir()
            source.write_text("TEMPLATE_TEST_CASE(\"x\", \"\", int) {}\nGENERATE(1, 2);\nSECTION(\"x\") {}")
            result = source_parameterizations(root)
            self.assertEqual(result["counts"], {"generator": 1, "section": 1, "template": 1})
            self.assertIn("not exposed", result["runtime_instances"])


if __name__ == "__main__":
    unittest.main()
