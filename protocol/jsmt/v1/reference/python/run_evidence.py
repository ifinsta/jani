"""Capture a machine-readable evidence report from the real mTLS unit/integration suite."""
import json
import ssl
import subprocess
import sys
from datetime import datetime,timezone
from pathlib import Path

out=subprocess.run([sys.executable,'-m','unittest','discover','-s','tests','-v'],capture_output=True,text=True,timeout=90)
result={
 'timestamp_utc':datetime.now(timezone.utc).isoformat(),
 'implementation':'Python JSMT/1 reference prototype (NOT Jani Rust runtime)',
 'test_command':'python -m unittest discover -s tests -v',
 'process_exit_code':out.returncode,
 'tls_provider':ssl.OPENSSL_VERSION,
 'stdout':out.stdout[-10000:],
 'stderr':out.stderr[-10000:],
 'production_certified':False,
 'coverage_scope':'3 explicitly configured synthetic host services only; NOT universal service discovery',
 'observed_encryption':'TLSv1.3 required; mTLS certificate verification; local negotiation asserted in test',
 'limitations':['No standalone Rust binaries compiled or exercised', 'No signed enterprise enrollment or revocation','No OS-native Windows/journald/container collector', 'No HA, DR, migration or high-load benchmark'],
}
Path('evidence').mkdir(parents=True,exist_ok=True)
Path('evidence/protocol_e2e.json').write_text(json.dumps(result,indent=2)+'\n')
print(out.stderr)
print('Evidence:',Path('evidence/protocol_e2e.json').resolve())
sys.exit(out.returncode)
