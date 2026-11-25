import unittest
from stats import average

class H(unittest.TestCase):
    def test_message(self):
        with self.assertRaises(ValueError) as caught:
            average([])
        self.assertEqual(str(caught.exception), "average of empty list")
    def test_normal(self):
        self.assertEqual(average([1, 2, 3]), 2)
        self.assertAlmostEqual(average([0.5, 1.5]), 1.0)
