"""Open-source JSMT/1 TLS 1.3/mTLS ingestion reference; pinned devices."""
from __future__ import annotations
import hashlib
import json
import sqlite3
import ssl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from .schema import MAX_PAYLOAD_BYTES, canonical, digest, validate


def create_db(path: str) -> None:
    con=sqlite3.connect(path,timeout=10)
    try:
        con.execute('PRAGMA journal_mode=WAL')
        con.execute('PRAGMA synchronous=FULL')
        con.executescript('''
            CREATE TABLE IF NOT EXISTS accepted (
                tenant TEXT NOT NULL, device TEXT NOT NULL, sequence INTEGER NOT NULL,
                payload_hash TEXT NOT NULL, json TEXT NOT NULL,
                PRIMARY KEY (tenant,device,sequence)
            );
            CREATE TABLE IF NOT EXISTS device_cursors (
                tenant TEXT NOT NULL, device TEXT NOT NULL, last_seq INTEGER NOT NULL,
                PRIMARY KEY (tenant,device)
            );
        ''')
        con.commit()
    finally: con.close()


def commit_envelope(db: str, env:dict) -> tuple[int, dict]:
    """ACK is returned only after durable SQLite commit; duplicate same-hash is safe."""
    validate(env)
    h=digest(env)
    tenant,device,seq=env['tenant_id'],env['device_id'],env['sequence']
    con=sqlite3.connect(db,timeout=10,isolation_level=None)
    try:
        con.execute('PRAGMA busy_timeout=10000')
        con.execute('BEGIN IMMEDIATE')
        old=con.execute('SELECT payload_hash FROM accepted WHERE tenant=? AND device=? AND sequence=?',(tenant,device,seq)).fetchone()
        if old:
            con.commit()
            if old[0] != h: return 409,{'error':'sequence_conflict'}
            return 200,{'version':'jani-jsmt/1','ack_sequence':seq,'payload_sha256':h,'duplicate':True}
        cursor=con.execute('SELECT last_seq FROM device_cursors WHERE tenant=? AND device=?',(tenant,device)).fetchone()
        expected=(cursor[0] if cursor else 0)+1
        if seq != expected:
            con.rollback()
            return 409,{'error':'out_of_order','expected_sequence':expected}
        con.execute('INSERT INTO accepted VALUES(?,?,?,?,?)',(tenant,device,seq,h,canonical(env).decode('utf-8')))
        con.execute('INSERT INTO device_cursors VALUES (?,?,?) ON CONFLICT(tenant,device) DO UPDATE SET last_seq=excluded.last_seq',(tenant,device,seq))
        con.commit()
        return 200,{'version':'jani-jsmt/1','ack_sequence':seq,'payload_sha256':h,'duplicate':False}
    except Exception:
        con.rollback()
        raise
    finally: con.close()


def make_server(host:str,port:int,db:str,registry:dict,ca_cert:str,server_cert:str,server_key:str)->ThreadingHTTPServer:
    """Client identity = CA-authenticated certificate digest + exact registry binding."""
    if host not in ('127.0.0.1','::1','localhost') and not registry:
        raise ValueError('network exposure requires registered devices')
    create_db(db)
    class Handler(BaseHTTPRequestHandler):
        server_version='JaniMonitoringGateway/0.1'
        def log_message(self,format,*args):
            pass  # do not log sensitive bodies, CNs or tokens by default
        def _send(self,code, obj):
            payload=canonical(obj)
            self.send_response(code)
            self.send_header('Content-Type','application/json')
            self.send_header('Content-Length',str(len(payload)))
            self.send_header('Cache-Control','no-store')
            self.end_headers()
            self.wfile.write(payload)
        def do_GET(self):
            # Health endpoint still requires mTLS at the TLS handshake.
            if self.path != "/health":
                return self._send(404,{"error":"not_found"})
            return self._send(200,{"status":"ready","transport":"mtls-tls13"})
        def do_POST(self):
            if self.path!='/v1/ingest':
                return self._send(404,{'error':'not_found'})
            cert=self.connection.getpeercert(binary_form=True)
            if not cert:
                return self._send(401,{'error':'client_certificate_required'})
            fp=hashlib.sha256(cert).hexdigest()
            device=registry.get(fp)
            if not device:
                return self._send(403,{'error':'unregistered_certificate'})
            try:
                length=int(self.headers.get('Content-Length','0'))
            except ValueError:
                return self._send(400,{'error':'bad_length'})
            if length<=0 or length>MAX_PAYLOAD_BYTES:
                return self._send(413,{'error':'payload_bounds'})
            if self.headers.get('Content-Type','')!='application/json':
                return self._send(415,{'error':'json_required'})
            try:
                env=json.loads(self.rfile.read(length))
                if env.get('tenant_id')!=device['tenant_id'] or env.get('device_id')!=device['device_id']:
                    return self._send(403,{'error':'client_identity_mismatch'})
                code,reply=commit_envelope(db,env)
                return self._send(code,reply)
            except (ValueError,TypeError,KeyError) as exc:
                return self._send(400,{'error':'invalid_envelope','detail':str(exc)[:100]})
            except sqlite3.Error:
                return self._send(503,{'error':'store_unavailable'})
    srv=ThreadingHTTPServer((host,port),Handler)
    srv.daemon_threads=True
    context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version=ssl.TLSVersion.TLSv1_3
    context.maximum_version=ssl.TLSVersion.TLSv1_3
    context.load_cert_chain(server_cert,server_key)
    context.load_verify_locations(cafile=ca_cert)
    context.verify_mode=ssl.CERT_REQUIRED
    srv.socket=context.wrap_socket(srv.socket,server_side=True)
    return srv


def accepted(db:str)->list[dict]:
    c=sqlite3.connect(db)
    try:return [json.loads(row[0]) for row in c.execute('SELECT json FROM accepted ORDER BY tenant,device,sequence')]
    finally:c.close()
