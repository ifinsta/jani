import unittest
from tags import add_tag

class T(unittest.TestCase):
    def test_independent_calls(self):
        self.assertEqual(add_tag("a"), ["a"])
        self.assertEqual(add_tag("b"), ["b"])
