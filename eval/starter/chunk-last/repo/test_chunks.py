import unittest
from chunks import chunk

class T(unittest.TestCase):
    def test_even(self):
        self.assertEqual(chunk([1, 2, 3, 4], 2), [[1, 2], [3, 4]])
