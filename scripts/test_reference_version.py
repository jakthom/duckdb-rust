"""Wrong releases/builds must fail before any compatibility SQL is executed."""
import contextlib
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from reference_version import TARGETS, require_checkout, require_reference
import verify_reference


class ReferenceVersionTests(unittest.TestCase):
    def test_resolves_symlink_and_records_actual_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "duckdb-versioned"
            binary.write_bytes(b"independent reference binary")
            link = Path(directory) / "duckdb"
            link.symlink_to(binary)
            with patch("reference_version.shutil.which", return_value=str(link)), \
                 patch("reference_version.subprocess.check_output", return_value="v1.5.5 (Variegata) d8cdaa33fd\n") as command, \
                 contextlib.redirect_stderr(io.StringIO()):
                path, identity = require_reference(link)
            self.assertEqual(path, binary.resolve())
            command.assert_called_once_with([str(path), "--version"], text=True, timeout=10)
            self.assertEqual(identity["sha256"], hashlib.sha256(binary.read_bytes()).hexdigest())
            self.assertEqual(identity["required_revision"], TARGETS["release"].revision)

    def test_rejects_old_new_development_and_wrong_revision_binaries(self):
        versions = ["v1.3.0 71c5c07cdd", "v1.5.50 d8cdaa33fd",
                    "v1.5.5-dev1 d8cdaa33fd", "v1.5.5 0000000000", "v1.5.5",
                    "v2.0.0-dev84019 (Development Version) 99063af2bd"]
        with tempfile.NamedTemporaryFile() as binary:
            for version in versions:
                with self.subTest(version=version), \
                     patch("reference_version.shutil.which", return_value=binary.name), \
                     patch("reference_version.subprocess.check_output", return_value=version):
                    with self.assertRaises(ValueError):
                        require_reference(Path(binary.name))

    def test_development_target_is_separately_pinned(self):
        with tempfile.NamedTemporaryFile() as binary, \
             patch("reference_version.shutil.which", return_value=binary.name), \
             patch("reference_version.subprocess.check_output", return_value="v2.0.0-dev84019 (Development Version) 99063af2bd"), \
             contextlib.redirect_stderr(io.StringIO()):
            _, identity = require_reference(target="development")
            self.assertEqual(identity["target"], "development")
            self.assertEqual(identity["required_revision"], TARGETS["development"].revision)
        for revision, changes in [(TARGETS["release"].revision, b""),
                                  (TARGETS["development"].revision, b"local source changes")]:
            with patch("reference_version.subprocess.check_output", side_effect=[revision, changes]):
                with self.assertRaises(ValueError):
                    require_checkout(Path("source"), "development")

    def test_failed_campaign_preserves_report_and_returns_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "rust"
            binary.write_bytes(b"rust binary")
            output = Path(directory) / "report.json"
            identity = {"version": "v1.5.5 (Variegata) d8cdaa33fd"}
            with patch("sys.argv", ["verify_reference.py", "--rust", str(binary), "--report", str(output)]), \
                 patch("verify_reference.require_reference", return_value=(Path("reference"), identity)), \
                 patch("verify_reference.subprocess.check_output", return_value="test rustc"), \
                 patch("verify_reference.platform.platform", return_value="test-platform"), \
                 patch("verify_reference.verify", side_effect=RuntimeError("native file mismatch")), \
                 contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit) as exit:
                    verify_reference.main()
            self.assertEqual(exit.exception.code, 1)
            result = json.loads(output.read_text())
            self.assertFalse(result["campaign_completed"])
            self.assertEqual(result["result"], "failed")
            self.assertEqual(result["error"]["message"], "native file mismatch")


if __name__ == "__main__":
    unittest.main()
