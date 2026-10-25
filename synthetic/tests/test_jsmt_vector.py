"""Independently reproduce the published JSMT/1 Python-compatible SHA vector."""
import hashlib
import json
from pathlib import Path
import unittest

VECTOR = Path(__file__).resolve().parents[2] / "protocol/jsmt/v1/vectors/basic.json"
DIGEST = VECTOR.with_suffix(".sha256")


class JsmtCanonicalVectorTest(unittest.TestCase):
    def test_canonical_bytes_match_the_published_digest(self):
        content = json.loads(VECTOR.read_text(encoding="utf-8"))
        self.assertEqual(content["version"], "jani-jsmt/1")
        # JSMT/1's current interop vector deliberately pins a float, not
        # an integer. Python's canonical JSON distinguishes 1 from 1.0.
        self.assertIsInstance(content["metrics"][0]["value"], float)
        canonical = json.dumps(
            content, separators=(",", ":"), sort_keys=True,
            ensure_ascii=False, allow_nan=False
        ).encode("utf-8")
        expected = DIGEST.read_text(encoding="ascii").strip()
        self.assertEqual(hashlib.sha256(canonical).hexdigest(), expected)


if __name__ == "__main__":
    unittest.main()
