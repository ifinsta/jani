import pathlib
import unittest
import calc

class H(unittest.TestCase):
    def test_old_name_is_gone(self):
        self.assertFalse(hasattr(calc, "add_numbers"))
    def test_no_file_mentions_the_old_name(self):
        for path in pathlib.Path(".").glob("*.py"):
            if path.name in ("hidden_test.py", "test_calc.py"):
                continue
            self.assertNotIn("add_numbers", path.read_text(), path.name)
    def test_cli_still_works(self):
        import io, contextlib, cli
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            cli.main(["cli", "4", "5"])
        self.assertEqual(out.getvalue().strip(), "9")
