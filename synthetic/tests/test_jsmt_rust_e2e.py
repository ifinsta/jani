"""Real JSMT/1 Rust probe/gateway interoperability gate.

Opt in with JANI_RUST_E2E=1 after building both Rust binaries.
Uses ephemeral development PKI on loopback only. Requires openssl.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import ssl
import subprocess
import tempfile
import time
import unittest
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "debug"
ENABLED = os.environ.get("JANI_RUST_E2E") == "1" and os.name != "nt"

def openssl(*args):
    subprocess.run(["openssl", *map(str, args)], check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=30)

def make_pki(tmp):
    p = Path(tmp)
    openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256",
            "-keyout", p/"ca.key", "-out", p/"ca.crt", "-days", "1",
            "-subj", "/CN=Jani Probe Test CA", "-addext", "basicConstraints=critical,CA:TRUE",
            "-addext", "keyUsage=critical,keyCertSign,cRLSign")
    for name, cn, san, usage in [
        ("server", "localhost", "DNS:localhost,IP:127.0.0.1", "serverAuth"),
        ("device", "device-one", "DNS:device-one", "clientAuth"),
        ("other", "other-device", "DNS:other-device", "clientAuth"),
    ]:
        openssl("req", "-new", "-newkey", "rsa:2048", "-nodes",
                "-keyout", p/f"{name}.key", "-out", p/f"{name}.csr",
                "-subj", f"/CN={cn}", "-addext", f"subjectAltName={san}",
                "-addext", "basicConstraints=critical,CA:FALSE",
                "-addext", "keyUsage=critical,digitalSignature,keyEncipherment",
                "-addext", f"extendedKeyUsage={usage}")
        openssl("x509", "-req", "-in", p/f"{name}.csr",
                "-CA", p/"ca.crt", "-CAkey", p/"ca.key", "-CAcreateserial",
                "-out", p/f"{name}.crt", "-days", "1", "-sha256",
                "-copy_extensions", "copy")
    return hashlib.sha256(ssl.PEM_cert_to_DER_cert((p/"device.crt").read_text())).hexdigest()

@unittest.skipUnless(ENABLED, "set JANI_RUST_E2E=1 on Linux with built Rust binaries")
class RustGatewayProbeTest(unittest.TestCase):
    def test_tls_mtls_logs_coverage_durable_ack_and_cross_language(self):
        self.assertIsNotNone(shutil.which("openssl"))
        self.assertTrue((BIN / "jani-probe").is_file(), "build jani-probe before test")
        self.assertTrue((BIN / "jani-gateway").is_file(), "build jani-gateway before test")
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp)
            fingerprint = make_pki(p)
            (p / "registry.json").write_text(json.dumps({fingerprint: {
                "tenant_id": "tenant-a", "device_id": "device-one",
            }}))
            services = []
            for name in ("api", "database", "worker"):
                file = p / (name+".log")
                file.write_text(name+" started\n")
                services.append({"service": name, "path": str(file)})
            services.append({"service":"missing-service", "path":str(p/"missing.log")})
            (p/"sources.json").write_text(json.dumps({"services": services}))
            with socket.socket() as sock:
                sock.bind(("127.0.0.1",0))
                port = sock.getsockname()[1]
            gateway = subprocess.Popen([
                str(BIN/"jani-gateway"), "--bind", f"127.0.0.1:{port}",
                "--ca", str(p/"ca.crt"), "--cert", str(p/"server.crt"),
                "--key", str(p/"server.key"), "--registry", str(p/"registry.json"),
                "--db", str(p/"central.db"),
            ], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            try:
                for _ in range(100):
                    if gateway.poll() is not None:
                        self.fail(f"gateway exited: {gateway.stderr.read()}")
                    try:
                        with socket.create_connection(("127.0.0.1",port),timeout=.2):
                            break
                    except OSError:
                        time.sleep(.05)
                else:
                    self.fail("gateway did not start")
                args = [
                    str(BIN/"jani-probe"),"--gateway",f"https://localhost:{port}",
                    "--tenant","tenant-a","--device","device-one",
                    "--spool",str(p/"spool.db"),"--sources",str(p/"sources.json"),
                    "--ca",str(p/"ca.crt"),"--cert",str(p/"device.crt"),"--key",str(p/"device.key"),
                ]
                run = subprocess.run(args, capture_output=True, text=True, timeout=35)
                self.assertEqual(run.returncode,0,run.stderr)
                with sqlite3.connect(p/"central.db") as con:
                    count=con.execute("SELECT COUNT(*) FROM accepted").fetchone()[0]
                    row=con.execute("SELECT json FROM accepted WHERE sequence=1").fetchone()
                self.assertEqual(count,1)
                payload=json.loads(row[0])
                self.assertEqual({l["resource"]["service.name"] for l in payload["logs"]},
                                 {"api","database","worker"})
                self.assertTrue(all(l["body"].endswith("started") for l in payload["logs"]))
                self.assertEqual(
                    {x["service"] for x in payload["coverage"] if x["status"]=="covered"},
                    {"api","database","worker"})
                self.assertEqual(
                    {x["service"] for x in payload["coverage"] if x["status"]=="missing"},
                    {"missing-service"})
                # Same transaction contains queryable normalized projections and source gaps.
                with sqlite3.connect(p/"central.db") as con:
                    self.assertGreaterEqual(con.execute("SELECT COUNT(*) FROM metric_samples").fetchone()[0],1)
                    self.assertEqual(con.execute("SELECT COUNT(*) FROM service_logs").fetchone()[0],3)
                    self.assertEqual(con.execute("SELECT COUNT(*) FROM coverage_latest WHERE status='covered'").fetchone()[0],3)
                    self.assertGreaterEqual(con.execute("SELECT COUNT(*) FROM incidents WHERE status='open'").fetchone()[0],1)
                    self.assertGreaterEqual(con.execute("SELECT COUNT(*) FROM incident_outbox WHERE event='open'").fetchone()[0],1)
                inspector=subprocess.run([str(BIN/"jani-gateway"),"inspect","--db",str(p/"central.db"),
                                          "--tenant","tenant-a"],capture_output=True,text=True,timeout=10)
                self.assertEqual(inspector.returncode,0,inspector.stderr)
                self.assertEqual(json.loads(inspector.stdout)["log_events"],3)
                metric_names={m["name"] for m in payload["metrics"]}
                self.assertIn("system.uptime",metric_names)
                self.assertIn("system.cpu.time",metric_names)
                self.assertIn("jani.filesystem.used",metric_names)
                self.assertTrue(all(m["unit"] in ("s","By","1","{process}") for m in payload["metrics"]))
                with sqlite3.connect(p/"spool.db") as con:
                    self.assertEqual(con.execute("SELECT COUNT(*) FROM pending").fetchone()[0],0)
                # Simulate loss of the final ACK followed by retention eviction.
                # The gateway still authenticates the latest receipt digest.
                raw=row[0].encode("utf-8")
                replay_context=ssl.create_default_context(cafile=str(p/"ca.crt"))
                replay_context.minimum_version=replay_context.maximum_version=ssl.TLSVersion.TLSv1_3
                replay_context.load_cert_chain(str(p/"device.crt"),str(p/"device.key"))
                replay_request=urllib.request.Request(f"https://localhost:{port}/v1/ingest",
                    data=raw,headers={"Content-Type":"application/json"},method="POST")
                with sqlite3.connect(p/"central.db") as con:
                    con.execute("DELETE FROM accepted WHERE tenant=? AND device=? AND sequence=?",
                                ("tenant-a","device-one",1))
                with urllib.request.urlopen(replay_request,context=replay_context,timeout=10) as response:
                    recovered=json.loads(response.read())
                self.assertTrue(recovered["duplicate"])
                self.assertEqual(recovered["ack_sequence"],1)
                self.assertEqual(recovered["payload_sha256"],hashlib.sha256(raw).hexdigest())
                # Independent Python client -> Rust gateway: canonical JSON, TLS 1.3,
                # mTLS, durable ACK and replay protection without using Rust probe code.
                extra={"version":"jani-jsmt/1","tenant_id":"tenant-a","device_id":"device-one",
                       "sequence":2,"observed_at":"2026-10-25T12:00:00Z",
                       "metrics":[],"logs":[],"coverage":[]}
                data=json.dumps(extra,separators=(",",":"),sort_keys=True,ensure_ascii=False).encode()
                ctx=ssl.create_default_context(cafile=str(p/"ca.crt"))
                ctx.minimum_version=ctx.maximum_version=ssl.TLSVersion.TLSv1_3
                ctx.load_cert_chain(str(p/"device.crt"),str(p/"device.key"))
                request=urllib.request.Request(f"https://localhost:{port}/v1/ingest",
                         data=data,headers={"Content-Type":"application/json"},method="POST")
                with urllib.request.urlopen(request,context=ctx,timeout=10) as response:
                    ack=json.loads(response.read())
                self.assertEqual(ack["payload_sha256"],hashlib.sha256(data).hexdigest())
                self.assertFalse(ack["duplicate"])
                with urllib.request.urlopen(request,context=ctx,timeout=10) as response:
                    same=json.loads(response.read())
                self.assertTrue(same["duplicate"])
                extra["observed_at"]="changed"
                changed=json.dumps(extra,separators=(",",":"),sort_keys=True).encode()
                with self.assertRaises(urllib.error.HTTPError) as conflict:
                    urllib.request.urlopen(urllib.request.Request(f"https://localhost:{port}/v1/ingest",
                        data=changed,headers={"Content-Type":"application/json"},method="POST"),context=ctx,timeout=10)
                self.assertEqual(conflict.exception.code,409)
                # A valid registered certificate cannot impersonate another tenant.
                wrong=dict(extra,tenant_id="other-tenant",sequence=3)
                wrong_data=json.dumps(wrong,separators=(",",":"),sort_keys=True).encode()
                with self.assertRaises(urllib.error.HTTPError) as mismatch:
                    urllib.request.urlopen(urllib.request.Request(f"https://localhost:{port}/v1/ingest",
                        data=wrong_data,headers={"Content-Type":"application/json"},method="POST"),
                        context=ctx,timeout=10)
                self.assertEqual(mismatch.exception.code,403)
                # Non-enrolled but CA-trusted client certificate is refused.
                other=ssl.create_default_context(cafile=str(p/"ca.crt"))
                other.load_cert_chain(str(p/"other.crt"),str(p/"other.key"))
                with self.assertRaises(urllib.error.HTTPError) as denied:
                    urllib.request.urlopen(request,context=other,timeout=10)
                self.assertEqual(denied.exception.code,403)
                # Revoking a registered certificate requires no gateway restart.
                updated={fingerprint:{"tenant_id":"tenant-a","device_id":"device-one","revoked":True}}
                replacement=p/"registry.new.json"
                replacement.write_text(json.dumps(updated))
                os.replace(replacement,p/"registry.json")
                with self.assertRaises(urllib.error.HTTPError) as revoked_reply:
                    urllib.request.urlopen(request,context=ctx,timeout=10)
                self.assertEqual(revoked_reply.exception.code,403)
                # No certificate should fail during the TLS handshake.
                bare=ssl.create_default_context(cafile=str(p/"ca.crt"))
                with self.assertRaises((ssl.SSLError,urllib.error.URLError,OSError)):
                    urllib.request.urlopen(request,context=bare,timeout=10)
            finally:
                gateway.terminate()
                try:
                    gateway.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    gateway.kill()
                    gateway.communicate(timeout=3)

if __name__=="__main__":
    unittest.main()
