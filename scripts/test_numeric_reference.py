import unittest

from numeric_reference import equivalent


class NumericOracleTests(unittest.TestCase):
    def result(self, value, kind='DECIMAL(38,2)'):
        return {'ok': True, 'columns': [kind], 'rows': [[value]]}

    def test_exact_types_digits_nulls_and_cardinality(self):
        a = self.result('999999999999999999999999999999999999.98')
        self.assertTrue(equivalent(a, a))
        for b in [self.result('999999999999999999999999999999999999.99'),
                  self.result(a['rows'][0][0], 'DOUBLE'), self.result('NULL'),
                  {'ok': True, 'columns': a['columns'], 'rows': []}]:
            self.assertFalse(equivalent(a, b))
        malformed = {'ok': True, 'columns': ['INTEGER'], 'rows': [[]]}
        self.assertFalse(equivalent(malformed, malformed))

    def test_floating_formatting_only_and_explicit_error_categories(self):
        self.assertTrue(equivalent(self.result('0', 'DOUBLE'), self.result('0.0', 'DOUBLE')))
        self.assertFalse(equivalent(self.result('0', 'DOUBLE'), self.result('0.000001', 'DOUBLE')))
        self.assertFalse(equivalent(self.result('bad', 'DOUBLE'), self.result('0', 'DOUBLE')))
        error = {'ok': False, 'message': 'Conversion Error: invalid input'}
        self.assertFalse(equivalent(error, error))
        self.assertTrue(equivalent(error, error, 'Conversion Error'))
        self.assertFalse(equivalent(error, error, 'Out of Range Error'))
        self.assertFalse(equivalent(error, {**error, 'unsupported': True}, 'Conversion Error'))


if __name__ == '__main__':
    unittest.main()
