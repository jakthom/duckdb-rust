"""Checkpoint reporting must never label an empty or failed proof run successful."""

import subprocess
import unittest
from unittest.mock import patch

import verify_kani


PASSED = "Complete - 3 successfully verified harnesses, 0 failures, 3 total.\n"


class KaniOutcomeTests(unittest.TestCase):
    def test_complete_run(self):
        self.assertEqual(verify_kani.verification_status(0, "compiler output\n" + PASSED), 0)

    def test_empty_or_missing_summary(self):
        for output in ["", "VERIFICATION:- SUCCESSFUL\n",
                       "Complete - 0 successfully verified harnesses, 0 failures, 0 total.\n"]:
            with self.subTest(output=output):
                self.assertNotEqual(verify_kani.verification_status(0, output), 0)

    def test_incomplete_or_failed_proofs_even_with_zero_exit(self):
        for output in [
            "Complete - 2 successfully verified harnesses, 1 failures, 3 total.\n",
            "Complete - 2 successfully verified harnesses, 0 failures, 3 total.\n",
            PASSED + PASSED,
        ]:
            with self.subTest(output=output):
                self.assertNotEqual(verify_kani.verification_status(0, output), 0)

    def test_failure_or_signal_overrides_passing_summary(self):
        for status in [1, 101, 124, -9]:
            with self.subTest(status=status):
                self.assertNotEqual(verify_kani.verification_status(status, PASSED), 0)
        self.assertEqual(verify_kani.verification_status(101, PASSED), 101)

    def test_wrong_or_missing_verifier_never_starts_proofs(self):
        for result in [subprocess.CompletedProcess([], 0, "cargo-kani 0.0.0\n"),
                       subprocess.CompletedProcess([], 101, "no such command: kani\n")]:
            with self.subTest(result=result), \
                 patch("verify_kani.subprocess.run", return_value=result), \
                 patch("verify_kani.subprocess.Popen") as launch, \
                 patch("verify_kani.sys.stderr"):
                self.assertNotEqual(verify_kani.main(), 0)
                launch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
