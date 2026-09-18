from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import session_reference
from source_identity import vendored_sources


class SourceIdentityTests(unittest.TestCase):
    def test_manifest_and_recursive_vendor_edits_change_source_identity(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            sources = ["Cargo.toml", "Cargo.lock", "src/lib.rs", "test/runner/worker.rs",
                       "third_party/parser/Cargo.toml", "third_party/parser/src/ast/mod.rs"]
            for source in sources:
                path = root / source
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("original")
            self.assertEqual(len(vendored_sources(root)), 2)
            with patch.object(session_reference, "ROOT", root):
                original = session_reference.source_fingerprint()
                for source in sources[-2:]:
                    path = root / source
                    path.write_text("changed")
                    self.assertNotEqual(original, session_reference.source_fingerprint())
                    path.write_text("original")
                    self.assertEqual(original, session_reference.source_fingerprint())


if __name__ == "__main__":
    unittest.main()
