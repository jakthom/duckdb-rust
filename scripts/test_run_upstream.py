"""Focused reporting and retry-selection tests for the G01 SQL campaign."""
import tempfile
import unittest
from pathlib import Path
import stat
import tarfile
from unittest.mock import patch

import sqllogic
from run_upstream import (cached_files, cached_manifest_matches_source, cached_population, comparable_outcome, extract_source_archive, failure_class,
                          rewrite_report_argument, run_case, selected_entries, summarize,
                          watch_feedback)


class RunUpstreamTests(unittest.TestCase):
    def test_failure_classes_do_not_turn_oracle_or_engine_blocks_into_passes(self):
        self.assertEqual(failure_class(sqllogic.Unsupported("test directive require: parquet")), "harness_directive_or_oracle")
        self.assertEqual(failure_class(sqllogic.Unsupported("unimplemented SQL function"), True), "engine_unsupported")
        self.assertEqual(failure_class(AssertionError("wrong error")), "assertion_or_error_mismatch")
        self.assertEqual(failure_class(TimeoutError("deadline")), "timeout")
        self.assertEqual(failure_class(RuntimeError("worker exited: signal 11")), "crash")
        self.assertEqual(failure_class(UnicodeDecodeError("utf8", b"", 0, 1, "bad"), phase="parse"), "harness_parse")
        self.assertEqual(failure_class(BrokenPipeError()), "crash")

    def test_selection_and_coverage_reject_missing_or_failed_ids(self):
        sql = [{"id": "a", "path": "a.test"}, {"id": "b", "path": "b.test"}]
        with tempfile.TemporaryDirectory() as d:
            paths = Path(d) / "retry.txt"; paths.write_text("b.test\n")
            self.assertEqual(selected_entries(sql, [], paths), [sql[1]])
        report = summarize(sql, sql, [{"id": "a", "status": "passed"}], [], {})
        self.assertFalse(report["sql_selection_passed"])
        self.assertFalse(report["sql_suite_passed"])
        report = summarize(sql, sql, [{"id": "a", "status": "passed"}, {"id": "b", "status": "failed", "failure_class": "engine_unsupported"}], [], {})
        self.assertFalse(report["sql_selection_passed"])
        self.assertEqual(report["failure_classes"], {"engine_unsupported": 1})

    def test_retry_selection_uses_only_prior_timeouts(self):
        sql = [{"id": "a", "path": "a.test"}, {"id": "b", "path": "b.test"}]
        with tempfile.TemporaryDirectory() as d:
            report = Path(d) / "prior.json"
            report.write_text('{"populations":{"development":{"results":[{"path":"b.test","failure_class":"timeout"}]}}}')
            self.assertEqual(selected_entries(sql, [], None, report, "development"), [sql[1]])

    def test_path_list_rejects_unknown_ids(self):
        with tempfile.TemporaryDirectory() as d:
            paths = Path(d) / "retry.txt"; paths.write_text("missing.test\n")
            with self.assertRaises(ValueError): selected_entries([{"id": "a", "path": "a.test"}], [], paths)
        with self.assertRaises(ValueError):
            selected_entries([{"id": "a", "path": "a.test"}], ["other/"], None)

    def test_run_case_counts_sent_sql_and_source_reach_honestly(self):
        with tempfile.TemporaryDirectory() as d:
            source, worker = Path(d) / "source", Path(d) / "worker"
            source.mkdir(); (source / "fail.test").write_text("statement ok\nFAIL\n\nstatement ok\nTAIL\n")
            (source / "loop.test").write_text("loop x 0 2\nstatement ok\nOK\n\nendloop\n")
            (source / "concurrent.test").write_text("concurrentloop x 0 2\nstatement ok\nOK {x}\n\nendloop\n")
            (source / "restart.test").write_text("restart\nstatement ok\nOK\n")
            (source / "empty.test").write_text("# only a comment\n")
            worker.write_text("#!/usr/bin/env python3\nimport json,sys\nfor line in sys.stdin:\n r=json.loads(line)\n if r['operation']=='concurrent': out={'ok':True,'streams':[[{'ok':True} for item in stream] for stream in r['streams']]}\n else:\n  ok='FAIL' not in r.get('sql',''); out={'ok':ok,'message':'failure'}\n print(json.dumps(out),flush=True)\n")
            worker.chmod(worker.stat().st_mode | stat.S_IXUSR)
            failed = run_case(worker, source, {"id":"f","path":"fail.test"}, 2)
            self.assertEqual((failed["attempted_records"], failed["worker_requests"], failed["unreached_source_records"]), (1, 1, 1))
            loop = run_case(worker, source, {"id":"l","path":"loop.test"}, 2)
            self.assertIsNone(loop["unreached_source_records"])
            concurrent = run_case(worker, source, {"id":"c","path":"concurrent.test"}, 2)
            self.assertEqual((concurrent["attempted_records"], concurrent["worker_requests"]), (2, 3))
            restarted = run_case(worker, source, {"id":"r","path":"restart.test"}, 2)
            self.assertEqual((restarted["attempted_records"], restarted["worker_requests"]), (1, 2))
            empty = run_case(worker, source, {"id":"e","path":"empty.test"}, 2)
            self.assertEqual(empty["failure_class"], "no_sql_records")

    def test_cache_tamper_is_rebuilt_from_the_population_not_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def population(target, temporary):
                self.assertTrue(temporary.is_dir())
                source = temporary / "tree"; source.mkdir(parents=True)
                (source / "case.test").write_text("statement ok\nSELECT 1\n")
                return source, {"tests": [], "counts": {}}, {"revision": "r"}
            with patch("run_upstream.cache_identity", return_value={"target": "release", "revision": "r"}), \
                 patch("run_upstream.archive_population", side_effect=population) as archive:
                source, _, identity = cached_population("release", root)
                self.assertEqual(identity["cache"], "created")
                (source / "case.test").write_text("tampered")
                rebuilt, _, identity = cached_population("release", root)
                self.assertEqual(identity["cache"], "created")
                self.assertEqual((rebuilt / "case.test").read_text(), "statement ok\nSELECT 1\n")
                self.assertEqual(archive.call_count, 2)

    def test_cached_suite_rejects_symlink_root_and_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"; source.mkdir()
            (source / "inside.test").write_text("SELECT 1")
            (source / "escape.test").symlink_to("../../outside.test")
            with self.assertRaises(ValueError): cached_files(source)
            link = root / "linked-source"; link.symlink_to(source, target_is_directory=True)
            with self.assertRaises(ValueError): cached_files(link)

    def test_cached_suite_json_cannot_rewrite_inventory_or_selection(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"; source.mkdir()
            path = source / "case.test"; path.write_text("statement ok\nSELECT 1\n")
            files = cached_files(source)
            manifest = {"files": files, "tests": [{"id": "case.test", "kind": "sqllogictest", "path": "case.test", "line": 1}],
                        "counts": {"sqllogictest": 1}}
            self.assertTrue(cached_manifest_matches_source(source, manifest))
            for changed in [{**manifest, "counts": {}}, {**manifest, "tests": []},
                            {**manifest, "files": [{**files[0], "kind": "symlink"}]}]:
                self.assertFalse(cached_manifest_matches_source(source, changed))

    def test_archive_extraction_allows_member_relative_link_but_rejects_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "source.tar"
            with tarfile.open(archive, "w") as contents:
                data = b"ok"
                entry = tarfile.TarInfo("data/csv/glob/crawl/symbolic_link")
                entry.size = len(data)
                import io
                contents.addfile(entry, io.BytesIO(data))
                link = tarfile.TarInfo("data/csv/glob/crawl/.symbolic_link/mydir/link_to_upper_dir")
                link.type = tarfile.SYMTYPE; link.linkname = "../../symbolic_link"
                contents.addfile(link)
            with tarfile.open(archive) as contents:
                extract_source_archive(contents, Path(directory) / "tree")
            self.assertEqual((Path(directory) / "tree/data/csv/glob/crawl/.symbolic_link/mydir/link_to_upper_dir").resolve().read_bytes(), b"ok")
            for name, target in [("absolute", "/etc/passwd"), ("escape", "../../outside")]:
                with tarfile.open(archive, "w") as contents:
                    link = tarfile.TarInfo("a/link"); link.type = tarfile.SYMTYPE; link.linkname = target
                    contents.addfile(link)
                with tarfile.open(archive) as contents:
                    with self.assertRaises(ValueError): extract_source_archive(contents, Path(directory) / name)

    def test_debug_release_comparison_includes_all_assertion_visible_fields(self):
        passed = {"id": "a", "path": "a.test", "status": "passed", "passed_records": 1,
                  "skipped_records": 0, "attempted_records": 1, "unreached_source_records": 0,
                  "source_sql_records": 1, "elapsed_seconds": 0.01}
        self.assertEqual(comparable_outcome(passed), comparable_outcome({**passed, "elapsed_seconds": 10}))
        self.assertNotEqual(comparable_outcome(passed), comparable_outcome({**passed, "status": "failed"}))

    def test_watch_reruns_settled_source_after_a_stale_child_and_rewrites_equals_report(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "watch.json"
            args = type("Args", (), {"report": report, "debounce_seconds": .01})()
            calls = []
            def child(command, **unused):
                calls.append(command)
                if len(calls) == 2:
                    raise KeyboardInterrupt()
            # First child starts at A and source changes to B.  The second must
            # run B after it settles; no additional mutation is supplied.
            fingerprints = iter(["A", "A", "B", "B", "B"])
            with patch("run_upstream.sys.argv", ["run_upstream.py", "--watch", f"--report={report}"]), \
                 patch("run_upstream.validation_fingerprint", side_effect=lambda: next(fingerprints)), \
                 patch("run_upstream.time.sleep"), patch("run_upstream.subprocess.run", side_effect=child):
                with self.assertRaises(KeyboardInterrupt): watch_feedback(args)
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[0][-1], f"--report={report}")
            self.assertEqual(calls[1][-1], f"--report={report.with_name('watch.watch-1.json')}")

    def test_report_argument_rewrite_rejects_missing_and_handles_split_form(self):
        command = ["--report", "old.json"]
        rewrite_report_argument(command, Path("new.json"))
        self.assertEqual(command, ["--report", "new.json"])
        with self.assertRaises(ValueError): rewrite_report_argument(["--jobs", "1"], Path("new.json"))


if __name__ == "__main__":
    unittest.main()
