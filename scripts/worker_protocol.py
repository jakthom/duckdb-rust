"""Minimal JSON-line protocol shared by SQLLogic Python runners."""
import startup_json as json
import os
import time


def worker_provenance_path(binary):
    from pathlib import Path
    return Path(str(binary) + ".provenance.json")


def _scratch_path(directory):
    if hasattr(directory, "validate_path"):
        directory.validate_path()
    path = os.path.abspath(os.fspath(directory))
    if not os.path.isdir(path):
        raise ValueError("worker scratch directory is unavailable")
    return path


def _anonymous_errors(directory):
    if hasattr(directory, "anonymous_text_file"):
        return directory.anonymous_text_file()
    from secure_scratch import anonymous_text_file
    return anonymous_text_file()


def _close_quietly(value):
    if value is not None:
        try:
            value.close()
        except BaseException:
            pass


def _normalize_fd(descriptor):
    """Keep spawn sources away from 0/1/2 when an embedding closed stdio."""
    if descriptor >= 3:
        return descriptor
    import fcntl
    duplicate = fcntl.fcntl(descriptor, fcntl.F_DUPFD_CLOEXEC, 3)
    try:
        os.close(descriptor)
    except BaseException:
        _close_fd(duplicate)
        raise
    return duplicate


def _close_fd(descriptor):
    if descriptor is not None:
        try:
            os.close(descriptor)
        except OSError:
            pass


def _strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON startup key")
        result[key] = value
    return result


class _Spawned:
    def __init__(self, pid, stdin, stdout):
        self.pid, self.stdin, self.stdout = pid, stdin, stdout
        self.returncode = None

    def poll(self):
        if self.returncode is None:
            try:
                pid, status = os.waitpid(self.pid, os.WNOHANG)
            except ChildProcessError:
                self.returncode = _UNKNOWN_EXIT
            else:
                if pid == self.pid:
                    self.returncode = os.waitstatus_to_exitcode(status)
        return self.returncode

    def kill(self):
        if self.poll() is None:
            try:
                os.kill(self.pid, 9)
            except ProcessLookupError:
                pass

    def wait(self):
        if self.returncode is None:
            try:
                _pid, status = os.waitpid(self.pid, 0)
                self.returncode = os.waitstatus_to_exitcode(status)
            except ChildProcessError:
                self.returncode = _UNKNOWN_EXIT
        return self.returncode


def _can_spawn():
    return (os.name == "posix" and hasattr(os, "posix_spawn")
            and hasattr(os, "POSIX_SPAWN_CLOSE") and hasattr(os, "POSIX_SPAWN_DUP2"))


_UNKNOWN_EXIT = object()


def _kill_reap_pid(pid):
    try:
        os.kill(pid, 9)
    except (ProcessLookupError, ChildProcessError):
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass


def _spawn(binary, directory, errors):
    """Public POSIX spawn path; every parent descriptor is owned on failure."""
    import io
    input_read = input_write = output_read = output_write = error_source = None
    raw_stdin = raw_stdout = stdin = stdout = process = None
    pid = None
    try:
        input_read, input_write = os.pipe()
        output_read, output_write = os.pipe()
        input_read = _normalize_fd(input_read)
        input_write = _normalize_fd(input_write)
        output_read = _normalize_fd(output_read)
        output_write = _normalize_fd(output_write)
        error_source = os.dup(errors.fileno())
        error_source = _normalize_fd(error_source)
        try:
            parent_input_write, parent_output_read = input_write, output_read
            raw_stdin = os.fdopen(parent_input_write, "wb", closefd=True)
            input_write = None
            stdin = io.TextIOWrapper(raw_stdin, write_through=True)
            raw_stdin = None
            raw_stdout = os.fdopen(parent_output_read, "rb", buffering=0, closefd=True)
            output_read = None
            stdout, raw_stdout = raw_stdout, None
            actions = [(os.POSIX_SPAWN_CLOSE, parent_input_write), (os.POSIX_SPAWN_CLOSE, parent_output_read),
                       (os.POSIX_SPAWN_DUP2, input_read, 0), (os.POSIX_SPAWN_DUP2, output_write, 1),
                       (os.POSIX_SPAWN_DUP2, error_source, 2)]
            actions.extend((os.POSIX_SPAWN_CLOSE, fd) for fd in (input_read, output_write, error_source))
            pid = os.posix_spawn(str(binary), [str(binary), "--working-directory", directory], os.environ, file_actions=actions)
            process = _Spawned(pid, stdin, stdout)
            pid = None
            stdin = stdout = None
        finally:
            _close_fd(error_source)
            error_source = None
        _close_fd(input_read); input_read = None
        _close_fd(output_write); output_write = None
        return process
    except BaseException:
        if process is not None:
            try: process.kill(); process.wait()
            except BaseException: pass
            _close_quietly(process.stdin); _close_quietly(process.stdout)
        else:
            if pid is not None:
                _kill_reap_pid(pid)
            _close_quietly(stdin); _close_quietly(stdout)
            _close_quietly(raw_stdin); _close_quietly(raw_stdout)
        raise
    finally:
        _close_fd(input_read); _close_fd(input_write); _close_fd(error_source)
        _close_fd(output_read); _close_fd(output_write)


def _popen(binary, directory, errors):
    import subprocess
    return subprocess.Popen([str(binary), "--working-directory", directory], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=errors, text=True, bufsize=1)


class RustEngine:
    def __init__(self, binary, directory, deadline):
        self.deadline, self.process, self.errors = deadline, None, None
        self._selector, self._buffer, self._spawned, self._bounded_writes = None, bytearray(), False, False
        self.engine_unsupported_seen = False; self.worker_requests = self.sql_requests = 0
        try:
            self.errors = _anonymous_errors(directory)
            directory = _scratch_path(directory)
            self._spawned = _can_spawn()
            self.process = _spawn(binary, directory, self.errors) if self._spawned else _popen(binary, directory, self.errors)
            if not self._spawned:
                import selectors
                self._selector = selectors.DefaultSelector()
                self._selector.register(self.process.stdout, selectors.EVENT_READ)
            if os.name == "posix" and hasattr(os, "set_blocking"):
                os.set_blocking(self.process.stdin.fileno(), False)
                self._bounded_writes = True
            self._expect_ready()
        except BaseException as original:
            try:
                self.close()
            except BaseException as cleanup_error:
                raise original from cleanup_error
            raise

    def _remaining(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("file deadline exceeded")
        return remaining

    def _read_line(self):
        while True:
            newline = self._buffer.find(b"\n")
            if newline >= 0:
                raw = bytes(self._buffer[:newline]); del self._buffer[:newline + 1]
                return raw.decode("utf-8", "strict")
            remaining = self._remaining()
            if self._spawned:
                import select
                ready = select.select([self.process.stdout], [], [], remaining)[0]
            else:
                ready = self._selector.select(remaining)
            if not ready:
                raise TimeoutError("file deadline exceeded")
            chunk = os.read(self.process.stdout.fileno(), 4096)
            if not chunk:
                self.errors.seek(0)
                raise RuntimeError("worker exited: " + self.errors.read(2000))
            self._buffer.extend(chunk)

    def _expect_ready(self):
        try:
            ready = json.loads(self._read_line(), object_pairs_hook=_strict_object)
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
            raise RuntimeError("worker emitted malformed startup record") from error
        if not isinstance(ready, dict) or set(ready) != {"ready"} or ready["ready"] is not True:
            raise RuntimeError("worker rejected startup protocol")

    def request(self, request):
        self._remaining()
        nested = [item for stream in request.get("streams", []) for item in stream]
        self.worker_requests += 1 + len(nested)
        self.sql_requests += request.get("operation") in ("query", "statement")
        self.sql_requests += sum(item.get("operation") in ("query", "statement") for item in nested)
        payload = (json.dumps(request) + "\n").encode("utf-8")
        if self._bounded_writes:
            import select
            pending = memoryview(payload)
            while pending:
                self._remaining()
                try:
                    written = os.write(self.process.stdin.fileno(), pending[:65536])
                    if written:
                        pending = pending[written:]
                        continue
                except BlockingIOError:
                    pass
                if not select.select([], [self.process.stdin], [], self._remaining())[1]:
                    raise TimeoutError("file deadline exceeded")
        else:
            self.process.stdin.write(payload.decode("utf-8")); self.process.stdin.flush()
        response = json.loads(self._read_line())
        self.engine_unsupported_seen = bool(response.get("unsupported"))
        return response

    def close(self):
        failure = None
        def attempt(operation):
            nonlocal failure
            try:
                operation()
            except BaseException as error:
                if failure is None:
                    failure = error
        process = self.process
        self.process = None
        if process is not None:
            try:
                alive = process.poll() is None
            except BaseException as error:
                # Poll failure cannot prove the child is gone; still attempt a
                # kill so a startup/close failure cannot orphan it.
                alive = True
                failure = error
            if alive:
                attempt(process.kill)
            attempt(process.wait)
            attempt(process.stdin.close)
            attempt(process.stdout.close)
        if self._selector is not None:
            attempt(self._selector.close); self._selector = None
        if self.errors is not None:
            attempt(self.errors.close); self.errors = None
        if failure is not None:
            raise failure
