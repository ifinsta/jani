"""Run `python -m jani_probe.cli gateway|probe` with external configs/certs."""
import argparse
import json
import ssl
from pathlib import Path
from .gateway import make_server
from .probe import Probe


def main():
    p=argparse.ArgumentParser(description='Private Jani telemetry prototype (TLS 1.3 mTLS)')
    s=p.add_subparsers(dest='cmd',required=True)
    g=s.add_parser('gateway')
    for name in ('db','registry','ca','cert','key'):
        g.add_argument('--'+name,required=True)
    g.add_argument('--host',default='127.0.0.1')
    g.add_argument('--port',type=int,default=9443)
    c=s.add_parser('probe')
    c.add_argument('--config',required=True,help='JSON, includes endpoint and cert references')
    c.add_argument('--queue',required=True)
    a=p.parse_args()
    if a.cmd=='gateway':
        registry=json.loads(Path(a.registry).read_text())
        server=make_server(a.host,a.port,a.db,registry,a.ca,a.cert,a.key)
        print('JSMT TLS1.3 mTLS gateway listening',server.server_address,flush=True)
        try:server.serve_forever()
        finally:server.server_close()
    else:
        cfg=json.loads(Path(a.config).read_text())
        probe=Probe(cfg['tenant_id'],cfg['device_id'],a.queue,cfg['ca'],cfg['cert'],cfg['key'],cfg['endpoint'])
        report=probe.enqueue(cfg['services'], journal=bool(cfg.get('journald',False)))
        print(json.dumps({'collected':report,'transport':probe.flush()},indent=2))

if __name__=='__main__':
    main()
