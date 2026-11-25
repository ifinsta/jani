import unittest
from stack import Stack

class T(unittest.TestCase):
    def test_peek(self):
        s = Stack()
        s.push(1)
        s.push(2)
        self.assertEqual(s.peek(), 2)
        self.assertEqual(s.pop(), 2)
