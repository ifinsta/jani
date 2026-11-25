import unittest
from tags import add_tag

class H(unittest.TestCase):
    def test_given_list_is_extended_and_returned(self):
        mine = ["x"]
        result = add_tag("y", mine)
        self.assertEqual(result, ["x", "y"])
        self.assertIs(result, mine)
    def test_no_shared_default(self):
        add_tag("a")
        add_tag("b")
        self.assertEqual(add_tag("c"), ["c"])
