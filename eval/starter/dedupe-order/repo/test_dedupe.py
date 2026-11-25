import unittest
from dedupe import dedupe

class T(unittest.TestCase):
    def test_order(self):
        self.assertEqual(dedupe([3, 1, 3, 2, 1]), [3, 1, 2])
