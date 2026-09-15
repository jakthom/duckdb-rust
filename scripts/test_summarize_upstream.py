import unittest

from summarize_upstream import FIXTURE, merge_population


def report(target, results, selected=None, source="s", binary="b"):
    selected = selected or [{"path": r["path"]} for r in results]
    return {"engine_git_revision": "git", "rust_source_sha256": source, "rust_binary_sha256": binary,
            "harness_sha256": {"runner": "h"}, "populations": {target: {"identity": {"revision": "r", "archive_sha256": "a"}, "selected": selected, "results": results}}}


class SummaryTests(unittest.TestCase):
    def test_retry_replaces_only_timeout_and_normalizes_fixture_and_empty(self):
        initial_rows = [{"path": "a.test", "status": "failed", "failure_class": "timeout", "source_sql_records": 2, "passed_records": 0, "skipped_records": 0},
                        {"path": "empty.test", "status": "incomplete", "failure_class": "conditional_skip", "source_sql_records": 0, "attempted_records": 0, "passed_records": 0, "skipped_records": 0},
                        {"path": FIXTURE, "status": "failed", "failure_class": "harness_parse"}]
        retried = [{"path": "a.test", "status": "passed", "source_sql_records": 2, "passed_records": 2, "skipped_records": 0}]
        merged = merge_population(report("development", initial_rows), report("development", retried), "development")
        self.assertEqual((merged["candidate_file_count"], merged["executable_file_count"]), (3, 2))
        self.assertEqual(merged["effective"]["status"], {"passed": 1, "incomplete": 1})
        self.assertEqual(merged["effective"]["first_blocker"]["no_sql_records"], 1)

    def test_rejects_missing_extra_and_changed_identity_retry(self):
        rows = [{"path": "a.test", "status": "failed", "failure_class": "timeout"}, {"path": FIXTURE, "status": "failed"}]
        for retry in [[], [{"path": "a.test", "status": "passed"}, {"path": "extra.test", "status": "passed"}]]:
            with self.assertRaises(ValueError): merge_population(report("development", rows), report("development", retry), "development")
        with self.assertRaises(ValueError): merge_population(report("development", rows), report("development", [{"path":"a.test","status":"passed"}], source="other"), "development")


if __name__ == "__main__": unittest.main()
