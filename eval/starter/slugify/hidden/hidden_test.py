import unittest
from slugify import slugify

class H(unittest.TestCase):
    def test_edges(self):
        self.assertEqual(slugify("  Hi!!  There "), "hi-there")
        self.assertEqual(slugify("a--b"), "a-b")
        self.assertEqual(slugify(""), "")
        self.assertEqual(slugify("C++ 101"), "c-101")
        self.assertEqual(slugify("---"), "")
        self.assertEqual(slugify("Caf\u00e9 au lait"), "caf-au-lait")
