"""Inventory counts are evidence of presence, never execution or completeness."""
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

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

    def test_native_listing_is_not_execution_and_checks_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "test/unittest"
            binary.parent.mkdir()
            binary.write_bytes(b"fake test executable")
            configuration = SimpleNamespace(build=root, source=root)
            command = [str(binary), "*", "--list-test-names-only"]
            for status, expected in ((2, "enumerated_not_executed"), (-11, "setup_failure")):
                process = subprocess.CompletedProcess(command, status, "test/sql/a.test\nNative case\n", "")
                with patch("parity_inventory.subprocess.run", return_value=process) as run:
                    result = native_registry(configuration, root)
                self.assertEqual(result["status"], expected)
                self.assertEqual(run.call_args.args[0], command)
                if status == 2:
                    self.assertEqual(result["names"], 2)
                    self.assertEqual(result["sql_file_names"], 1)
                    self.assertEqual(result["other_names"], 1)


if __name__ == "__main__":
    unittest.main()
