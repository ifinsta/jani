import unittest
from stack import Stack

class H(unittest.TestCase):
    def test_empty_peek(self):
        with self.assertRaises(IndexError):
            Stack().peek()
    def test_len(self):
        s = Stack()
        self.assertEqual(len(s), 0)
        s.push("a")
        self.assertEqual(len(s), 1)
        s.peek()
        self.assertEqual(len(s), 1)
