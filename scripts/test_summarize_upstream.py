import unittest

from summarize_upstream import FIXTURE, classify_incremental, merge_population, observations
from reference_version import TARGETS


def report(target, results, selected=None, source="s", binary="b", revision=None):
    selected = selected or [{"path": r["path"]} for r in results]
    return {"engine_git_revision": "git", "rust_source_sha256": source, "rust_binary_sha256": binary,
            "harness_sha256": {"runner": "h"}, "populations": {target: {"identity": {"revision": revision or TARGETS[target].revision, "archive_sha256": "a"}, "selected": selected, "results": results}}}


class SummaryTests(unittest.TestCase):
    def incremental_report(self, rows, *, target="development", config="ordinary", source="s", binary="b", stale=False, selected=None, archive="a"):
        selected = selected if selected is not None else [{"path": row["path"]} for row in rows]
        return {"engine_git_revision": "git", "rust_source_sha256": source, "rust_binary_sha256": binary,
                "harness_sha256": {"runner": "h"}, "campaign_kind": "suite-campaign", "selection_kind": "suite",
                "worker_profile": "release", "timeout_seconds": 10, "execution_mode": config, "path_prefixes": [],
                "source_fingerprint_before": "same", "source_fingerprint_after": "same", "stale_source": stale,
                "populations": {target: {"identity": {"revision": TARGETS[target].revision, "archive_sha256": archive},
                                           "selected": selected, "results": rows}}}

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

    def test_rejects_duplicate_ids_and_wrong_pin_without_mutating_inputs(self):
        rows = [{"path": "a.test", "status": "failed", "failure_class": "timeout"}, {"path": FIXTURE, "status": "failed"}]
        retry = report("development", [{"path": "a.test", "status": "passed"}])
        initial = report("development", rows)
        before = repr(initial)
        merge_population(initial, retry, "development")
        self.assertEqual(repr(initial), before)
        duplicate = report("development", rows + [dict(rows[0])])
        with self.assertRaises(ValueError): merge_population(duplicate, retry, "development")
        duplicate_retry = report("development", [{"path": "a.test", "status": "passed"}], selected=[{"path": "a.test"}, {"path": "a.test"}])
        with self.assertRaises(ValueError): merge_population(initial, duplicate_retry, "development")
        bad_pin = report("development", rows, revision="bad")
        with self.assertRaises(ValueError): merge_population(bad_pin, retry, "development")

    def test_preserves_failed_zero_sql_and_prefers_modern_worker_counter(self):
        rows = [{"path": "a.test", "status": "failed", "failure_class": "timeout"},
                {"path": "failed-empty.test", "status": "failed", "failure_class": "harness_directive_or_oracle", "source_sql_records": 0, "attempted_records": 3},
                {"path": "controls.test", "status": "incomplete", "source_sql_records": 0, "attempted_records": 0, "worker_requests": 2},
                {"path": FIXTURE, "status": "failed"}]
        merged = merge_population(report("development", rows), report("development", [{"path":"a.test", "status":"passed"}]), "development")
        output = {r["path"]: r for r in merged["results"]}
        self.assertEqual(output["failed-empty.test"]["status"], "failed")
        self.assertEqual(output["controls.test"]["failure_class"], "controls_only")

    def test_merge_population_accepts_missing_retry_when_initial_has_no_timeouts(self):
        rows = [{"path": "a.test", "status": "passed"}, {"path": FIXTURE, "status": "failed"}]
        self.assertEqual(merge_population(report("development", rows), None, "development")["effective"]["status"], {"passed": 1})
        with self.assertRaises(ValueError): merge_population(report("development", rows), report("development", [{"path": "a.test", "status": "passed"}]), "development")
        with self.assertRaises(ValueError): merge_population(report("development", rows, revision="wrong"), None, "development")

    def test_incremental_view_keys_case_pin_and_configuration(self):
        baseline = self.incremental_report([{"path": "a.test", "status": "passed"}], config="one")
        current = self.incremental_report([{"path": "a.test", "status": "failed"}], config="one")
        result = classify_incremental(baseline, current)
        self.assertEqual(result["counts"], {"fresh_failure": 1})
        # The same path on another pin/configuration is a distinct observation.
        both = self.incremental_report([{"path": "a.test", "status": "passed"}], target="release", config="two")
        self.assertEqual(len(observations(both)), 1)

    def test_incremental_view_marks_current_omission_as_lost_pass(self):
        baseline = self.incremental_report([{"path": "a.test", "status": "passed"}])
        current = self.incremental_report([])
        result = classify_incremental(baseline, current)
        self.assertEqual(result["rows"][0]["classification"], "lost_pass_omitted")
        self.assertTrue(result["rows"][0]["lost_pass"])

    def test_incremental_view_rejects_duplicate_and_omitted_selection_or_result(self):
        duplicate = self.incremental_report([{"path": "a.test", "status": "passed"}], selected=[{"path": "a.test"}, {"path": "a.test"}])
        with self.assertRaises(ValueError): observations(duplicate)
        omitted = self.incremental_report([], selected=[{"path": "a.test"}])
        with self.assertRaises(ValueError): observations(omitted)

    def test_incremental_view_rejects_overlapping_same_snapshot_reports(self):
        report_one = self.incremental_report([{"path": "a.test", "status": "passed"}])
        report_one["populations"]["development"]["results"].append({"path": "a.test", "status": "failed"})
        with self.assertRaises(ValueError): observations(report_one)

    def test_incremental_view_allows_changed_rust_identity_and_reports_own_staleness(self):
        baseline = self.incremental_report([{"path": "a.test", "status": "passed"}], source="old", binary="old")
        current = self.incremental_report([{"path": "a.test", "status": "passed"}], source="new", binary="new", stale=True)
        result = classify_incremental(baseline, current)
        self.assertEqual(result["rows"][0]["classification"], "stale")
        bad = self.incremental_report([{"path": "a.test", "status": "passed"}])
        bad["source_fingerprint_after"] = "changed"
        self.assertEqual(classify_incremental(baseline, bad)["rows"][0]["classification"], "stale")
        bad["populations"]["development"]["identity"]["revision"] = "bad"
        with self.assertRaises(ValueError): observations(bad)

    def test_incremental_view_marks_runtime_configuration_change_uncomparable(self):
        baseline = self.incremental_report([{"path": "a.test", "status": "passed"}], config="ordinary")
        current = self.incremental_report([{"path": "a.test", "status": "failed"}], config="debug")
        result = classify_incremental(baseline, current)
        self.assertEqual(result["counts"], {"uncomparable_configuration": 1})
        self.assertFalse(result["rows"][0]["fresh_failure"])

    def test_incremental_view_elapsed_stages(self):
        # Per-result durations remain observations; the accounting view never sums retry rows.
        baseline = self.incremental_report([{"path": "a.test", "status": "passed", "elapsed_seconds": 0}])
        current = self.incremental_report([{"path": "a.test", "status": "passed", "elapsed_seconds": 2.5}])
        current["elapsed_stages"] = {"worker_preparation_seconds": 0.2}
        current["populations"]["development"]["elapsed_stages"] = {"execution_seconds": 2.6}
        result = classify_incremental(baseline, current)
        self.assertEqual(result["counts"], {"unchanged_passed": 1})
        self.assertEqual((result["rows"][0]["baseline_elapsed_seconds"], result["rows"][0]["current_elapsed_seconds"]), (0, 2.5))
        self.assertIsNone(result["elapsed_stages"]["baseline"]["report"])
        self.assertEqual(result["elapsed_stages"]["current"]["report"], {"worker_preparation_seconds": 0.2})
        self.assertEqual(result["elapsed_stages"]["current"]["populations"]["development"], {"execution_seconds": 2.6})
        current["elapsed_stages"]["worker_preparation_seconds"] = -1
        with self.assertRaises(ValueError): classify_incremental(baseline, current)

    def test_incremental_rejects_missing_identity_unknown_pin_and_invalid_result(self):
        import copy
        valid = self.incremental_report([{"path": "a.test", "status": "passed"}])
        for field in ("rust_source_sha256", "rust_binary_sha256", "engine_git_revision", "harness_sha256"):
            bad = copy.deepcopy(valid)
            del bad[field]
            with self.assertRaises(ValueError): observations(bad)
        bad = copy.deepcopy(valid)
        bad["populations"]["unknown"] = bad["populations"].pop("development")
        with self.assertRaises(ValueError): observations(bad)
        for status, elapsed in (("mystery", 1), ("passed", -1), ("passed", float("nan"))):
            bad = copy.deepcopy(valid)
            bad["populations"]["development"]["results"][0].update(status=status, elapsed_seconds=elapsed)
            with self.assertRaises(ValueError): observations(bad)

    def test_selected_feedback_refresh_compares_overlap_with_exact_file_identities(self):
        baseline = self.incremental_report([{"path": "a.test", "status": "passed"}, {"path": "b.test", "status": "passed"}])
        current = self.incremental_report([{"path": "a.test", "status": "failed"}])
        current["selection_kind"] = "selected-feedback"
        for report_value, cache in ((baseline, "created"), (current, "validated")):
            population = report_value["populations"]["development"]
            population["identity"] = {"kind": "selected_feedback", "revision": TARGETS["development"].revision,
                                      "files": [{"path": row["path"], "kind": "file", "sha256": "same-bytes"} for row in population["selected"]], "cache": cache}
        result = classify_incremental(baseline, current)
        self.assertEqual(result["counts"], {"fresh_failure": 1, "lost_pass_omitted": 1})
        current["populations"]["development"]["identity"]["files"][0]["sha256"] = "changed-bytes"
        self.assertEqual(classify_incremental(baseline, current)["rows"][0]["classification"], "uncomparable_population")


if __name__ == "__main__": unittest.main()
