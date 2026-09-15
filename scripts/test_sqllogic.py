import unittest

from sqllogic import Runner, parse


class ConcurrentEngine:
    def __init__(self):
        self.requests = []

    def request(self, request):
        self.requests.append(request)
        if request["operation"] != "concurrent":
            return {"ok": True}
        streams = []
        for stream in request["streams"]:
            responses = []
            for item in stream:
                if item["operation"] == "query":
                    value = item["sql"].removeprefix("SELECT ")
                    responses.append({"ok": True, "columns": ["INTEGER"], "rows": [[value]]})
                else:
                    responses.append({"ok": True})
            streams.append(responses)
        return {"ok": True, "streams": streams}


class SQLLogicSchedulingTests(unittest.TestCase):
    def test_loop_conditions_continue_and_boundaries(self):
        engine = ConcurrentEngine()
        runner = Runner(engine)
        runner.run(parse("""
loop i -2 3
onlyif i>=-1&&i<=1
statement ok
SELECT {i}

onlyif i=0
continue

statement ok
SELECT 99

endloop
loop empty 3 1
statement ok
SELECT 100

endloop
"""))
        self.assertEqual([request["sql"] for request in engine.requests],
                         ["SELECT 99", "SELECT -1", "SELECT 99", "SELECT 0",
                          "SELECT 1", "SELECT 99", "SELECT 99", "SELECT 100"])
        self.assertEqual((runner.passed, runner.skipped), (8, 2))

    def test_concurrent_loop_compiles_isolated_streams_with_loop_conditions(self):
        engine = ConcurrentEngine()
        runner = Runner(engine)
        runner.run(parse("""
concurrentloop threadid 0 4
onlyif threadid<>2
query I
SELECT {threadid}
----
{threadid}

endloop
"""))
        self.assertEqual((runner.passed, runner.skipped), (3, 1))
        batch = engine.requests[0]
        self.assertEqual(batch["operation"], "concurrent")
        self.assertEqual([[item["sql"] for item in stream] for stream in batch["streams"]],
                         [["SELECT 0"], ["SELECT 1"], [], ["SELECT 3"]])

    def test_concurrent_rejects_nested_parallel_named_sessions_and_continue(self):
        for source, message in [
            ("concurrentloop i 0 2\nstatement ok named\nSELECT 1\n\nendloop\n", "Named connections"),
            ("concurrentloop i 0 2\ncontinue\n\nendloop\n", "not supported"),
            ("concurrentloop i 0 2\nconcurrentloop j 0 2\nstatement ok\nSELECT 1\n\nendloop\nendloop\n", "Nested parallel"),
        ]:
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                Runner(ConcurrentEngine()).run(parse(source))

    def test_missing_and_non_numeric_loop_conditions_fail(self):
        for source in [
            "loop i 0 1\nonlyif missing=0\nstatement ok\nSELECT 1\n\nendloop\n",
            "foreach i text\nonlyif i>0\nstatement ok\nSELECT 1\n\nendloop\n",
        ]:
            with self.assertRaises(ValueError):
                Runner(ConcurrentEngine()).run(parse(source))

    def test_exact_comparators_and_integer_domains(self):
        for source in [
            "loop i 0 1\nonlyif i==0\nstatement ok\nSELECT 1\n\nendloop\n",
            "loop i 2147483648 2147483649\nstatement ok\nSELECT 1\n\nendloop\n",
            "foreach i 9223372036854775808\nonlyif i>0\nstatement ok\nSELECT 1\n\nendloop\n",
        ]:
            with self.subTest(source=source), self.assertRaises(ValueError):
                Runner(ConcurrentEngine()).run(parse(source))
        with self.assertRaisesRegex(Exception, "condition"):
            Runner(ConcurrentEngine()).run(parse(
                "loop i 0 1\nonlyif i!=0\nstatement ok\nSELECT 1\n\nendloop\n"))

    def test_foreach_collections_removal_tuple_and_variable(self):
        engine = ConcurrentEngine()
        engine.request = lambda request: (
            {"ok": True, "values": ["x,y", "z,w"]}
            if request["operation"] == "foreach" else {"ok": True})
        runner = Runner(engine)
        values = runner.foreach_values(["<signed>", "!integer", "<compression>"])
        self.assertEqual(values[:4], ["tinyint", "smallint", "bigint", "hugeint"])
        self.assertEqual(values[-9:],
                         "none uncompressed rle bitpacking dictionary fsst dict_fsst alp alprd".split())
        self.assertEqual(len(runner.foreach_values(["<all_types_columns>"])), 53)
        runner.run(parse(
            "foreach left,right <variable:pairs>\n"
            "statement ok\nSELECT '{left}:{right}'\n\nendloop\n"))


if __name__ == "__main__":
    unittest.main()
