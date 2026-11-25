"""Negative and positive tests for releases/{type}/{platform}/{version}."""
import importlib.util
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

TOOLS = Path(__file__).resolve().parents[1] / 'tools'
sys.path.insert(0, str(TOOLS))
SPEC = importlib.util.spec_from_file_location('jani_release_catalog', TOOLS / 'release_catalog.py')
catalog = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(catalog)
import release_signing as signing
INSTALLER_SPEC = importlib.util.spec_from_file_location('jani_install_unix', TOOLS / 'install_unix.py')
installer = importlib.util.module_from_spec(INSTALLER_SPEC)
INSTALLER_SPEC.loader.exec_module(installer)


def signed_fixture(root):
    folder, name = fixture(root)
    private_key = root / 'release-private.pem'
    subprocess.run(
        ['openssl', 'genpkey', '-algorithm', 'ED25519', '-out', str(private_key)],
        check=True,
        capture_output=True,
    )
    path = folder / 'manifest.json'
    manifest = json.loads(path.read_text())
    manifest = signing.sign_manifest(manifest, private_key, 'release-test')
    path.write_text(json.dumps(manifest))
    public = signing.public_key_hex(private_key)
    return folder, name, public


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

    def test_unsigned_manifests_can_be_required_to_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture(Path(tmp))
            self.assertTrue(catalog.check(Path(tmp), require_signatures=True)[0])

    @unittest.skipUnless(shutil.which('openssl'), 'OpenSSL is required for Ed25519 release tests')
    def test_signed_manifest_verifies_only_with_the_explicit_trusted_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            folder, _, public = signed_fixture(root)
            self.assertEqual(catalog.check(root, [f'release-test:{public}'])[0], [])
            source, name, digest = installer.verify(root, 'agent', 'x86_64-unknown-linux-gnu', 'v1.2.3', [f'release-test:{public}'])
            self.assertTrue(source.is_file())
            self.assertEqual(name, 'jani')
            self.assertEqual(len(digest), 64)
            with self.assertRaises(ValueError):
                installer.verify(root, 'agent', 'x86_64-unknown-linux-gnu', 'v1.2.3', [])
            self.assertTrue(catalog.check(root)[0])
            self.assertTrue(catalog.check(root, ['release-test:' + '22' * 32])[0])
            path = folder / 'manifest.json'
            manifest = json.loads(path.read_text())
            manifest['artifacts'][0]['sha256'] = '33' * 32
            path.write_text(json.dumps(manifest))
            self.assertTrue(catalog.check(root, [f'release-test:{public}'])[0])

if __name__ == '__main__':
    unittest.main()
