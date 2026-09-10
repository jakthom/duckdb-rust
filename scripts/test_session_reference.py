import io
from pathlib import Path
import tempfile
import time
import unittest

from session_reference import CppEngine, encode_request, verify_records
from sqllogic import Record


class SessionReferenceTests(unittest.TestCase):
    def test_framing_preserves_unicode_newlines_and_empty_fields(self):
        for connection, sql in [('', 'SELECT 1'), ('会話\n2', "SELECT 'a\0雪\\\"\n'")]:
            request = {'operation': 'query', 'connection': connection, 'sql': sql}
            encoded = io.BytesIO(encode_request(request))
            fields = [encoded.read(int(encoded.readline())).decode('utf-8') for _ in range(3)]
            self.assertEqual(fields, ['query', connection, sql])
            self.assertEqual(encoded.read(), b'')
        for request in [{'operation': 'restart'}, {'operation': 'load', 'path': 'db'},
                        {'operation': 'load', 'read_only': True}]:
            with self.assertRaises(ValueError):
                encode_request(request)

    def test_failures_remain_recorded_while_later_sql_runs(self):
        class Engine:
            def __init__(self):
                self.requests = []

            def request(self, request):
                self.requests.append(request)
                return {'ok': True, 'columns': ['BIGINT'], 'rows': [['2']]}

        engine = Engine()
        records = [Record(1, ('query', 'I', 'a'), 'SELECT 1', ('1',)),
                   Record(5, ('query', 'I', 'b'), 'SELECT 2', ('2',))]
        outcomes = verify_records(engine, records)
        self.assertEqual([r['passed'] for r in outcomes], [False, True])
        self.assertEqual([r['connection'] for r in engine.requests], ['a', 'b'])
        self.assertEqual([r['sql'] for r in outcomes], ['SELECT 1', 'SELECT 2'])

    def test_partial_output_and_blocked_input_obey_deadlines(self):
        for partial, sql in [(True, 'SELECT 1'), (False, 'x' * 1000000)]:
            with self.subTest(partial=partial), tempfile.TemporaryDirectory() as directory:
                worker = Path(directory)/'worker'
                worker.write_text('#!/usr/bin/env python3\nimport sys,time\n'
                                  'sys.stdout.write(\'{"ready":true}\\n\' + ' + repr('{' if partial else '') + ')\n'
                                  'sys.stdout.flush()\ntime.sleep(0.4)\n')
                worker.chmod(0o700)
                engine = CppEngine(worker, directory, time.monotonic()+2)
                try:
                    engine.deadline = time.monotonic()+0.05
                    with self.assertRaises(TimeoutError):
                        engine.request({'operation': 'query', 'sql': sql})
                finally:
                    engine.close()


if __name__ == '__main__':
    unittest.main()
