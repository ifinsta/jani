"""Restricted initial Jani binary probe runner, not a full five-million-case executor.
Only harmless CLI probes run in disposable directories; unsupported hooks SKIP.
"""
from __future__ import annotations
import argparse,hashlib,json,os,subprocess,tempfile,time
from pathlib import Path
from .generate import case

SUPPORTED = {
    ('cli','missing_argument'): (['mcp','authorize'],False),
    ('cli','unexpected_subcommand'): (['unsupported-command'],False),
    ('cli','unknown_mode'): (['doctor','--mode','not-a-mode'],False),
    ('cli','help_requires_key'): (['--help'],True),
}

def run_probe(binary:Path,index:int,seed=20261025):
    row=case(index,seed)
    probe=SUPPORTED.get((row['domain'],row['fault']))
    if not probe:
        return {'id':row['id'],'status':'SKIPPED','reason':'NO_ADAPTER_FOR_FAULT','fault_hook':row['replay']['fault_hook']}
    argv,expects_success=probe
    env={k:v for k,v in os.environ.items() if not (k.startswith('JANI_') or k in {'OPENAI_API_KEY','ANTHROPIC_API_KEY'})}
    start=time.monotonic()
    with tempfile.TemporaryDirectory() as workdir:
        try:
            r=subprocess.run([str(binary),*argv,'--repo',workdir],cwd=workdir,
                             stdin=subprocess.DEVNULL,capture_output=True,timeout=8,env=env)
            correct=(r.returncode==0)==expects_success
            return {'id':row['id'],'status':'PASSED' if correct else 'FAILED',
                'fault_hook':row['replay']['fault_hook'],
                'exit':r.returncode,'elapsed_s':round(time.monotonic()-start,6),
                'stdout_sha256':hashlib.sha256(r.stdout).hexdigest(),
                'stderr_sha256':hashlib.sha256(r.stderr).hexdigest(),
                'oracle':'exit_success' if expects_success else 'nonzero_exit'}
        except subprocess.TimeoutExpired:
            return {'id':row['id'],'status':'FAILED','reason':'PROBE_TIMEOUT','fault_hook':row['replay']['fault_hook']}

def main():
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--limit',type=int,default=1000);p.add_argument('--seed',type=int,default=20261025)
    args=p.parse_args();binary=args.binary.resolve()
    if not binary.is_file():raise SystemExit('Jani executable not found')
    if not (1<=args.limit<=100000):raise SystemExit('invalid limit')
    counts={'PASSED':0,'FAILED':0,'SKIPPED':0};args.output.parent.mkdir(parents=True,exist_ok=True)
    with args.output.open('w') as output:
        for i in range(1,args.limit+1):
            row=run_probe(binary,i,args.seed);counts[row['status']]+=1
            output.write(json.dumps(row,sort_keys=True)+'\n')
    print(json.dumps({'cases':args.limit,'results':counts,'source':'real_binary_probe',
                      'production_ready':False,'note':'Only supported CLI probes execute. Skips are not passes.'}))
if __name__=='__main__':main()
