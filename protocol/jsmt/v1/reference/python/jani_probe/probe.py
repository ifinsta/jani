"""JSMT/1 reference probe: scoped sources, persistent queue, verified TLS ACK."""
from __future__ import annotations
import json
from contextlib import closing
import sqlite3
import ssl
from urllib.parse import urlsplit
import urllib.error
import urllib.request
from pathlib import Path
from .collect import collect_services,coverage_report,linux_metrics,systemd_inventory,collect_journald
from .schema import VERSION,canonical,digest,utc_now,validate

class Probe:
    def __init__(self,tenant:str,device:str,queue_db:str,ca:str,cert:str,key:str,endpoint:str):
        parsed=urlsplit(endpoint)
        if parsed.scheme!='https' or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path not in ('','/'):
            raise ValueError('only verified HTTPS origin is allowed')
        self.tenant,self.device,self.queue_db=tenant,device,queue_db
        self.endpoint=endpoint
        ctx=ssl.create_default_context(ssl.Purpose.SERVER_AUTH,cafile=ca)
        ctx.minimum_version=ssl.TLSVersion.TLSv1_3
        ctx.maximum_version=ssl.TLSVersion.TLSv1_3
        ctx.load_cert_chain(certfile=cert,keyfile=key)
        ctx.check_hostname=True
        self.context=ctx
        class RefuseRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, req, fp, code, msg, headers, newurl):
                raise RuntimeError('redirect refused by JSMT')
        self.opener=urllib.request.build_opener(urllib.request.HTTPSHandler(context=ctx),RefuseRedirect())
        with closing(sqlite3.connect(queue_db)) as con:
            con.execute('PRAGMA journal_mode=WAL')
            con.execute('PRAGMA synchronous=FULL')
            con.execute('CREATE TABLE IF NOT EXISTS pending (seq INTEGER PRIMARY KEY,payload BLOB NOT NULL,hash TEXT NOT NULL,offsets TEXT NOT NULL)')
            con.execute('CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)')
            con.commit()
    def enqueue(self,services:list[dict],discovered:list[str]|None=None, journal:bool=False)->dict:
        con=sqlite3.connect(self.queue_db,timeout=10,isolation_level=None)
        try:
            con.execute('BEGIN IMMEDIATE')
            outstanding=con.execute('SELECT COUNT(*) FROM pending').fetchone()[0]
            if outstanding>=1000:
                raise RuntimeError('offline queue quota reached; no silent drops')
            old={k:int(v) for k,v in con.execute("SELECT key,value FROM meta WHERE key LIKE 'offset:%'")}
            offsets={k.removeprefix('offset:'):v for k,v in old.items()}
            metrics,logs,coverage,new_offsets=collect_services(services,offsets)
            if len(logs)>200:
                raise RuntimeError("log batch exceeds limit; all source cursors preserved for retry")
            discovered=discovered if discovered is not None else systemd_inventory()
            journal_health=None
            if journal and len(logs)<200:
                previous_cursor=con.execute("SELECT value FROM meta WHERE key='journal_cursor'").fetchone()
                jlogs,jcursor,journal_health=collect_journald(previous_cursor[0] if previous_cursor else None, max_entries=min(100,200-len(logs)))
                # No source is ever marked fully covered merely because journalctl is accessible.
                logs.extend(jlogs[:max(0,200-len(logs))])
                seen={log['resource']['service.name'] for log in jlogs}
                for name in discovered:
                    status='covered' if name in seen and journal_health['status']=='covered' else 'partial'
                    if not any(c['service']==name and c['status']=='covered' for c in coverage):
                        coverage.append({'service':name,'source':'journald','status':status,'reason':journal_health['reason'][:128] if journal_health else 'no visible journal events'})
                if jcursor and jcursor!=(previous_cursor[0] if previous_cursor else None):
                    con.execute("INSERT INTO meta(key,value) VALUES('journal_cursor',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",(jcursor,))
            if len(coverage)>256 or len(logs)>200:
                raise RuntimeError("coverage/log batch exceeds limit; no records silently discarded")
            report=coverage_report(services,coverage,discovered)
            metrics.extend(linux_metrics())
            previous=con.execute("SELECT value FROM meta WHERE key='sequence'").fetchone()
            sequence=(int(previous[0]) if previous else 0)+1
            env={'version':VERSION,'tenant_id':self.tenant,'device_id':self.device,'sequence':sequence,'observed_at':utc_now(), 'metrics':metrics,'logs':logs,'coverage':coverage}
            validate(env)
            body=canonical(env)
            # Offset progress is queued with payload; it is not committed on loss of telemetry.
            con.execute('INSERT INTO pending VALUES (?,?,?,?)',(sequence,body,digest(env),json.dumps(new_offsets)))
            con.execute("INSERT INTO meta VALUES ('sequence',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",(str(sequence),))
            # To avoid overlapping re-collection before ACK, store a staging cursor separate from ACKed offset.
            for name,pos in new_offsets.items():
                con.execute('INSERT INTO meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value',('offset:'+name,str(pos)))
            con.commit()
            return {'sequence':sequence,'metrics':len(metrics),'logs':len(logs),'coverage':report,'journal':journal_health}
        except Exception:
            con.rollback()
            raise
        finally:con.close()
    def flush(self,max_items:int=32)->dict:
        sent=0
        with closing(sqlite3.connect(self.queue_db,timeout=10)) as con:
            items=con.execute('SELECT seq,payload,hash FROM pending ORDER BY seq LIMIT ?', (max_items,)).fetchall()
        for seq,payload,digest_val in items:
            req=urllib.request.Request(self.endpoint+'/v1/ingest',data=payload,method='POST',headers={'Content-Type':'application/json'})
            try:
                with self.opener.open(req,timeout=5) as response:
                    reply=json.loads(response.read(8192))
                    if response.status!=200 or reply.get('version')!=VERSION or reply.get('ack_sequence')!=seq or reply.get('payload_sha256')!=digest_val:
                        raise RuntimeError('invalid or mismatched server ACK')
            except (urllib.error.URLError,OSError):
                return {'sent':sent,'pending':self.pending(),'status':'transport_unavailable'}
            with closing(sqlite3.connect(self.queue_db)) as con:
                con.execute('DELETE FROM pending WHERE seq=? AND hash=?',(seq,digest_val))
                con.commit()
            sent+=1
        return {'sent':sent,'pending':self.pending(),'status':'ok'}
    def pending(self)->int:
        with closing(sqlite3.connect(self.queue_db)) as con:
            return con.execute('SELECT COUNT(*) FROM pending').fetchone()[0]
