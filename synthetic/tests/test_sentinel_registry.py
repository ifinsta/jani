"""Offline registry durability, validation and revocation tests."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

TOOL = Path(__file__).resolve().parents[2] / "tools/sentinel_registry.py"
SPEC = importlib.util.spec_from_file_location("sentinel_registry", TOOL)
registry = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(registry)

class RegistryTests(unittest.TestCase):
    def test_create_and_revoke(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "devices.json"
            fp = "a" * 64
            record = {fp: {"tenant_id": "test-tenant", "device_id": "host_01",
                           "revoked": False, "expires_unix": 1888888888}}
            registry.registry_write(path, record)
            self.assertEqual(registry.registry_read(path), record)
            if hasattr(__import__("os"), "chmod") and __import__("os").name != "nt":
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            record[fp]["revoked"] = True
            registry.registry_write(path, record)
            self.assertTrue(registry.registry_read(path)[fp]["revoked"])

    def test_reject_symlink_and_invalid_entries(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "devices.json"
            path.write_text(json.dumps({"z" * 64: {"tenant_id": "t", "device_id": "d"}}))
            with self.assertRaises(ValueError):
                registry.registry_read(path)
            link = Path(tmp) / "link.json"
            try:
                link.symlink_to(path)
            except OSError:
                return  # Windows without developer mode
            with self.assertRaises(ValueError):
                registry.registry_read(link)


    @unittest.skipUnless(__import__("shutil").which("openssl"), "requires OpenSSL")
    def test_real_certificate_enroll_and_revoke(self):
        import contextlib
        import io
        import subprocess
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp)
            def openssl(*args):
                subprocess.run(["openssl", *map(str,args)], check=True,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
            openssl("req","-x509","-newkey","rsa:2048","-nodes","-sha256",
                    "-keyout",p/"ca.key","-out",p/"ca.crt","-days","1",
                    "-subj","/CN=JaniTestCA","-addext","basicConstraints=critical,CA:TRUE",
                    "-addext","keyUsage=critical,keyCertSign,cRLSign")
            openssl("req","-new","-newkey","rsa:2048","-nodes",
                    "-keyout",p/"device.key","-out",p/"device.csr",
                    "-subj","/CN=device","-addext","extendedKeyUsage=clientAuth")
            openssl("x509","-req","-in",p/"device.csr","-CA",p/"ca.crt",
                    "-CAkey",p/"ca.key","-CAcreateserial",
                    "-out",p/"device.crt","-days","1","-sha256",
                    "-copy_extensions","copy")
            fingerprint,_ = registry.credential(str(p/"ca.crt"),str(p/"device.crt"))
            f=p/"devices.json"
            with contextlib.redirect_stdout(io.StringIO()):
                registry.main(["enroll","--registry",str(f),"--ca",str(p/"ca.crt"),
                               "--cert",str(p/"device.crt"),"--tenant","tenant-a",
                               "--device","device-one"])
                registry.main(["revoke","--registry",str(f),"--fingerprint",fingerprint])
            self.assertTrue(registry.registry_read(f)[fingerprint]["revoked"])

    def test_reject_oversize_and_unknown_fields(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "devices.json"
            path.write_bytes(b"a" * (registry.MAX_SIZE + 1))
            with self.assertRaises(ValueError):
                registry.registry_read(path)
            path.write_text(json.dumps({"a" * 64: {"tenant_id": "t", "device_id": "d", "superuser": True}}))
            with self.assertRaises(ValueError):
                registry.registry_read(path)
