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
            dependencies = [
                "third_party/parser/Cargo.toml", "third_party/parser/src/ast/mod.rs",
                "vendor/utf8proc-sys/Cargo.toml", "vendor/utf8proc-sys/build.rs",
                "vendor/utf8proc-sys/src/lib.rs", "vendor/utf8proc-sys/src/generated.rs",
                "vendor/utf8proc-sys/utf8proc/utf8proc.c",
                "vendor/utf8proc-sys/utf8proc/utf8proc.h",
                "vendor/utf8proc-sys/utf8proc/utf8proc_data.c",
                "vendor/utf8proc-sys/PROVENANCE.md",
            ]
            sources = ["Cargo.toml", "Cargo.lock", "src/lib.rs", "test/runner/worker.rs",
                       *dependencies]
            for source in sources:
                path = root / source
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("original")
            self.assertEqual(vendored_sources(root), sorted(root / path for path in dependencies))
            with patch.object(session_reference, "ROOT", root):
                original = session_reference.source_fingerprint()
                for source in dependencies:
                    path = root / source
                    path.write_text("changed")
                    self.assertNotEqual(original, session_reference.source_fingerprint())
                    path.write_text("original")
                    self.assertEqual(original, session_reference.source_fingerprint())


if __name__ == "__main__":
    unittest.main()
