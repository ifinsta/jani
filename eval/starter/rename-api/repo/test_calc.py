import unittest
from calc import add
from report import total

class T(unittest.TestCase):
    def test_add_and_total(self):
        self.assertEqual(add(2, 3), 5)
        self.assertEqual(total([1, 2, 3]), 6)
