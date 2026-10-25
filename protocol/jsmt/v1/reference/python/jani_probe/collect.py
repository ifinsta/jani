"""Bounded Linux-first sample collector and explicit service log coverage inventory."""
from __future__ import annotations
import json
import os
import shutil
from pathlib import Path
from .schema import normalize_log, normalize_metric


def linux_metrics() -> list[dict]:
    metrics = []
    if not Path('/proc').exists():
        return metrics
    try:
        uptime = float(Path('/proc/uptime').read_text().split()[0])
        metrics.append(normalize_metric('system.uptime', uptime))
    except (OSError, ValueError, IndexError):
        pass
    try:
        cpu = Path('/proc/stat').read_text().splitlines()[0].split()
        ticks = os.sysconf('SC_CLK_TCK')
        for i, name in enumerate(('user','nice','system','idle','iowait','interrupt','softirq','steal'), 1):
            if i < len(cpu):
                metrics.append(normalize_metric('system.cpu.time', float(cpu[i]) / ticks, {'cpu.mode': name}))
    except (OSError, ValueError, IndexError):
        pass
    try:
        mem = {}
        for line in Path('/proc/meminfo').read_text().splitlines():
            k, _, v = line.partition(':')
            mem[k] = int(v.strip().split()[0]) * 1024
        if 'MemTotal' in mem and 'MemAvailable' in mem:
            metrics.extend([normalize_metric('system.memory.usage', mem['MemTotal'] - mem['MemAvailable'], {'system.memory.state':'used'}),normalize_metric('system.memory.usage', mem['MemAvailable'], {'system.memory.state':'free'})])
    except (OSError, ValueError, IndexError):
        pass
    try:
        count = sum(1 for p in os.scandir('/proc') if p.name.isdecimal())
        metrics.append(normalize_metric('system.process.count', count, {'process.state':'all'}))
    except OSError:
        pass
    try:
        used = shutil.disk_usage('/').used
        metrics.append(normalize_metric('jani.filesystem.used', used, {'mountpoint':'/'}))
    except OSError:
        pass
    return metrics


def collect_services(services: list[dict], offsets: dict[str, int], max_lines=50) -> tuple[list[dict], list[dict], list[dict], dict]:
    """Only explicitly configured log sources. Every discovered service gets coverage row."""
    metrics, logs, coverage = [], [], []
    new_offsets = dict(offsets)
    for item in services:
        name = item['name']
        path = item.get('log_file')
        if not path:
            coverage.append({'service':name,'source':'','status':'missing','reason':'no log source configured'})
            metrics.extend([normalize_metric('jani.service.up', int(bool(item.get('running',True))), {'service.name':name}),normalize_metric('jani.service.log.covered',0,{'service.name':name})])
            continue
        file = Path(path)
        try:
            if not file.is_file() or file.is_symlink():
                raise OSError('log source unavailable or symlink')
            prev = max(0, offsets.get(name,0))
            with file.open('rb') as stream:
                stream.seek(0,2)
                length = stream.tell()
                if prev > length:
                    prev = 0  # rotation/truncation: restart, avoid silent omission
                stream.seek(prev)
                raw = stream.read(32768)
                valid = raw.rfind(b'\n') + 1
                if valid == 0 and len(raw) == 32768:
                    raise ValueError('unbounded log line')
                next_pos = prev + valid
                lines = raw[:valid].decode('utf-8',errors='replace').splitlines()
                # preserve unconsumed lines to be sent next cycle
                lines = lines[:max_lines]
                if len(lines) < len(raw[:valid].decode('utf-8',errors='replace').splitlines()):
                    next_pos = prev + sum(len((line+'\n').encode('utf-8')) for line in lines)
                new_offsets[name] = next_pos
                for line in lines:
                    level = 'ERROR' if 'ERROR' in line.upper() else 'WARN' if 'WARN' in line.upper() else 'INFO'
                    logs.append(normalize_log(name,line,'file:'+file.name,level))
            status='partial' if next_pos<length else 'covered'
            coverage.append({'service':name,'source':'file:'+file.name,'status':status,'reason':'unprocessed file backlog' if status=='partial' else ''})
            metrics.extend([normalize_metric('jani.service.up',int(bool(item.get('running',True))),{'service.name':name}),normalize_metric('jani.service.log.covered',1,{'service.name':name})])
        except (OSError,PermissionError,ValueError) as exc:
            coverage.append({'service':name,'source':'file:'+file.name,'status':'permission_denied' if isinstance(exc,PermissionError) else 'missing','reason':str(exc)[:128]})
            metrics.extend([normalize_metric('jani.service.up',int(bool(item.get('running',True))),{'service.name':name}),normalize_metric('jani.service.log.covered',0,{'service.name':name})])
    return metrics, logs, coverage, new_offsets


def systemd_inventory(timeout:float=3.0) -> list[str]:
    """Discovery only. No implicit claim that journald has each service's logs."""
    import subprocess
    try:
        run=subprocess.run(['systemctl','list-units','--type=service','--all','--no-legend','--no-pager','--plain'],capture_output=True,text=True,timeout=timeout,check=False)
    except (OSError,subprocess.TimeoutExpired):
        return []
    return [line.split()[0] for line in run.stdout.splitlines() if line.split() and line.split()[0].endswith('.service')][:256]


def coverage_report(services:list[dict], covered:list[dict], discovered:list[str]) -> dict:
    successfully_covered={c['service'] for c in covered if c['status']=='covered'}
    expected=set(discovered)|{s['name'] for s in services}
    missing=sorted(x for x in expected if x not in successfully_covered)
    return {'expected_services':len(expected),'covered_services':len(expected)-len(missing),
            'missing_services':missing,'complete':len(missing)==0 and bool(expected),
            'discovery_supported':bool(discovered)}


def collect_journald(cursor: str | None, max_entries: int = 100, timeout: float = 4.0) -> tuple[list[dict], str | None, dict]:
    """Read authorized systemd journal entries without requiring shell or an agent-owned daemon.

    Coverage = source access, not an assertion that every quiet service emitted logs.
    Return same cursor on any error so no records are silently acknowledged.
    """
    import subprocess
    max_entries = min(max(1, max_entries), 100)
    cmd = ['journalctl', '--system', '--no-pager', '--output=json', '--quiet', '--no-hostname', '-n', str(max_entries)]
    if cursor:
        if len(cursor) > 256 or any(ord(ch)<32 for ch in cursor):
            raise ValueError('invalid journal cursor')
        cmd += ['--after-cursor', cursor]
    try:
        result = subprocess.run(cmd,capture_output=True,text=True,timeout=timeout,check=False)
    except FileNotFoundError:
        return [],cursor,{'status':'unsupported','reason':'journalctl not installed'}
    except (subprocess.TimeoutExpired,OSError):
        return [],cursor,{'status':'partial','reason':'journal read timed out or failed'}
    if result.returncode != 0:
        return [],cursor,{'status':'permission_denied','reason':'journal access denied or unavailable'}
    records=[]
    last_cursor=cursor
    for line in result.stdout.splitlines()[:max_entries]:
        try:
            event=json.loads(line)
            if not isinstance(event,dict): continue
            service=str(event.get('_SYSTEMD_UNIT') or event.get('SYSLOG_IDENTIFIER') or 'unknown')[:96]
            service = ''.join(ch if ch.isalnum() or ch in '._@-' else '_' for ch in service) or 'unknown'
            message=event.get('MESSAGE','')
            if not isinstance(message,str):message='<non-text journal message>'
            priority=int(event.get('PRIORITY',6))
            severity='ERROR' if priority<=3 else 'WARN' if priority==4 else 'DEBUG' if priority==7 else 'INFO'
            records.append(normalize_log(service,message,'journald',severity))
            reported=event.get('__CURSOR')
            if isinstance(reported,str) and 0<len(reported)<=256:
                last_cursor=reported
        except (ValueError,TypeError):
            # Unparseable records mean we cannot safely advance the cursor beyond that point.
            return records,cursor,{'status':'partial','reason':'invalid JSON journal record'}
    return records,last_cursor,{'status':'covered','reason':'journal access succeeded; logs visible only if emitted and retained'}
