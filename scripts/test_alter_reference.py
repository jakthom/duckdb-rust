"""Regression coverage for independent ALTER corpus and exchange reporting."""
import tempfile
import unittest
from dataclasses import dataclass
from pathlib import Path
from unittest.mock import patch

import alter_reference


@dataclass
class Engine:
    arguments: tuple = ()


class AlterReferenceReportingTests(unittest.TestCase):
    rust = Engine()
    reference = object()

    def configuration(self, durability, corpus, exchange):
        def record_corpus(*args, **kwargs):
            outcomes = args[3]
            outcomes.append({"ordinal": 17, "passed": False,
                             "error": {"message": corpus}})
            raise AssertionError(corpus)

        with tempfile.TemporaryDirectory() as temporary:
            with patch.object(alter_reference, "verify_corpus", side_effect=record_corpus), \
                 patch.object(alter_reference, "verify", return_value=exchange) as verify:
                result = alter_reference.run_configuration(
                    self.rust, self.reference, object(), Path(temporary), durability
                )
        return result, verify

    def test_release_and_development_divergences_remain_failures_but_run_exchange(self):
        exchange = {"configurations": [{"passed": True}], "native_wal": {"passed": True}}
        for durability, divergence in [
            ("checkpoint", "release accepts DROP NOT NULL"),
            ("wal", "development accepts ADD COLUMN NOT NULL"),
        ]:
            with self.subTest(durability=durability):
                result, verify = self.configuration(durability, divergence, exchange.copy())
                self.assertFalse(result["corpus_passed"])
                self.assertIn(divergence, result["corpus_error"])
                self.assertEqual(len(result["records"]), 1)
                self.assertTrue(result["exchange"]["passed"])
                self.assertFalse(result["passed"])
                verify.assert_called_once()

    def test_wrong_exchange_result_is_fail_closed_even_when_corpus_passes(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.object(alter_reference, "verify_corpus", return_value=[]), \
                 patch.object(alter_reference, "verify", side_effect=AssertionError("wrong sum")):
                result = alter_reference.run_configuration(
                    self.rust, self.reference, object(), Path(temporary), "checkpoint"
                )
        self.assertTrue(result["corpus_passed"])
        self.assertFalse(result["exchange"]["passed"])
        self.assertEqual(result["exchange"]["error"]["message"], "wrong sum")
        self.assertFalse(result["passed"])


if __name__ == "__main__":
    unittest.main()
