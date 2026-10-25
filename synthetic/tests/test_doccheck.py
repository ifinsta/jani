"""Regression tests for the normative documentation inventory gate."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

TOOL = Path(__file__).resolve().parents[2] / "tools/doccheck.py"
SPEC = importlib.util.spec_from_file_location("jani_doccheck", TOOL)
doccheck = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(doccheck)


def fixture(root):
    (root / "docs").mkdir(exist_ok=True)
    (root / "README.md").write_text("# Source fixture\n", encoding="utf-8")
    for i, path in enumerate(doccheck.CORE, 1):
        f = root / path
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(
            "---\n"
            f"id: JANI-TEST-{i:03d}\n"
            "title: Test contract\n"
            "status: proposed\n"
            "owner: test\n"
            "version: 1.0.0\n"
            "last-reviewed: 2026-10-25\n"
            "source:\n  - README.md\n"
            "tests: []\n---\n# Test\n",
            encoding="utf-8",
        )
    (root / "docs/documentation_manifest.json").write_text(
        json.dumps({
            "schemaVersion": "jani.docs/v1",
            "requiredCore": list(doccheck.CORE),
            "files": list(doccheck.CORE),
        }),
        encoding="utf-8",
    )


class DocumentationGateTest(unittest.TestCase):
    def test_missing_critical_contract_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture(root)
            self.assertEqual(doccheck.check(root), [])
            (root / doccheck.CORE[0]).unlink()
            self.assertTrue(doccheck.check(root))

    def test_missing_source_and_duplicate_id_are_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture(root)
            first = root / doccheck.CORE[0]
            first.write_text(first.read_text().replace("README.md", "missing.rs"))
            self.assertTrue(any("missing source" in x for x in doccheck.check(root)))
            fixture(root)
            second = root / doccheck.CORE[1]
            second.write_text(second.read_text().replace("JANI-TEST-002", "JANI-TEST-001"))
            self.assertTrue(any("duplicate document ID" in x for x in doccheck.check(root)))

    def test_extra_uncatalogued_contract_and_traversal_are_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture(root)
            extra = root / "docs/rogue.md"
            extra.write_text("---\nid: JANI-TEST-999\n---\n# Extra\n")
            self.assertTrue(any("absent from manifest" in x for x in doccheck.check(root)))
            self.assertRaises(ValueError, doccheck.rooted, root, "../outside.md")


if __name__ == "__main__":
    unittest.main()
