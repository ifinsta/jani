#!/usr/bin/env python3
"""Deterministic 5M-row adversarial fixture generator; Python stdlib only.

Fixtures are scenario INPUTS, NOT test executions or verified Jani behaviors.
"""
from __future__ import annotations
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import time
from .catalog import FAULTS, PROFILES, VARIANTS, OUTCOMES, DOMAIN_INVARIANT, SOURCE_MAP

MASK=(1<<64)-1
DOMAINS=tuple(FAULTS)
TOTAL_FAULTS=sum(map(len,FAULTS.values()))

def mix(n):
    n=(n+0x9e3779b97f4a7c15)&MASK
    n=((n^(n>>30))*0xbf58476d1ce4e5b9)&MASK
    n=((n^(n>>27))*0x94d049bb133111eb)&MASK
    return n^(n>>31)

def case(index,seed=20261025):
    x=index-1
    d=DOMAINS[x%len(DOMAINS)]
    modes=FAULTS[d]
    epoch=x//len(DOMAINS)
    fault=modes[epoch%len(modes)]
    variation=VARIANTS[(epoch//len(modes))%len(VARIANTS)]
    profile=PROFILES[(epoch//(len(modes)*len(VARIANTS))+x//97)%len(PROFILES)]
    r=mix(seed^index)
    secondary_domain=DOMAINS[(r>>11)%len(DOMAINS)]
    secondary=(FAULTS[secondary_domain][(r>>23)%len(FAULTS[secondary_domain])]
               if index%7==0 else None)
    tertiary=(FAULTS[DOMAINS[(r>>19)%len(DOMAINS)]][(r>>37)%len(FAULTS[DOMAINS[(r>>19)%len(DOMAINS)]])]
              if index%101==0 else None)
    tenant=f"tenant-{1+(r%1009):04d}"
    source_tenant=(f"tenant-{1+((r+1)%1009):04d}" if index%13==0 else tenant)
    risk=("critical" if d in {"identity","delegation","mcp","approvals","policy","data_governance","cross_system","sandbox"} else "high")
    expected=("PERMIT" if variation=="clean_valid_context" and index%2==0 else
              "RECONCILE" if "ambiguous" in fault or variation=="ambiguous_effect" else
              "AWAIT_HUMAN" if "approval" in fault and variation=="approval_forged" else
              "QUARANTINE" if d in {"data_governance","claims","mirror"} else
              "RETRY" if d=="model_gateway" and fault in {"model_408","model_429","model_500","model_502","model_503","model_504"} else
              "FAIL_CLOSED" if d in {"storage","audit","operations"} else "DENY")
    if source_tenant!=tenant or secondary is not None or tertiary is not None: expected="DENY"
    return {
      "id":f"JANI-{index:010d}","seed":seed,"domain":d,"source":SOURCE_MAP[d],
      "fault":fault,"secondary":secondary,"tertiary":tertiary,"mutation":variation,
      "profile":profile,"risk":risk,
      "context":{"tenant":tenant,"source_tenant":source_tenant,
         "user":f"principal-{1+(r>>13)%5003:05d}","agent":f"agent-{1+(r>>26)%113:03d}",
         "task":f"task-{1+(r>>5)%200_003:06d}","resource":f"record-{1+(r>>39)%100_003:06d}",
         "classification":("PUBLIC","INTERNAL","CONFIDENTIAL","RESTRICTED","SECRET")[(r>>9)%5],
         "concurrent":1+(r>>17)%1024,"clock_offset_s":int((r>>31)%86401)-43200,
         "payload_bytes":(0,1,1024,65536,1048576,16777216)[(r>>47)%6]},
      "oracle":{"decision":expected,"invariant":DOMAIN_INVARIANT[d],
         "no_unapproved_side_effect":True,"requires_audit":True,"must_not_leak_secrets":True},
      "replay":{"fault_hook":f"{d}.{fault}","case_seed":r,"executed":False}
    }

def generate(output:Path,count:int=5_000_000,shard_size:int=100_000,seed:int=20261025,quiet=False):
    if not (0<count<=20_000_000) or not (1<=shard_size<=500_000): raise ValueError("invalid size")
    output=Path(output);output.mkdir(parents=True,exist_ok=True)
    shard_dir=output/"shards";shard_dir.mkdir(exist_ok=True)
    manifest_path=output/"manifest.json"
    if manifest_path.exists() or list(shard_dir.iterdir()):
        raise ValueError("output is not empty; use a fresh directory to avoid corrupted mixed corpus")
    shard_list=[];counts={d:0 for d in DOMAINS};decisions={};faults=set();mutations=set();profiles=set()
    t0=time.monotonic()
    for start in range(1,count+1,shard_size):
        end=min(count,start+shard_size-1)
        name=f"cases_{start:010d}_{end:010d}.jsonl.gz";path=shard_dir/name
        with path.open("wb") as raw:
            with gzip.GzipFile(fileobj=raw,mode="wb",compresslevel=1,mtime=0,filename="") as gz:
                for i in range(start,end+1):
                    row=case(i,seed)
                    gz.write((json.dumps(row,separators=(",",":"),ensure_ascii=True)+"\n").encode())
                    counts[row["domain"]]+=1;faults.add(row["domain"]+"."+row["fault"])
                    decisions[row["oracle"]["decision"]]=decisions.get(row["oracle"]["decision"],0)+1
                    mutations.add(row["mutation"]);profiles.add(row["profile"])
        with path.open("rb") as digest_input: digest=hashlib.file_digest(digest_input,"sha256").hexdigest()
        shard_list.append({"file":"shards/"+name,"first":start,"last":end,"rows":end-start+1,"sha256":digest,"bytes":path.stat().st_size})
        if not quiet:print(f"generated {end:,}/{count:,} cases | {len(shard_list)} shards",flush=True)
    report={"schema":"jani.synthetic/v1","count":count,"seed":seed,"shard_size":shard_size,
       "catalogued_faults":TOTAL_FAULTS,"represented_faults":len(faults),
       "domains":counts,"decisions":decisions,"mutation_variants":len(mutations),"profiles":len(profiles),
       "distinct_primary_templates":len(faults)*len(mutations),
       "note":"Generated scenario expectations; none of these rows has executed Jani."}
    (output/"coverage.json").write_text(json.dumps(report,indent=2,sort_keys=True)+"\n")
    manifest={"schema":"jani.synthetic.manifest/v1","generator":"synthetic/generate.py",
      "records":count,"seed":seed,"shard_size":shard_size,"compression":"gzip level 1 mtime 0",
      "hash":"sha256","shards":shard_list,"coverage":"coverage.json",
      "executed_cases":0,"production_ready":False,"requirements":"No readiness verdict without real execution receipts."}
    manifest_path.write_text(json.dumps(manifest,indent=2,sort_keys=True)+"\n")
    if not quiet:print(f"completed in {time.monotonic()-t0:.1f}s; represented {len(faults)} primary faults across {len(counts)} domains",flush=True)
    return manifest

def verify(root:Path,full=True):
    manifest=json.loads((root/"manifest.json").read_text());total=0;seen=set()
    for shard in manifest["shards"]:
        path=root/shard["file"]
        with path.open("rb") as f: digest=hashlib.file_digest(f,"sha256").hexdigest()
        if digest!=shard["sha256"]:raise ValueError("checksum failure: "+shard["file"])
        count=0
        if full:
            with gzip.open(path,"rt",encoding="utf-8") as f:
                for line in f:
                    item=json.loads(line);count+=1;index=shard["first"]+count-1
                    if item!=case(index,manifest["seed"]):raise ValueError(f"oracle/sequence mismatch {index}")
                    seen.add(item["domain"])
            if count!=shard["rows"]:raise ValueError("shard row-count mismatch")
        total+=shard["rows"]
    if total!=manifest["records"]:raise ValueError("manifest total mismatch")
    if full and len(seen)!=len(DOMAINS):raise ValueError("not all domains represented")
    print(json.dumps({"verified":True,"rows":total,"shards":len(manifest["shards"]),"full":full,"source_domains":len(seen) if full else None}))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    subs=parser.add_subparsers(dest="action",required=True)
    p=subs.add_parser("generate");p.add_argument("--output",type=Path,required=True);p.add_argument("--count",type=int,default=5_000_000);p.add_argument("--shard-size",type=int,default=100_000);p.add_argument("--seed",type=int,default=20261025);p.add_argument("--quiet",action="store_true")
    v=subs.add_parser("verify");v.add_argument("--output",type=Path,required=True);v.add_argument("--checksums-only",action="store_true")
    s=subs.add_parser("sample");s.add_argument("--count",type=int,default=10);s.add_argument("--seed",type=int,default=20261025)
    a=parser.parse_args()
    if a.action=="generate":generate(a.output,a.count,a.shard_size,a.seed,a.quiet)
    elif a.action=="verify":verify(a.output,not a.checksums_only)
    elif a.action=="sample":
        for i in range(1,a.count+1):print(json.dumps(case(i,a.seed),separators=(",",":")))
if __name__=="__main__":main()
