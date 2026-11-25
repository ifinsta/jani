import unittest
from stats import average

class T(unittest.TestCase):
    def test_empty(self):
        with self.assertRaises(ValueError):
            average([])
