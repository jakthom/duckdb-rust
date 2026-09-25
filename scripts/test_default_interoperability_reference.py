import unittest
from pathlib import Path

from default_interoperability_reference import run_cases


class IndependentCaseTests(unittest.TestCase):
    def test_failure_does_not_hide_later_success(self):
        visited = []

        def success(_rust, _cpp, directory):
            visited.append(("success", directory))
            return {"passed": True, "evidence": "retained"}

        def failure(_rust, _cpp, directory):
            visited.append(("failure", directory))
            raise RuntimeError("expected WAL failure")

        def later(_rust, _cpp, directory):
            visited.append(("later", directory))
            return {"passed": True, "evidence": "still ran"}

        results = run_cases(
            [("success", success), ("failure", failure), ("later", later)],
            object(),
            object(),
        )

        self.assertEqual([name for name, _ in visited], ["success", "failure", "later"])
        self.assertEqual(results["success"], {"passed": True, "evidence": "retained"})
        self.assertEqual(
            results["failure"],
            {
                "passed": False,
                "error": {
                    "type": "RuntimeError",
                    "message": "expected WAL failure",
                },
            },
        )
        self.assertEqual(results["later"], {"passed": True, "evidence": "still ran"})
        directories = [directory for _, directory in visited]
        self.assertEqual(len(set(directories)), 3)
        self.assertTrue(all(isinstance(directory, Path) for directory in directories))
        self.assertTrue(all(not directory.exists() for directory in directories))


if __name__ == "__main__":
    unittest.main()
