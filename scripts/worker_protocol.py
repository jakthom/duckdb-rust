"""Minimal JSON-line protocol shared by SQLLogic Python runners."""
import json
from pathlib import Path
import selectors
import subprocess
import tempfile
import time


def worker_provenance_path(binary):
    return Path(str(binary) + ".provenance.json")


class RustEngine:
    def __init__(self, binary, directory, deadline):
        self.errors = tempfile.TemporaryFile(mode="w+t")
        self.process = subprocess.Popen([str(binary)], cwd=directory, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.errors, text=True, bufsize=1)
        self.events = selectors.DefaultSelector(); self.events.register(self.process.stdout, selectors.EVENT_READ)
        self.deadline = deadline; self.engine_unsupported_seen = False; self.worker_requests = 0; self.sql_requests = 0
    def request(self, request):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0: raise TimeoutError("file deadline exceeded")
        nested = [item for stream in request.get("streams", []) for item in stream]
        self.worker_requests += 1 + len(nested)
        self.sql_requests += request.get("operation") in ("query", "statement")
        self.sql_requests += sum(item.get("operation") in ("query", "statement") for item in nested)
        self.process.stdin.write(json.dumps(request) + "\n"); self.process.stdin.flush()
        if not self.events.select(timeout=remaining): raise TimeoutError("file deadline exceeded")
        line = self.process.stdout.readline()
        if not line:
            self.errors.seek(0); raise RuntimeError("worker exited: " + self.errors.read()[:2000])
        response = json.loads(line)
        self.engine_unsupported_seen = bool(response.get("unsupported"))
        return response
    def close(self):
        if self.process.poll() is None: self.process.kill()
        self.process.wait(); self.process.stdin.close(); self.process.stdout.close(); self.events.close(); self.errors.close()
