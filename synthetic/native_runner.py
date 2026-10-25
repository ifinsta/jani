"""Safely execute a bounded baseline smoke suite against a real built Jani CLI.

This is not a fault injector for all 240 modes. Results never self-certify production.
"""
import argparse,hashlib,json,os,subprocess,tempfile
from pathlib import Path
from .native_tools import TOOLS
PROBES=(
 ("index",("build",),True),
 ("index",("search","hello"),True),
 ("context",("pin","src/hello.rs"),True),
 ("context",("assemble","hello","800"),True),
 ("planner",("create","plan.json"),True),
 ("pruner",("preview",),True),
 ("checkpoint",("create",),True),
 ("verifier",("select",),True),
 ("impact",("analyze","src/hello.rs"),True),
 ("scheduler",("next",),True),
 ("workspace",("status",),True),
 ("evidence",("record","ev-1","src/hello.rs"),True),
 ("optimizer",("review",),True),
 ("simulator",("run","42"),True),
 ("planner",("verify","inspect"),False),
 ("verifier",("run",),False),
 ("workspace",("rollback",),False),
)
def run(binary:Path,output:Path):
 if not binary.is_file():raise ValueError("compiled Jani executable required")
 output.parent.mkdir(parents=True,exist_ok=True)
 env={k:v for k,v in os.environ.items() if k not in {"JANI_API_KEY","OPENAI_API_KEY","JANI_POLICY","JANI_IDENTITY","JANI_MODE"} and not k.startswith("JANI_MCP")}
 with tempfile.TemporaryDirectory(prefix="jani-native-smoke-") as temp:
  repo=Path(temp)
  (repo/"src").mkdir()
  (repo/"src/hello.rs").write_text("pub fn hello() {}\n")
  (repo/"plan.json").write_text(json.dumps({"nodes":[
   {"id":"inspect","deps":[],"status":"planned","acceptance":"compile","priority":3}]}))
  results=[]
  for tool,args,expected_ok in PROBES:
   cmd=[str(binary.resolve()),"native",tool,*args,
        "--repo",str(repo),"--state-dir",str(repo/".jani"),"--tenant","test",
        "--task","task-smoke"]
   try:
    proc=subprocess.run(cmd,env=env,cwd=repo,stdin=subprocess.DEVNULL,
         capture_output=True,timeout=12)
    ok=(proc.returncode==0)==expected_ok
    results.append({"tool":tool,"operation":args[0],"passed":ok,
       "actualExitCode":proc.returncode,"expectSuccess":expected_ok,
       "stdoutSha256":hashlib.sha256(proc.stdout).hexdigest(),
       "stderrSha256":hashlib.sha256(proc.stderr).hexdigest(),
       "executed":True,"independentSecurityCertification":False})
   except subprocess.TimeoutExpired:
    results.append({"tool":tool,"operation":args[0],"passed":False,
      "reason":"timeout","executed":True})
 with output.open("w") as f:
  for row in results:f.write(json.dumps(row,sort_keys=True)+"\n")
 return {"smokeCases":len(results),"tools":len(set(x["tool"] for x in results)),
    "passed":sum(x["passed"] for x in results),"failed":sum(not x["passed"] for x in results),
    "notExecutedCorpusCases":240_000,"productionReady":False}
def main():
 p=argparse.ArgumentParser();p.add_argument("--binary",required=True,type=Path)
 p.add_argument("--output",required=True,type=Path)
 a=p.parse_args();result=run(a.binary,a.output);print(json.dumps(result,indent=2))
 raise SystemExit(0 if result["failed"]==0 else 2)
if __name__=="__main__":main()
