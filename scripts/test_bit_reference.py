import unittest

from bit_reference import canonical_error_category, equivalent_bit


class BitOracleTests(unittest.TestCase):
    def test_error_normalization_changes_only_category_label_case(self):
        raw = {'ok': False, 'message': 'INTERNAL Error: Cannot cast EMPTY BLOB\nLINE 1: SQL'}
        normalized = canonical_error_category(raw)
        self.assertEqual(normalized['message'], 'internal error: Cannot cast EMPTY BLOB\nLINE 1: SQL')
        self.assertEqual(raw['message'], 'INTERNAL Error: Cannot cast EMPTY BLOB\nLINE 1: SQL')
        changed_body = {**raw, 'message': 'Internal Error: cannot cast empty BLOB\nLINE 1: SQL'}
        self.assertNotEqual(normalized, canonical_error_category(changed_body))
        self.assertTrue(equivalent_bit(raw, normalized, 'Internal Error'))
        self.assertFalse(equivalent_bit(raw, raw))
        self.assertFalse(equivalent_bit(raw, raw, 'Conversion Error'))
        self.assertFalse(equivalent_bit(raw, {**raw, 'unsupported': True}, 'Internal Error'))
        value = {'ok': True, 'columns': ['VARCHAR'], 'rows': [['INTERNAL Error: text']]}
        self.assertEqual(canonical_error_category(value), value)
        self.assertFalse(equivalent_bit(value, {**value, 'rows': [['internal error: text']]}))


if __name__ == '__main__':
    unittest.main()
