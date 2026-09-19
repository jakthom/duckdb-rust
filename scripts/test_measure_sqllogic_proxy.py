import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import measure_sqllogic_proxy as proxy


def sample(command, records=5):
    return {"command": command, "returncode": 0, "stdout": "PASS", "stderr": "",
            "wall_ns": 10, "cpu_ns": 10, "max_rss_bytes": 10,
            "block_input": 0, "block_output": 0, "records": records}


class ProxyEvidenceTests(unittest.TestCase):
    def report(self):
        commands = {name: [name, "--serial"] for name in ("release", "development", "proxy")}
        return {"schema": "sqllogic-proxy-v1", "samples": 21,
                "proxy_configuration": {"threads": 1, "worker": "release-attested", "mode": "python-proxy"},
                "inputs": {"x": "a" * 64}, "workloads": [{"id": "a", "commands": commands,
                "observations": {name: [sample(command) for _ in range(21)] for name, command in commands.items()}}]}

    def test_complete_evidence_and_mutations_fail_closed(self):
        report = self.report(); self.assertTrue(proxy.validate_report(report))
        for mutate in (
            lambda r: r["workloads"][0]["observations"].pop("proxy"),
            lambda r: r["workloads"][0]["observations"]["proxy"].pop(),
            lambda r: r["workloads"][0]["observations"]["proxy"][0].update(records=0),
            lambda r: r["workloads"][0]["observations"]["proxy"][0].update(command=["wrong"]),
            lambda r: r["workloads"][0]["observations"]["proxy"][1].update(records=6),
            lambda r: r.update(inputs={}),
        ):
            broken = copy.deepcopy(report); mutate(broken)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError): proxy.validate_report(broken)

