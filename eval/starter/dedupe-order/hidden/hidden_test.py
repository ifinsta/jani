import unittest
from dedupe import dedupe

class H(unittest.TestCase):
    def test_unhashable(self):
        self.assertEqual(dedupe([[1], [1], [2]]), [[1], [2]])
    def test_strings_and_empty(self):
        self.assertEqual(dedupe(["b", "a", "b"]), ["b", "a"])
        self.assertEqual(dedupe([]), [])
    def test_input_untouched(self):
        data = [2, 2, 1]
        dedupe(data)
        self.assertEqual(data, [2, 2, 1])
