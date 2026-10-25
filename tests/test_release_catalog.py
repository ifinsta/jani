"""Negative and positive tests for releases/{type}/{platform}/{version}."""
import importlib.util
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('jani_release_catalog', Path(__file__).resolve().parents[1] / 'tools/release_catalog.py')
catalog = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(catalog)


def fixture(root):
    folder = root / 'releases/agent/linux/v1.2.3'
    folder.mkdir(parents=True)
    name = 'jani-1.2.3-x86_64-unknown-linux-gnu'
    body = b'test artifact only; not a runnable client'
    (folder / name).write_bytes(body)
    h = hashlib.sha256(body).hexdigest()
    (folder / (name + '.sha256')).write_text(f'{h}  {name}\n')
    m = {'schemaVersion':'jani.release/v2','type':'agent','platform':'linux','package':'jani','version':'1.2.3','tag':'v1.2.3','commit':'a'*40,'dirty':False,'builtAt':'2026-10-25T00:00:00Z', 'artifacts':[{'target':'x86_64-unknown-linux-gnu','binary':name,'sha256':h,'sizeBytes':len(body),'commit':'a'*40,'dirty':False,'builtAt':'2026-10-25T00:00:00Z'}]}
    (folder / 'manifest.json').write_text(json.dumps(m))
    (folder.parent / 'latest.json').write_text(json.dumps({'schemaVersion':'jani.release-channel/v2','type':'agent','platform':'linux','latest':'v1.2.3'}))
    return folder, name

class ReleaseCatalogTest(unittest.TestCase):
    def test_valid_release_and_unbuilt_types(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder, name = fixture(Path(tmp))
            errors, releases = catalog.check(Path(tmp))
            self.assertEqual(errors, [])
            self.assertEqual(len(releases), 1)
    def test_tampered_binary_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder, name = fixture(Path(tmp))
            (folder / name).write_bytes(b'tampered')
            self.assertTrue(catalog.check(Path(tmp))[0])
    def test_channel_and_cross_platform_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder, name = fixture(Path(tmp))
            path = folder / 'manifest.json'
            m = json.loads(path.read_text()); m['platform'] = 'windows'; path.write_text(json.dumps(m))
            self.assertTrue(catalog.check(Path(tmp))[0])
    def test_path_traversal_and_missing_sidecar_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder, name = fixture(Path(tmp))
            self.assertFalse(catalog.safe_name('../bad'))
            (folder / (name+'.sha256')).unlink()
            self.assertTrue(catalog.check(Path(tmp))[0])

if __name__ == '__main__':
    unittest.main()
