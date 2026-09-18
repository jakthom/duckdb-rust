"""Focused reporting and retry-selection tests for the G01 SQL campaign."""
import tempfile
import unittest
from pathlib import Path
import stat
import tarfile
from unittest.mock import patch
import subprocess

import sqllogic
from run_upstream import (cached_files, cached_manifest_matches_source, cached_population, comparable_outcome, extract_source_archive, failure_class, main,
                          rewrite_report_argument, run_case, selected_entries, selected_feedback_population, selected_path_list, summarize,
                          watch_feedback, checked_worker_provenance, record_release_worker_provenance, worker_provenance_path, worker_source_digest)


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

    def test_selected_path_list_rejects_empty_duplicates_and_traversal(self):
        with tempfile.TemporaryDirectory() as directory:
            paths = Path(directory) / "paths"
            for content in ("", "a.test\na.test\n", "../a.test\n", "/a.test\n", "a//b.test\n"):
                paths.write_text(content)
                with self.assertRaises(ValueError): selected_path_list(paths)
            paths.write_text("# selected edit\na.test\n")
            self.assertEqual(selected_path_list(paths), ["a.test"])

    def test_selected_feedback_cache_validates_bytes_metadata_and_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); data = b"statement ok\nSELECT 1\n"
            with patch("run_upstream.selected_source_file", return_value=("pin-a", data)) as source_file:
                source, manifest, identity = selected_feedback_population("release", ["test/sql/case.test"], root)
                self.assertEqual(identity["cache"], "created")
                self.assertEqual(manifest["tests"][0]["path"], "test/sql/case.test")
                self.assertEqual(sorted(p.relative_to(source).as_posix() for p in source.rglob("*") if p.is_file()), ["test/sql/case.test"])
                _, _, identity = selected_feedback_population("release", ["test/sql/case.test"], root)
                self.assertEqual(identity["cache"], "validated")
                (source / "test/sql/case.test").write_text("tampered")
                rebuilt, _, identity = selected_feedback_population("release", ["test/sql/case.test"], root)
                self.assertEqual(identity["cache"], "created")
                self.assertEqual((rebuilt / "test/sql/case.test").read_bytes(), data)
                metadata = next(root.glob("selected-feedback/release/*/suite.json"))
                saved = __import__("json").loads(metadata.read_text()); saved["manifest"]["counts"] = {}
                metadata.write_text(__import__("json").dumps(saved))
                _, _, identity = selected_feedback_population("release", ["test/sql/case.test"], root)
                self.assertEqual(identity["cache"], "created")
                self.assertGreaterEqual(source_file.call_count, 4)
            with patch("run_upstream.selected_source_file", side_effect=[("pin-a", data), ("pin-b", data)]):
                with self.assertRaisesRegex(ValueError, "revision changed"):
                    selected_feedback_population("release", ["a.test", "b.test"], root)

    def test_selected_feedback_cache_is_order_independent_but_execution_is_not(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def source(target, path): return "pin", b"statement ok\nSELECT 1\n"
            with patch("run_upstream.selected_source_file", side_effect=source):
                _, _, first = selected_feedback_population("release", ["b.test", "a.test"], root)
                _, _, second = selected_feedback_population("release", ["a.test", "b.test"], root)
            self.assertEqual(first["cache"], "created")
            self.assertEqual(second["cache"], "validated")
            sql = [{"id": "a.test", "path": "a.test"}, {"id": "b.test", "path": "b.test"}]
            paths = root / "paths"; paths.write_text("b.test\na.test\n")
            self.assertEqual([item["path"] for item in selected_entries(sql, [], paths)], ["b.test", "a.test"])

    def test_selected_development_source_rejects_untrusted_manifest(self):
        from run_upstream import selected_source_file
        with patch("run_upstream.digest", return_value="tampered"):
            with self.assertRaisesRegex(ValueError, "manifest digest"):
                selected_source_file("development", "test/sql/cte/cte_schema.test")

    def test_selected_feedback_does_not_supply_an_unlisted_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sql = b"statement ok\nCOPY t FROM '__SOURCE_DIR__/fixture.csv'\n"
            with patch("run_upstream.selected_source_file", return_value=("pin-a", sql)):
                source, manifest, _ = selected_feedback_population("release", ["case.test"], root)
            self.assertFalse((source / "fixture.csv").exists())
            worker = root / "worker"
            worker.write_text("#!/usr/bin/env python3\nimport json,sys\nfor line in sys.stdin:\n r=json.loads(line); print(json.dumps({'ok': 'fixture.csv' not in r.get('sql',''), 'message': 'missing fixture'}), flush=True)\n")
            worker.chmod(worker.stat().st_mode | stat.S_IXUSR)
            outcome = run_case(worker, source, manifest["tests"][0], 2)
            self.assertEqual(outcome["status"], "failed")

    def test_prebuilt_path_list_uses_selected_feedback_not_full_population(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); source = root / "source"; source.mkdir()
            (source / "case.test").write_text("statement ok\nSELECT 1\n")
            worker, paths, report = root / "worker", root / "paths", root / "report.json"
            worker.write_text("#!/usr/bin/env python3\nimport json,sys\nfor line in sys.stdin: print(json.dumps({'ok':True}),flush=True)\n")
            worker.chmod(worker.stat().st_mode | stat.S_IXUSR); paths.write_text("case.test\n")
            worker_provenance_path(worker).write_text(__import__("json").dumps({"profile": "release", "source_sha256": worker_source_digest(), "binary_sha256": __import__("hashlib").sha256(worker.read_bytes()).hexdigest()}))
            manifest = {"files": cached_files(source), "tests": [{"id":"case.test", "kind":"sqllogictest", "path":"case.test", "line":1}], "counts":{"sqllogictest":1}}
            with patch("run_upstream.cached_population", side_effect=AssertionError("must not materialize full suite")), \
                 patch("run_upstream.selected_feedback_population", return_value=(source, manifest, {"kind":"selected_feedback", "revision":"pin", "cache":"validated"})), \
                 patch("run_upstream.sys.argv", ["run_upstream.py", "--target", "release", "--worker", str(worker), "--path-list", str(paths), "--report", str(report), "--suite-cache", str(root / "cache")]):
                main()
            saved = __import__("json").loads(report.read_text())
            self.assertEqual(saved["campaign_kind"], "selected-feedback")
            self.assertFalse(saved["populations"]["release"]["full_suite_passed"])

    def test_prebuilt_failure_persists_report_then_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); source = root / "source"; source.mkdir()
            (source / "case.test").write_text("statement ok\nSELECT 1\n")
            worker, paths, report = root / "worker", root / "paths", root / "failed.json"
            worker.write_text("#!/usr/bin/env python3\nimport json,sys\nfor line in sys.stdin: print(json.dumps({'ok':False,'message':'wrong'}),flush=True)\n")
            worker.chmod(worker.stat().st_mode | stat.S_IXUSR); paths.write_text("case.test\n")
            worker_provenance_path(worker).write_text(__import__("json").dumps({"profile":"release", "source_sha256":worker_source_digest(), "binary_sha256":__import__("hashlib").sha256(worker.read_bytes()).hexdigest()}))
            manifest = {"files": cached_files(source), "tests": [{"id":"case.test", "kind":"sqllogictest", "path":"case.test", "line":1}], "counts":{"sqllogictest":1}}
            with patch("run_upstream.selected_feedback_population", return_value=(source, manifest, {"kind":"selected_feedback", "revision":"pin", "cache":"validated"})), \
                 patch("run_upstream.sys.argv", ["run_upstream.py", "--target", "release", "--worker", str(worker), "--path-list", str(paths), "--report", str(report), "--suite-cache", str(root / "cache")]):
                with self.assertRaisesRegex(RuntimeError, "did not pass"):
                    main()
            self.assertEqual(__import__("json").loads(report.read_text())["populations"]["release"]["results"][0]["status"], "failed")

    def test_prebuilt_worker_requires_matching_current_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"; worker.write_bytes(b"worker")
            sidecar = worker_provenance_path(worker)
            sidecar.write_text('{"profile":"release","source_sha256":"stale","binary_sha256":"stale"}')
            with self.assertRaisesRegex(ValueError, "stale"):
                checked_worker_provenance(worker)

    def test_provenance_record_builds_only_canonical_release_worker(self):
        expected = Path(__import__("run_upstream").ROOT) / "target/release/duckdb-rust-test-worker"
        with patch("run_upstream.subprocess.run") as build, \
             patch("run_upstream.write_worker_provenance", return_value=(Path("sidecar"), {})) as write:
            self.assertEqual(record_release_worker_provenance(expected)[0], Path("sidecar"))
            build.assert_called_once_with(["cargo", "build", "--offline", "--release", "--no-default-features", "--bin", "duckdb-rust-test-worker"], cwd=__import__("run_upstream").ROOT, check=True)
            write.assert_called_once_with(expected, "release")
        with patch("run_upstream.subprocess.run") as build:
            with self.assertRaisesRegex(ValueError, "only attest"):
                record_release_worker_provenance(Path("/tmp/foreign-worker"))
            build.assert_not_called()
        with patch("run_upstream.subprocess.run", side_effect=subprocess.CalledProcessError(1, "cargo")), \
             patch("run_upstream.write_worker_provenance") as write:
            with self.assertRaises(subprocess.CalledProcessError):
                record_release_worker_provenance(expected)
            write.assert_not_called()

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
                 patch("run_upstream.validation_fingerprint", side_effect=lambda *unused: next(fingerprints)), \
                 patch("run_upstream.time.sleep"), patch("run_upstream.subprocess.run", side_effect=child):
                with self.assertRaises(KeyboardInterrupt): watch_feedback(args)
            self.assertEqual(len(calls), 2)
            self.assertIn(f"--report={report}", calls[0])
            self.assertIn(f"--report={report.with_name('watch.watch-1.json')}", calls[1])
            self.assertIn("--feedback-watch-child", calls[0])

    def test_report_argument_rewrite_rejects_missing_and_handles_split_form(self):
        command = ["--report", "old.json"]
        rewrite_report_argument(command, Path("new.json"))
        self.assertEqual(command, ["--report", "new.json"])
        with self.assertRaises(ValueError): rewrite_report_argument(["--jobs", "1"], Path("new.json"))


if __name__ == "__main__":
    unittest.main()
