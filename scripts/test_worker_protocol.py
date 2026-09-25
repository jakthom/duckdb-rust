"""Fault and real-process coverage for the worker startup transport."""
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

import worker_protocol as protocol


class _Stream:
    def __init__(self, failure=None): self.failure, self.closed = failure, False
    def fileno(self): return 91
    def close(self):
        self.closed = True
        if self.failure: raise self.failure


class _Process:
    def __init__(self):
        self.stdin, self.stdout = _Stream(), _Stream(); self.killed = self.waited = False
    def poll(self): return None
    def kill(self): self.killed = True
    def wait(self): self.waited = True; return 0


class WorkerProtocolTests(unittest.TestCase):
    def reader(self, chunks, deadline=float("inf"), spawned=True):
        engine = object.__new__(protocol.RustEngine)
        engine.deadline, engine._buffer, engine._spawned = deadline, bytearray(), spawned
        engine.process = SimpleNamespace(stdout=SimpleNamespace(fileno=lambda: 91))
        engine.errors = io.StringIO("worker diagnostic")
        engine._selector = Mock()
        engine._selector.select.return_value = [object()]
        return engine, patch("os.read", side_effect=iter(chunks))

    def test_partial_ready_waits_for_newline_and_decodes_utf8(self):
        engine, read = self.reader([b'{"ready":', b'true}\n'])
        with read, patch("select.select", return_value=([object()], [], [])):
            engine._expect_ready()
        self.assertEqual(engine._buffer, b"")

    def test_ready_rejects_duplicate_keys_numeric_true_and_extra_keys(self):
        for value in (b'{"ready":1}\n', b'{"ready":true,"ready":true}\n', b'{"ready":true,"x":0}\n'):
            engine, read = self.reader([value])
            with self.subTest(value=value), read, patch("select.select", return_value=([object()], [], [])):
                with self.assertRaises(RuntimeError): engine._expect_ready()

    def test_partial_ready_deadline_and_eof_preserve_existing_diagnostic_cap(self):
        engine, read = self.reader([b'{"ready"'])
        with read, patch("select.select", return_value=([], [], [])):
            with self.assertRaises(TimeoutError): engine._expect_ready()
        engine, read = self.reader([b""])
        with read, patch("select.select", return_value=([object()], [], [])):
            with self.assertRaisesRegex(RuntimeError, "worker diagnostic"): engine._expect_ready()

    def test_expired_request_does_not_mutate_counters_or_write(self):
        engine = object.__new__(protocol.RustEngine)
        engine.deadline = 0; engine.worker_requests = engine.sql_requests = 0
        engine.process = SimpleNamespace(stdin=Mock())
        with self.assertRaises(TimeoutError): engine.request({"operation": "statement", "sql": "DELETE FROM t"})
        engine.process.stdin.write.assert_not_called()
        self.assertEqual((engine.worker_requests, engine.sql_requests), (0, 0))

    def test_constructor_consumes_ready_for_zero_request_worker_on_spawn_and_fallback(self):
        for spawned in (True, False):
            process = _Process()
            with self.subTest(spawned=spawned), tempfile.TemporaryDirectory() as directory, patch.object(
                protocol, "_anonymous_errors", return_value=tempfile.TemporaryFile(mode="w+t")
            ), patch.object(protocol, "_can_spawn", return_value=spawned), patch.object(
                protocol, "_spawn" if spawned else "_popen", return_value=process
            ), patch("os.set_blocking"), patch("selectors.DefaultSelector", return_value=Mock()), patch.object(protocol.RustEngine, "_expect_ready") as ready:
                engine = protocol.RustEngine("/worker", directory, float("inf"))
                ready.assert_called_once(); self.assertEqual(engine.worker_requests, 0)
                engine.close()

    def test_low_standard_fd_is_duplicated_with_public_fcntl(self):
        with patch("fcntl.fcntl", return_value=7) as duplicate, patch("os.close") as close:
            self.assertEqual(protocol._normalize_fd(1), 7)
        duplicate.assert_called_once(); close.assert_called_once_with(1)

    def test_close_attempts_every_resource_and_reports_first_failure(self):
        engine = object.__new__(protocol.RustEngine)
        process = _Process(); process.kill = Mock(side_effect=OSError("kill")); process.wait = Mock(side_effect=OSError("wait"))
        process.stdin = _Stream(OSError("stdin")); process.stdout = _Stream(OSError("stdout"))
        selector, errors = _Stream(OSError("selector")), _Stream(OSError("errors"))
        engine.process, engine._selector, engine.errors = process, selector, errors
        with self.assertRaisesRegex(OSError, "kill"): engine.close()
        self.assertTrue(process.stdin.closed and process.stdout.closed and selector.closed and errors.closed)

    def test_constructor_chains_cleanup_without_replacing_startup_error(self):
        process = _Process(); process.kill = Mock(side_effect=OSError("cleanup"))
        with tempfile.TemporaryDirectory() as directory, patch.object(protocol, "_anonymous_errors", return_value=tempfile.TemporaryFile(mode="w+t")), patch.object(
            protocol, "_can_spawn", return_value=True), patch("os.set_blocking"), patch.object(protocol, "_spawn", return_value=process), patch.object(
            protocol.RustEngine, "_expect_ready", side_effect=RuntimeError("startup")):
            with self.assertRaisesRegex(RuntimeError, "startup") as raised:
                protocol.RustEngine("/worker", directory, float("inf"))
        self.assertIsInstance(raised.exception.__cause__, OSError)

class RealTransportTests(unittest.TestCase):
    def worker(self, directory):
        import stat, sys
        path = os.path.join(directory, "fake-worker")
        Path(path).write_text("#!" + sys.executable + "\n"
            "import json,os,sys\n"
            "assert sys.argv[1] == '--working-directory'\n"
            "os.chdir(sys.argv[2])\n"
            "print(json.dumps({'ready':True}),flush=True)\n"
            "for line in sys.stdin:\n"
            " r=json.loads(line); print(json.dumps({'ok':True,'cwd':os.getcwd(),'text':'hé'}),flush=True)\n")
        os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)
        return path

    def test_real_spawn_and_forced_popen_preserve_cwd_unicode_and_reap(self):
        for spawned in (True, False):
            with self.subTest(spawned=spawned), tempfile.TemporaryDirectory() as directory, patch.object(
                protocol, "_anonymous_errors", return_value=tempfile.TemporaryFile(mode="w+t")
            ), patch.object(protocol, "_can_spawn", return_value=spawned):
                engine = protocol.RustEngine(self.worker(directory), directory, protocol.time.monotonic() + 10)
                response = engine.request({"operation": "query"})
                self.assertEqual(response["cwd"], os.path.realpath(directory))
                self.assertEqual(response["text"], "hé")
                process = engine.process
                engine.close()
                self.assertIsNotNone(process.poll())

    def test_real_spawn_partial_ready_respects_deadline_and_cleanup(self):
        import stat, sys
        with tempfile.TemporaryDirectory() as directory, patch.object(protocol, "_anonymous_errors", return_value=tempfile.TemporaryFile(mode="w+t")), patch.object(protocol, "_can_spawn", return_value=True):
            path = os.path.join(directory, "partial-worker")
            Path(path).write_text("#!" + sys.executable + "\nimport sys,time\nsys.stdout.write('{\\\"ready\\\":');sys.stdout.flush();time.sleep(5)\n")
            os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)
            observed = []
            original_read = os.read
            def read(descriptor, size):
                chunk = original_read(descriptor, size)
                observed.append(chunk)
                return chunk
            with patch("os.read", side_effect=read), self.assertRaises(TimeoutError):
                protocol.RustEngine(path, directory, protocol.time.monotonic() + 0.5)
            self.assertTrue(any(b"ready" in chunk for chunk in observed))

    def test_real_worker_eof_limits_diagnostic_to_2000_characters(self):
        import stat, sys
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exit-worker"
            path.write_text("#!" + sys.executable + "\nimport sys\nsys.stderr.write('x' * 5000)\n")
            path.chmod(path.stat().st_mode | stat.S_IXUSR)
            with self.assertRaises(RuntimeError) as caught:
                protocol.RustEngine(path, directory, protocol.time.monotonic() + 10)
            self.assertEqual(str(caught.exception), "worker exited: " + "x" * 2000)

    def test_wrapper_failure_happens_before_spawn_and_all_pipe_fds_close(self):
        pipes = []
        real_pipe = os.pipe
        def pipe():
            pair = real_pipe(); pipes.extend(pair); return pair
        with tempfile.TemporaryFile(mode="w+t") as errors, patch("os.pipe", side_effect=pipe), patch(
            "io.TextIOWrapper", side_effect=OSError("wrapper")
        ), patch("os.posix_spawn") as spawn:
            with self.assertRaisesRegex(OSError, "wrapper"):
                protocol._spawn("/worker", "/tmp", errors)
        spawn.assert_not_called()
        for descriptor in pipes:
            with self.assertRaises(OSError): os.fstat(descriptor)

    def test_spawn_failure_closes_parent_pipe_ends_without_child(self):
        pipes = []
        real_pipe = os.pipe
        def pipe():
            pair = real_pipe(); pipes.extend(pair); return pair
        with tempfile.TemporaryFile(mode="w+t") as errors, patch("os.pipe", side_effect=pipe), patch(
            "os.posix_spawn", side_effect=OSError("spawn")
        ):
            with self.assertRaisesRegex(OSError, "spawn"):
                protocol._spawn("/worker", "/tmp", errors)
        for descriptor in pipes:
            with self.assertRaises(OSError): os.fstat(descriptor)

    def test_spawn_failure_closes_duplicated_stderr_exactly_once(self):
        duplicated = []
        real_dup = os.dup
        def duplicate(descriptor):
            result = real_dup(descriptor)
            duplicated.append(result)
            return result
        with tempfile.TemporaryFile(mode="w+t") as errors, patch(
            "os.dup", side_effect=duplicate
        ), patch("os.close", wraps=os.close) as close, patch(
            "os.posix_spawn", side_effect=OSError("spawn")
        ):
            with self.assertRaisesRegex(OSError, "spawn"):
                protocol._spawn("/worker", "/tmp", errors)
            self.assertEqual(len(duplicated), 1)
            self.assertEqual(sum(call.args == (duplicated[0],) for call in close.call_args_list), 1)

    def test_actual_spawn_with_closed_standard_fds_is_isolated(self):
        import subprocess, sys
        with tempfile.TemporaryDirectory() as directory:
            result = os.path.join(directory, "result")
            worker = self.worker(directory)
            program = """import os,sys
sys.path.insert(0, sys.argv[2])
for fd in (0,1,2):
 try: os.close(fd)
 except OSError: pass
import worker_protocol
engine = worker_protocol.RustEngine(sys.argv[3], sys.argv[4], worker_protocol.time.monotonic() + 10)
try:
 response = engine.request({'operation':'query'})
finally:
 engine.close()
with open(sys.argv[1], 'w') as output: output.write(response['text'])
"""
            subprocess.run([sys.executable, "-c", program, result, os.path.dirname(protocol.__file__), worker, directory],
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           check=True)
            self.assertEqual(Path(result).read_text(), "hé")

class SpawnOwnershipFaultTests(unittest.TestCase):
    def test_second_normalization_failure_closes_first_pipe_ownership(self):
        created, calls = [], []
        real_pipe = os.pipe
        def pipe():
            pair = real_pipe(); created.extend(pair); return pair
        def normalize(descriptor):
            calls.append(descriptor)
            if len(calls) == 2: raise OSError("second normalize")
            return descriptor
        with tempfile.TemporaryFile(mode="w+t") as errors, patch("os.pipe", side_effect=pipe), patch.object(
            protocol, "_normalize_fd", side_effect=normalize
        ):
            with self.assertRaisesRegex(OSError, "second normalize"):
                protocol._spawn("/worker", "/tmp", errors)
        for descriptor in created:
            with self.assertRaises(OSError): os.fstat(descriptor)

    def test_spawned_wrapper_construction_failure_kills_and_reaps_raw_pid(self):
        with tempfile.TemporaryFile(mode="w+t") as errors, patch("os.posix_spawn", return_value=741), patch.object(
            protocol, "_Spawned", side_effect=OSError("spawned wrapper")
        ), patch("os.kill") as kill, patch("os.waitpid") as wait:
            with self.assertRaisesRegex(OSError, "spawned wrapper"):
                protocol._spawn("/worker", "/tmp", errors)
        kill.assert_called_once_with(741, 9)
        wait.assert_called_once_with(741, 0)

    def test_scalar_list_and_null_ready_records_are_rejected(self):
        for value in (b"null\n", b"[]\n", b"1\n"):
            engine = object.__new__(protocol.RustEngine)
            engine.deadline, engine._buffer, engine._spawned = float("inf"), bytearray(), True
            engine.process = SimpleNamespace(stdout=SimpleNamespace(fileno=lambda: 92))
            engine.errors = io.StringIO()
            with self.subTest(value=value), patch("os.read", return_value=value), patch(
                "select.select", return_value=([object()], [], [])
            ):
                with self.assertRaises(RuntimeError): engine._expect_ready()

    def test_real_posix_write_deadline_kills_worker_that_never_reads(self):
        import stat, sys
        with tempfile.TemporaryDirectory() as directory, patch.object(
            protocol, "_anonymous_errors", return_value=tempfile.TemporaryFile(mode="w+t")
        ), patch.object(protocol, "_can_spawn", return_value=True):
            path = Path(directory) / "never-read-worker"
            path.write_text("#!" + sys.executable + "\nimport json,os,sys,time\nassert sys.argv[1] == '--working-directory'\nos.chdir(sys.argv[2])\nprint(json.dumps({'ready':True}),flush=True)\ntime.sleep(10)\n")
            path.chmod(path.stat().st_mode | stat.S_IXUSR)
            engine = protocol.RustEngine(path, directory, protocol.time.monotonic() + 0.5)
            process = engine.process
            try:
                with self.assertRaises(TimeoutError):
                    engine.request({"operation": "statement", "sql": "x" * 1_000_000})
            finally:
                engine.close()
            self.assertIsNotNone(process.poll())
