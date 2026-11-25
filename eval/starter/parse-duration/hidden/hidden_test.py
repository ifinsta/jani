import unittest
from duration import parse_duration

class H(unittest.TestCase):
    def test_valid(self):
        self.assertEqual(parse_duration("2h"), 7200)
        self.assertEqual(parse_duration("45s"), 45)
        self.assertEqual(parse_duration("1h2m3s"), 3723)
    def test_invalid(self):
        for bad in ("", "5", "m5", "1x", "1m1h", "h", "1h 30m", "-5s"):
            with self.assertRaises(ValueError, msg=bad):
                parse_duration(bad)
