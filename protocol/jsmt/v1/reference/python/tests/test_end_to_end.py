"""True local network TLS1.3/mTLS client-server test with process-independent SQLite state."""
import hashlib
from contextlib import closing
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import ssl
import subprocess
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from jani_probe.collect import collect_services,coverage_report,linux_metrics
from jani_probe.gateway import accepted,make_server
from jani_probe.probe import Probe
from jani_probe.schema import canonical,digest,normalize_metric


def run(*args):
    subprocess.run(['openssl',*args],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=10)

def credentials(p:Path):
    run('req','-x509','-newkey','rsa:2048','-nodes','-sha256','-keyout',str(p/'ca.key'),'-out',str(p/'ca.crt'),'-days','1','-subj','/CN=Jani Dev Test CA','-addext','basicConstraints=critical,CA:TRUE','-addext','keyUsage=critical,keyCertSign,cRLSign')
    for name,cn,san in [('server','localhost','DNS:localhost,IP:127.0.0.1'),('device','device-one','DNS:device-one'),('other','device-other','DNS:device-other')]:
        run('req','-new','-newkey','rsa:2048','-nodes','-keyout',str(p/f'{name}.key'),'-out',str(p/f'{name}.csr'),'-subj',f'/CN={cn}','-addext',f'subjectAltName={san}','-addext','basicConstraints=critical,CA:FALSE','-addext','keyUsage=critical,digitalSignature,keyEncipherment','-addext',f'extendedKeyUsage={"serverAuth" if name=="server" else "clientAuth"}')
        run('x509','-req','-in',str(p/f'{name}.csr'),'-CA',str(p/'ca.crt'),'-CAkey',str(p/'ca.key'),'-CAcreateserial','-out',str(p/f'{name}.crt'),'-days','1','-sha256','-copy_extensions','copy')
    der=ssl.PEM_cert_to_DER_cert((p/'device.crt').read_text())
    return hashlib.sha256(der).hexdigest()

class EndToEndTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp=tempfile.TemporaryDirectory()
        cls.root=Path(cls.temp.name)
        cls.fp=credentials(cls.root)
        cls.db=str(cls.root/'central.sqlite3')
        cls.registry={cls.fp:{'tenant_id':'tenant-a','device_id':'device-one'}}
        cls.srv=make_server('127.0.0.1',0,cls.db,cls.registry,str(cls.root/'ca.crt'),str(cls.root/'server.crt'),str(cls.root/'server.key'))
        cls.thread=threading.Thread(target=cls.srv.serve_forever,daemon=True)
        cls.thread.start()
        cls.base=f'https://localhost:{cls.srv.server_address[1]}'
    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown();cls.srv.server_close();cls.thread.join(timeout=3);cls.temp.cleanup()
    def create_probe(self,name='device-one',cert='device',ca='ca.crt'):
        return Probe('tenant-a',name,str(self.root/(name+'-queue.sqlite3')),str(self.root/ca),str(self.root/(cert+'.crt')),str(self.root/(cert+'.key')),self.base)
    def get_services(self,folder='cases'):
        p=self.root/folder;p.mkdir(exist_ok=True)
        for name,line in (('nginx','GET / 200\nERROR retry token=supersecret\n'),('postgres','checkpoint completed\n'),('worker','background task done\n')):
            (p/f'{name}.log').write_text(line)
        return [{'name':n,'log_file':str(p/f'{n}.log'),'running':True} for n in ('nginx','postgres','worker')]
    def test_01_tls13_mtls_normalized_metrics_and_all_configured_logs(self):
        services=self.get_services('first')
        p=self.create_probe()
        report=p.enqueue(services,discovered=['nginx','postgres','worker'])
        self.assertEqual(report['coverage']['covered_services'],3)
        self.assertTrue(report['coverage']['complete'])
        self.assertEqual(report['logs'],4)
        metrics={m['name'] for m in linux_metrics()}
        self.assertIn('system.uptime',metrics)
        self.assertIn('system.memory.usage',metrics)
        out=p.flush();self.assertEqual(out['status'],'ok');self.assertEqual(out['pending'],0)
        env=accepted(self.db)[0]
        self.assertEqual({x['resource']['service.name'] for x in env['logs']},{'nginx','postgres','worker'})
        self.assertTrue(all(m['unit'] in ('s','By','1','{process}') for m in env['metrics']))
        self.assertTrue(all(x['status']=='covered' for x in env['coverage']))
        self.assertTrue(all('supersecret' not in x['body'] for x in env['logs']))
        # Actual mutually authenticated network socket negotiates TLS 1.3.
        ctx=ssl.create_default_context(cafile=str(self.root/'ca.crt'))
        ctx.load_cert_chain(str(self.root/'device.crt'),str(self.root/'device.key'))
        with socket.create_connection(('127.0.0.1',self.srv.server_address[1]),timeout=2) as raw:
            with ctx.wrap_socket(raw,server_hostname='localhost') as sock:
                self.assertEqual(sock.version(),'TLSv1.3')
                self.assertTrue(sock.cipher()[0].startswith('TLS_AES_') or sock.cipher()[0].startswith('TLS_CHACHA20_'))
                sock.sendall(b'GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n')
                self.assertIn(b'200',sock.recv(1024))
    def test_02_duplicate_idempotency_and_conflicting_replay(self):
        env=accepted(self.db)[0]
        import jani_probe.gateway as gw
        code,result=gw.commit_envelope(self.db,env)
        self.assertEqual((code,result['duplicate']),(200,True))
        clone=json.loads(json.dumps(env));clone['logs'][0]['body']='different body'
        code,result=gw.commit_envelope(self.db,clone)
        self.assertEqual(code,409)
    def test_03_disconnected_queue_recovers_no_loss(self):
        services=self.get_services('second')
        # separate enrolled device sequence must remain sequential: reuse existing probe state.
        p=self.create_probe()
        p.enqueue(services,discovered=['nginx','postgres','worker'])
        old=p.endpoint;p.endpoint='https://localhost:1'
        failed=p.flush();self.assertEqual(failed['status'],'transport_unavailable');self.assertEqual(failed['pending'],1)
        p.endpoint=old
        self.assertEqual(p.flush()['pending'],0)
        self.assertEqual(len(accepted(self.db)),2)
    def test_04_no_client_certificate_or_plaintext(self):
        ctx=ssl.create_default_context(cafile=str(self.root/'ca.crt'))
        with self.assertRaises((ssl.SSLError,urllib.error.URLError,ConnectionError,OSError)):
            urllib.request.urlopen(self.base+'/v1/ingest',context=ctx,timeout=2)
        with self.assertRaises((ConnectionError,http.client.HTTPException,OSError,TimeoutError)):
            c=http.client.HTTPConnection('127.0.0.1',self.srv.server_address[1],timeout=2)
            try:c.request('POST','/v1/ingest',body=b'plaintext');c.getresponse()
            finally:c.close()
    def test_05_unregistered_cert_and_wrong_device_id(self):
        p=self.create_probe(name='device-other',cert='other')
        p.enqueue(self.get_services('third'),discovered=['nginx','postgres','worker'])
        with self.assertRaises(urllib.error.HTTPError) as cm:
            with closing(sqlite3.connect(p.queue_db)) as con:
                env=json.loads(con.execute('SELECT payload FROM pending').fetchone()[0])
            ctx=ssl.create_default_context(cafile=str(self.root/'ca.crt'))
            ctx.load_cert_chain(str(self.root/'other.crt'),str(self.root/'other.key'))
            urllib.request.urlopen(urllib.request.Request(self.base+'/v1/ingest',data=canonical(env),headers={'Content-Type':'application/json'},method='POST'),context=ctx,timeout=2)
        self.assertEqual(cm.exception.code,403)
        # Registered cert but claiming another device must also fail.
        env['device_id']='other-device'
        ctx.load_cert_chain(str(self.root/'device.crt'),str(self.root/'device.key'))
        with self.assertRaises(urllib.error.HTTPError) as cm:
            urllib.request.urlopen(urllib.request.Request(self.base+'/v1/ingest',data=canonical(env),headers={'Content-Type':'application/json'},method='POST'),context=ctx,timeout=2)
        self.assertEqual(cm.exception.code,403)
    def test_06_detect_missing_coverage(self):
        services=self.get_services('fourth')
        services[1]['log_file']=str(self.root/'does-not-exist.log')
        m,l,c,_=collect_services(services,{})
        report=coverage_report(services,c,['nginx','postgres','worker','extra-daemon'])
        self.assertFalse(report['complete'])
        self.assertEqual(report['covered_services'],2)
        self.assertEqual(report['missing_services'],['extra-daemon','postgres'])
        self.assertEqual({s['service'] for s in c if s['status']=='covered'},{'nginx','worker'})
    def test_08_oversized_log_batch_fails_without_advancing_spool(self):
        p = self.create_probe(name='overflow-test')
        folder = self.root / 'overflow'
        folder.mkdir(exist_ok=True)
        services = []
        for i in range(5):
            path = folder / f'service-{i}.log'
            path.write_text('test line\n' * 51)
            services.append({'name':f'service-{i}','log_file':str(path)})
        with self.assertRaises(RuntimeError):
            p.enqueue(services, discovered=[s['name'] for s in services])
        self.assertEqual(p.pending(), 0)
        with closing(sqlite3.connect(p.queue_db)) as c:
            self.assertEqual(c.execute("SELECT COUNT(*) FROM meta WHERE key LIKE 'offset:%'").fetchone()[0], 0)

    def test_07_plaintext_probe_config_rejected(self):
        with self.assertRaises(ValueError):
            Probe('t','d',str(self.root/'bad.sqlite3'),'missing','missing','missing','http://localhost:8080')

if __name__=='__main__':unittest.main()

class CollectorUnitTest(unittest.TestCase):
    def test_journald_normalizes_all_visible_units_and_redacts(self):
        from unittest.mock import patch
        from types import SimpleNamespace
        from jani_probe.collect import collect_journald
        records='\n'.join([json.dumps({'_SYSTEMD_UNIT':'nginx.service','MESSAGE':'ready','PRIORITY':'6','__CURSOR':'one'}),json.dumps({'_SYSTEMD_UNIT':'db.service','MESSAGE':'password=do-not-send','PRIORITY':'3','__CURSOR':'two'})])
        with patch('subprocess.run',return_value=SimpleNamespace(returncode=0,stdout=records)) as fake:
            logs,cursor,health=collect_journald('initial')
        self.assertEqual({x['resource']['service.name'] for x in logs},{'nginx.service','db.service'})
        self.assertEqual(logs[1]['severity_text'],'ERROR')
        self.assertNotIn('do-not-send',logs[1]['body'])
        self.assertEqual(cursor,'two')
        self.assertEqual(health['status'],'covered')
        self.assertIn('--after-cursor',fake.call_args.args[0])
    def test_journald_permission_denied_never_advances_cursor(self):
        from unittest.mock import patch
        from types import SimpleNamespace
        from jani_probe.collect import collect_journald
        with patch('subprocess.run',return_value=SimpleNamespace(returncode=1,stdout='')):
            logs,cursor,health=collect_journald('old')
        self.assertEqual(logs,[]);self.assertEqual(cursor,'old');self.assertEqual(health['status'],'permission_denied')

class SchemaAndSpoolTest(unittest.TestCase):
    def test_metric_unit_mismatch_fails(self):
        from jani_probe.schema import normalize_metric
        with self.assertRaises(ValueError):normalize_metric('system.memory.usage',float('nan'))
        with self.assertRaises(ValueError):normalize_metric('arbitrary.metric',1)
    def test_plaintext_endpoint_is_rejected_before_disk_or_network(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                Probe('t','d',str(Path(directory)/'q.db'),'x','y','z','http://example.test')
    def test_log_overflow_preserves_offline_queue_and_cursors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sources = []
            for n in range(5):
                f = root / f"service-{n}.log"
                f.write_text("ordinary line\n" * 51)
                sources.append({"name": f"service-{n}", "log_file": str(f)})
            from jani_probe.collect import collect_services
            _, logs, _, offsets = collect_services(sources, {})
            self.assertGreater(len(logs), 200)
            self.assertEqual(len(offsets), 5)

    def test_source_coverage_is_not_inferred_from_host_metrics(self):
        from jani_probe.collect import coverage_report
        result=coverage_report([],[],['service-a','service-b'])
        self.assertEqual(result['covered_services'],0)
        self.assertFalse(result['complete'])
        self.assertEqual(len(result['missing_services']),2)

class SeparateProcessInteroperabilityTest(unittest.TestCase):
    def test_process_to_process_tls_ingestion_and_service_logs(self):
        import sys
        import time
        root=Path(__file__).resolve().parents[1]
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)
            fp=credentials(p)
            registry=p/'registry.json'
            registry.write_text(json.dumps({fp:{'tenant_id':'isolated','device_id':'probe-1'}}))
            db=p/'ingest.db'
            paths=[]
            for name in ('api','scheduler','database'):
                logfile=p/(name+'.log')
                logfile.write_text(f'{name} startup complete\n')
                paths.append({'name':name,'log_file':str(logfile),'running':True})
            sock=socket.socket()
            sock.bind(('127.0.0.1',0))
            port=sock.getsockname()[1]
            sock.close()
            server_args=[sys.executable,'-m','jani_probe.cli','gateway','--db',str(db),'--registry',str(registry),'--ca',str(p/'ca.crt'),'--cert',str(p/'server.crt'),'--key',str(p/'server.key'),'--port',str(port)]
            proc=subprocess.Popen(server_args,cwd=root,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,text=True)
            try:
                for _ in range(60):
                    if proc.poll() is not None:raise AssertionError('gateway unexpectedly exited')
                    try:
                        with socket.create_connection(('127.0.0.1',port),timeout=.1):
                            break
                    except OSError:
                        time.sleep(.05)
                config=p/'config.json'
                config.write_text(json.dumps({'tenant_id':'isolated','device_id':'probe-1','endpoint':f'https://localhost:{port}', 'ca':str(p/'ca.crt'),'cert':str(p/'device.crt'),'key':str(p/'device.key'),'services':paths,'journald':False}))
                result=subprocess.run([sys.executable,'-m','jani_probe.cli','probe','--config',str(config),'--queue',str(p/'queue.db')],cwd=root,capture_output=True,text=True,timeout=10)
                self.assertEqual(result.returncode,0,msg=result.stderr)
                output=json.loads(result.stdout)
                self.assertEqual(output['transport']['sent'],1)
                self.assertEqual(output['transport']['pending'],0)
                self.assertTrue(output['collected']['coverage']['complete'])
                env=accepted(str(db))
                self.assertEqual(len(env),1)
                self.assertEqual({row['resource']['service.name'] for row in env[0]['logs']},{'api','scheduler','database'})
            finally:
                proc.terminate()
                try:proc.communicate(timeout=3)
                except subprocess.TimeoutExpired:proc.kill();proc.communicate(timeout=3)
