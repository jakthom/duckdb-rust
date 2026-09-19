"""Fresh-process differential tests for the guarded startup JSON adapter.

Run only after a reserved Python validation slot:
    PYTHONPATH=scripts python3 scripts/test_startup_json.py -v

Each of the 45 generated cases starts a child interpreter.  The child imports
startup_json before json, invokes it, then imports stdlib json and runs
the identical scenario as the oracle.  The child reports hook order, normalized
values, and complete JSONDecodeError coordinates as well as success/error type.
"""
from __future__ import annotations

import ast
import os
from pathlib import Path
import subprocess
import sys
import unittest

HERE = Path(__file__).resolve().parent
CANDIDATE_CASE_COUNT = 45

# Inputs include the actual strict one-job envelope, worker ready record, and
# canonical provenance sidecar record consumed by the proxy protocol.
CASES = {
    "loads_nested": ("loads", '{"a":[1,true,null,{"b":"x"}]}', {}),
    "loads_scalar": ("loads", ' -12.5e+2 ', {}),
    "loads_empty": ("loads", '', {}),
    "loads_whitespace": ("loads", ' \t\n ', {}),
    "loads_trailing": ("loads", 'true false', {}),
    "loads_unicode": ("loads", '"snowman \\u2603 \\ud800"', {}),
    "loads_control": ("loads", '"bad\x01control"', {}),
    "loads_bytes": ("loads", b'{"bytes":1}', {}),
    "loads_bom": ("loads", '\ufeff{"bom":1}', {}),
    "loads_duplicate_pairs": ("loads", '{"a":1,"a":2}', {"object_pairs_hook": "pairs"}),
    "loads_constant_hook": ("loads", '[NaN,Infinity,-Infinity]', {"parse_constant": "constant"}),
    "loads_hooks": ("loads", '{"i":1,"f":1.5}', {"parse_int": "int", "parse_float": "float", "object_hook": "object"}),
    "loads_malformed_later_hooks": ("loads", '{"ok":{"x":1},"bad":[}', {"object_hook": "object"}),
    "loads_integer_limit": ("loads", '1' * 5000, {}),
    "loads_nan": ("loads", 'NaN', {}),
    "loads_nan_identity": ("loads", '[NaN,NaN]', {}),
    "loads_depth": ("loads", '[' * 1100 + '0' + ']' * 1100, {}),
    "loads_strict_fallback": ("loads", '"bad\x01control"', {"strict": False}),
    "loads_already_imported": ("loads", '{"already":true}', {"already_imported": True}),
    "loads_unknown_kwarg": ("loads", '1', {"bogus": True}),
    "load_read_once": ("load", '{"load":true}', {}, {"ok": True}),
    "loads_pairs_reject": ("loads", '{"a":1,"a":2}', {"object_pairs_hook": "pairs_reject"}, {"error": "pairs rejected"}),
    "loads_constant_reject": ("loads", 'NaN', {"parse_constant": "constant_reject"}, {"error": "constant rejected"}),
    "loads_job_envelope": ("loads", '{"schema":"sqllogic-proxy-once-v1","worker":"/tmp/worker","worker_provenance":"/tmp/worker.provenance.json","test_root":"/tmp/tests","path":"test/performance/a2_1_scalar_comma_loop.test","timeout":60,"attestation":{"worker_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","source_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","provenance_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}}', {}, {"ok": True}),
    "loads_standalone_job": ("loads", '{"schema":"sqllogic-proxy-once-v1","worker":"/tmp/worker","worker_provenance":"/tmp/worker.provenance.json","test_root":"/tmp/tests","path":"test/performance/a2_1_scalar_comma_loop.test","timeout":60,"attestation":null}', {}, {"ok": True}),
    "loads_sidecar": ("loads", '{"profile":"release","source_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","binary_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}', {}),
    "loads_worker_ready": ("loads", '{"ready":true}', {}),
    "dumps_nested": ("dumps", "nested", {}),
    "dumps_unicode": ("dumps", "unicode", {}),
    "dumps_separators": ("dumps", "nested", {"separators": (",", ":")}),
    "dumps_sorted": ("dumps", "unordered", {"sort_keys": True}),
    "dumps_nan": ("dumps", "nan", {}),
    "dumps_nan_forbidden": ("dumps", "nan", {"allow_nan": False}),
    "dumps_default": ("dumps", "unknown", {"default": "default"}),
    "dumps_default_reject": ("dumps", "unknown", {"default": "default_reject"}, {"error": "default rejected"}),
    "dumps_falsey_default": ("dumps", "unknown", {"default": "falsey"}, {"error": "falsey default called"}),
    "dumps_custom_class_name": ("dumps", "spoofed_class", {}, {"error": "SpoofedClass"}),
    "dumps_cycle": ("dumps", "cycle", {}),
    "dumps_nonserializable": ("dumps", "unknown", {}),
    "dumps_skipkeys": ("dumps", "skipkeys", {"skipkeys": True}),
    "dumps_check_circular": ("dumps", "nested", {"check_circular": False}),
    "dumps_cls_fallback": ("dumps", "nested", {"cls": "bad"}),
    "dumps_unknown_kwarg": ("dumps", "nested", {"bogus": True}),
    "dumps_indent_fallback": ("dumps", "nested", {"indent": 2}),
    "dumps_ensure_ascii_fallback": ("dumps", "unicode", {"ensure_ascii": False}),
}



class CandidateDifferentialTests(unittest.TestCase):
    maxDiff = None

    def test_stdlib_fallback_promotes_unreplaced_bindings(self):
        """An unsupported public input permanently selects the stdlib bindings."""
        program = """
import sys
sys.path.insert(0, 'scripts')
import startup_json
original = startup_json.loads
assert startup_json.loads(b'{\"fallback\": true}') == {'fallback': True}
import json
assert startup_json.loads is json.loads and startup_json.loads is not original
assert startup_json.dumps is json.dumps
assert startup_json.load is json.load
assert startup_json.dump is json.dump
"""
        completed = subprocess.run(
            [sys.executable, "-c", program], cwd=HERE.parent, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_stdlib_fallback_preserves_caller_replacement(self):
        """Fallback must not overwrite a caller-installed public function."""
        program = """
import sys
sys.path.insert(0, 'scripts')
import startup_json
marker = object()
def replacement(*args, **kwargs):
    return marker
startup_json.loads = replacement
startup_json.dumps({'fallback': True}, ensure_ascii=False)
import json
assert startup_json.loads is replacement
assert startup_json.dumps is json.dumps
assert startup_json.loads('anything') is marker
"""
        completed = subprocess.run(
            [sys.executable, "-c", program], cwd=HERE.parent, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)


def _make_case(name: str):
    def case(self):
        self.assertEqual(len(CASES), CANDIDATE_CASE_COUNT)
        completed = subprocess.run(
            [sys.executable, os.fspath(HERE / "startup_json_test_child.py"), repr(CASES[name])],
            cwd=HERE.parent, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        candidate, oracle, kept_unloaded = ast.literal_eval(completed.stdout)
        operation, _payload, options, expectation = CASES[name]
        if "error" in expectation:
            self.assertEqual(candidate[0], "error")
            self.assertIn(expectation["error"], candidate[3])
        if name == "dumps_cycle":
            self.assertEqual(candidate[0], "error")
            self.assertEqual(candidate[2], "ValueError")
            self.assertIn("Circular reference", candidate[3])
        if name == "load_read_once":
            self.assertEqual(candidate[0], "ok")
            self.assertIn(("tuple", [("str", "read_count"), ("int", "1")]), candidate[2][1])
        self.assertEqual(candidate, oracle)
        unload_expected = name in {
            "loads_nested", "loads_scalar", "loads_unicode", "loads_job_envelope",
            "loads_sidecar", "loads_worker_ready", "dumps_nested", "dumps_unicode",
            "dumps_separators", "dumps_sorted", "dumps_nan", "dumps_skipkeys", "dumps_check_circular",
        }
        if unload_expected and candidate[0] == "ok":
            self.assertTrue(kept_unloaded, "supported candidate path imported json")
        fallback_expected = name in {
            "loads_bytes", "loads_bom", "loads_nan", "loads_nan_identity", "loads_constant_hook", "loads_strict_fallback", "loads_already_imported",
            "loads_unknown_kwarg", "dumps_cls_fallback", "dumps_unknown_kwarg", "dumps_indent_fallback",
            "dumps_ensure_ascii_fallback",
        }
        if fallback_expected:
            self.assertFalse(kept_unloaded, "fallback context did not use stdlib json")
    return case


for _name, _case in tuple(CASES.items()):
    if len(_case) == 3:
        CASES[_name] = (*_case, {})
    setattr(CandidateDifferentialTests, "test_" + _name, _make_case(_name))


if __name__ == "__main__":
    unittest.main()
