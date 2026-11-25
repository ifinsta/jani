import unittest
from chunks import chunk

class H(unittest.TestCase):
    def test_ragged(self):
        self.assertEqual(chunk([1, 2, 3], 2), [[1, 2], [3]])
    def test_empty(self):
        self.assertEqual(chunk([], 3), [])
    def test_bigger_than_list(self):
        self.assertEqual(chunk([1], 5), [[1]])
    def test_bad_n(self):
        for n in (0, -1):
            with self.assertRaises(ValueError):
                chunk([1], n)
