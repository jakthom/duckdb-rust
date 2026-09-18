import copy
import unittest

from fastest_reference import summarize
from reference_version import TARGETS


class FasterReferenceTests(unittest.TestCase):
    def setUp(self):
        self.cases = [{'name': 'a', 'sql': 'SELECT 1', 'rows': 1, 'sum': '1'},
                      {'name': 'b', 'sql': 'SELECT 2', 'rows': 1, 'sum': '2'}]
        self.reports = []
        for target, times in [('release', [100, 200]), ('development', [200, 100])]:
            report = {key: 'same' for key in ('rust_source_sha256', 'rust_binary_sha256',
                       'workloads_sha256', 'cpp_worker_source_sha256', 'platform', 'machine',
                       'compiler', 'rustc')}
            report.update(reference_identity={'target': target}, cpp_revision=TARGETS[target].revision,
                          max_ratio=1.0, iterations=9, warmups=3, workloads=[])
            for case, elapsed in zip(self.cases, times):
                sample = {'rows': case['rows'], 'sum': case['sum']}
                report['workloads'].append({**case, 'workers': [{}, {'engine': 'rust'}],
                    'cpp': [{**sample, 'elapsed_ns': elapsed} for _ in range(9)],
                    'rust': [{**sample, 'elapsed_ns': 90} for _ in range(9)]})
            self.reports.append(report)

    def test_selects_faster_reference_per_workload(self):
        result = summarize(*self.reports, self.cases)
        self.assertTrue(result['passed'])
        self.assertEqual([case['selected_reference'] for case in result['workloads']], ['release', 'development'])

    def test_slower_reference_and_better_rust_sample_cannot_hide_regression(self):
        self.reports[1]['workloads'][0]['rust'][4]['elapsed_ns'] = 150
        self.reports[1]['workloads'][0]['rust'][5:] = [self.reports[1]['workloads'][0]['rust'][4]] * 4
        result = summarize(*self.reports, self.cases)
        self.assertFalse(result['passed'])
        self.assertEqual(result['workloads'][0]['rust_over_fastest']['development'], 1.5)

    def test_wrong_results_missing_cases_and_changed_identity_are_rejected(self):
        for mutate in [lambda r: r['workloads'].pop(),
                       lambda r: r.update(rust_binary_sha256='changed'),
                       lambda r: r.update(error='timeout'),
                       lambda r: r['workloads'][0]['cpp'][0].update(sum='wrong'),
                       lambda r: r['workloads'].append(r['workloads'][0]),
                       lambda r: r['workloads'][0].update(sql='SELECT 3')]:
            changed = copy.deepcopy(self.reports)
            mutate(changed[0])
            with self.assertRaises(ValueError):
                summarize(*changed, self.cases)


if __name__ == '__main__':
    unittest.main()
