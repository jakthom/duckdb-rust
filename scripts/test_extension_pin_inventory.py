import unittest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).parent))
from extension_pin_inventory import CALL, KEY, active


class ExtensionPinInventoryTests(unittest.TestCase):
    def test_commented_load_is_not_a_configured_ref(self):
        self.assertEqual(CALL.findall(active("# duckdb_extension_load(nope GIT_TAG deadbeef)")), [])

    def test_load_fields_do_not_consume_the_next_keyword(self):
        body = "\n LOAD_TESTS\n GIT_URL https://example.invalid/repo\n GIT_TAG 0123456789012345678901234567890123456789\n"
        fields = dict(KEY.findall(body))
        self.assertEqual(fields["LOAD_TESTS"], "")
        self.assertEqual(fields["GIT_URL"], "https://example.invalid/repo")

    def test_short_ref_is_recognizably_not_immutable(self):
        self.assertNotRegex("deadbeef", r"[0-9a-f]{40}")


if __name__ == "__main__": unittest.main()
