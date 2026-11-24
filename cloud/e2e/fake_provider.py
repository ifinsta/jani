"""Scripted OpenAI-compatible streaming provider for the cloud end-to-end test.
No network, no real model: it returns fixed tool calls and a final answer."""
import json,sys,threading
from http.server import BaseHTTPRequestHandler,HTTPServer
def tc(i,name,args):
    return {"choices":[{"message":{"role":"assistant","content":None,"tool_calls":[{"id":i,"type":"function","function":{"name":name,"arguments":json.dumps(args)}}]}}],"usage":{"prompt_tokens":120,"completion_tokens":30}}
def fin(t):
    return {"choices":[{"message":{"role":"assistant","content":t}}],"usage":{"prompt_tokens":200,"completion_tokens":40}}
SCRIPTS=[
 [tc("c1","read",{"path":"README.md"}),tc("c2","list",{"directory":"."}),tc("c4","write",{"path":"out/notes.txt","content":"hi"}),fin("I read README.md, listed the workspace and wrote `out/notes.txt`.")],
 [fin("Nothing further to do.")],
]
state={"turn":-1,"step":0}
lock=threading.Lock()
class H(BaseHTTPRequestHandler):
    def log_message(self,*a):pass
    def do_POST(self):
        n=int(self.headers.get("content-length",0));body=json.loads(self.rfile.read(n) or b"{}")
        with lock:
            last=body["messages"][-1]
            if last.get("role")=="user":
                state["turn"]+=1;state["step"]=0
            sc=SCRIPTS[min(state["turn"],len(SCRIPTS)-1)]
            r=sc[min(state["step"],len(sc)-1)];state["step"]+=1
        m=r["choices"][0]["message"]
        d={"role":"assistant"}
        if m.get("tool_calls"):
            tcs=[dict(c,index=i) for i,c in enumerate(m["tool_calls"])]
            d["tool_calls"]=tcs
        else:
            d["content"]=m["content"]
        chunks=[{"choices":[{"index":0,"delta":d,"finish_reason":None}]},{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":r["usage"]}]
        out=("".join("data: "+json.dumps(c)+"\n\n" for c in chunks)+"data: [DONE]\n\n").encode()
        self.send_response(200);self.send_header("content-type","text/event-stream");self.send_header("content-length",str(len(out)));self.end_headers();self.wfile.write(out)
HTTPServer(("127.0.0.1",int(sys.argv[1])),H).serve_forever()
