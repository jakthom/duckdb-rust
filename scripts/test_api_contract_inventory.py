import tempfile
import unittest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).parent))
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


if __name__ == "__main__": unittest.main()
