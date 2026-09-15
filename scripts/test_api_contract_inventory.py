import tempfile
import unittest
from pathlib import Path
import sys
import json
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import api_contract_inventory
from api_contract_inventory import RUST_TEST, assertions, rust_tests


class ApiContractInventoryTests(unittest.TestCase):
    def test_assertions_are_path_line_ordinal_records(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "test/api/sample.cpp"; path.parent.mkdir(parents=True)
            path.write_text('TEST_CASE("one") {\n REQUIRE(true);\n CHECK_FAIL(x);\n}\n')
            rows = assertions(root)
            self.assertEqual([row["id"] for row in rows], ["test/api/sample.cpp:2:1", "test/api/sample.cpp:3:2"])

    def test_rust_test_regex_finds_a_test_name(self):
        self.assertEqual(RUST_TEST.search("#[test]\nfn contract() {} ").group(1), "contract")

    def test_duplicate_rust_test_names_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); path = root / "test/a.rs"; path.parent.mkdir()
            path.write_text("#[test]\nfn same() {}\n#[test]\nfn same() {}")
            with self.assertRaisesRegex(ValueError, "duplicate Rust"):
                rust_tests(root)

    def test_rust_contract_metadata_requires_public_calls(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); path = root / "test/a.rs"; path.parent.mkdir()
            path.write_text("#[test]\nfn public() { let mut c=Database::memory().unwrap().connect(); c.query(\"SELECT 1\").unwrap(); }")
            self.assertTrue(rust_tests(root)["public"]["uses_public_api"])

    def assert_rejected_mapping(self, entries, message):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mapping = root / "mapping.json"
            mapping.write_text(json.dumps({"schema": 1, "entries": entries,
                "expected_mapped": len(entries),
                "expected_unmapped": {"development": 2 - len(entries), "release": 2 - len(entries)}}))
            rows = {"development": [{"id": "dev-1"}, {"id": "dev-2"}],
                    "release": [{"id": "rel-1"}, {"id": "rel-2"}]}
            argv = ["inventory", "--development-root", str(root), "--release-root", str(root),
                    "--mapping", str(mapping), "--output-dir", str(root / "out")]
            with patch.object(sys, "argv", argv), patch.object(api_contract_inventory, "require_pin"), \
                 patch.object(api_contract_inventory, "assertions", side_effect=lambda p: rows["development"] if p == root else rows["release"]), \
                 patch.object(api_contract_inventory, "rust_tests", return_value={"contract": {"path": "test/a.rs", "uses_public_api": True}}):
                # Roots are identical in this synthetic call, so replace the collected set directly.
                with patch.object(api_contract_inventory, "assertions", side_effect=[rows["development"], rows["release"]]):
                    with self.assertRaisesRegex(ValueError, message): api_contract_inventory.main()

    def test_wrong_source_contract_name_and_duplicate_target_fail_closed(self):
        base = {"source_ids": {"development": "dev-1", "release": "rel-1"},
                "rust_test": "contract", "invariant_category": "success",
                "public_api_evidence": "Database::memory/connect/query"}
        wrong_line = {**base, "source_ids": {"development": "dev-999", "release": "rel-1"}}
        self.assert_rejected_mapping([wrong_line], "unknown mapped source")
        self.assert_rejected_mapping([{**base, "rust_test": "missing"}], "unknown Rust")
        self.assert_rejected_mapping([{k: v for k, v in base.items() if k != "invariant_category"}], "lacks source")
        second = {**base, "source_ids": {"development": "dev-2", "release": "rel-2"}}
        self.assert_rejected_mapping([base, second], "duplicate Rust")


if __name__ == "__main__": unittest.main()
