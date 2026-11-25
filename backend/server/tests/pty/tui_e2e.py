"""Drives the real `jani` in a pseudo-terminal to check the consent prompts. Not part of
`cargo test` (needs python3 and a built binary): run `cargo build`, then
`python3 backend/server/tests/pty/tui_e2e.py`. JANI_BIN overrides the binary."""
import re
import os as _os
JANI = _os.environ.get("JANI_BIN") or _os.path.join(_os.path.dirname(_os.path.abspath(__file__)), "../../../../target/debug/jani")
import os, pty, select, subprocess, sys, tempfile, threading, time, json, fcntl, termios, struct, re
from http.server import BaseHTTPRequestHandler, HTTPServer

replies = [
    ("tool", ["touch", "made.txt"]),
    ("text", "made it"),
    ("tool", ["touch", "other.txt"]),
    ("text", "understood, not made"),
    ("tool", ["touch", "third.txt"]),
    ("ask", ("Which database should the cache use?", ["sqlite", "postgres"])),
    ("text", "going with your pick"),
    ("ask", ("What should the table be called?", [])),
    ("text", "using your words"),
]
seen = []
n = [0]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        seen.append(self.rfile.read(int(self.headers.get("content-length", 0))).decode())
        kind, val = replies[n[0]]; n[0] += 1
        self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
        def send(o): self.wfile.write(f"data: {json.dumps(o)}\n\n".encode())
        if kind in ("tool", "ask"):
            name, args = ("command", {"argv": val, "timeout_secs": 20}) if kind == "tool" else ("ask", {"question": val[0], "options": val[1]})
            send({"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":name,"arguments":json.dumps(args)}}]}}]})
            send({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
        else:
            send({"choices":[{"index":0,"delta":{"role":"assistant","content":val}}]})
            send({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        send({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}})
        self.wfile.write(b"data: [DONE]\n\n")
srv = HTTPServer(("127.0.0.1", 0), H); threading.Thread(target=srv.serve_forever, daemon=True).start()
base = f"http://127.0.0.1:{srv.server_port}/v1"
d = tempfile.mkdtemp(prefix="jani-tui-"); os.makedirs(d + "/repo")
env = dict(os.environ, JANI_API_KEY="k", JANI_API_BASE=base, JANI_MODEL="scripted", TERM="xterm-256color")
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 130, 0, 0))
p = subprocess.Popen([JANI, "repl", "--exec", "smart", "--repo", d + "/repo", "--state-dir", d + "/state"],
                     stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True)
buf = b""
def pump(t=0.3):
    global buf
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([master], [], [], 0.05)
        if r:
            try: data = os.read(master, 65536)
            except OSError: return
            buf += data
            if b'\x1b[6n' in data: os.write(master, b'\x1b[1;1R')  # answer the cursor-position query
def wait_for(text, timeout=20):
    global buf
    end = time.time() + timeout
    while time.time() < end:
        pump(0.2)
        if text.encode() in buf: return True
    return False
def send(s): os.write(master, s.encode())
def clear(): 
    global buf; buf = b""

ok = True
def check(name, cond):
    global ok; ok &= bool(cond); print(("PASS " if cond else "FAIL ") + name)

pump(1.5)
send("make a file"); time.sleep(0.2); send("\r")
check("question shown", wait_for("needs your OK"))
check("shows the command", b"touch made.txt" in buf)
check("file not made while waiting", not os.path.exists(d + "/repo/made.txt"))
send("x"); pump(0.5)  # a stray key must not answer
send("y"); pump(0.5)  # a plain y no longer approves: approval is Alt+Y
check("plain y does not approve", not os.path.exists(d + "/repo/made.txt"))
check("stray key leaves it open", not os.path.exists(d + "/repo/made.txt"))
clear(); send("\x1by")  # Alt+Y
check("answer once -> agent continues", wait_for("made it"))
check("file made after allow once", os.path.exists(d + "/repo/made.txt"))
check("note recorded", b"allowed once" in buf)
clear(); time.sleep(0.5)
send("make another"); time.sleep(0.2); send("\r")
check("second question shown", wait_for("needs your OK"))
clear(); send("n")
check("deny -> agent told", wait_for("understood, not made"))
check("file not made after deny", not os.path.exists(d + "/repo/other.txt"))
clear(); time.sleep(0.5)
send("one more"); time.sleep(0.2); send("\r")
check("third question shown", wait_for("needs your OK"))
clear(); send("\x03")  # Ctrl+C while the agent waits
check("ctrl+c closes the question and the turn ends", wait_for("cancel", 15) or wait_for("jani:", 5))
pump(1)
check("cancelled command not run", not os.path.exists(d + "/repo/third.txt"))
# --- the agent asks a question instead of guessing
clear(); time.sleep(0.5)
send("pick a store"); time.sleep(0.2); send("\r")
check("question shown", wait_for("would rather ask than guess"))
check("question lists the options", b"sqlite" in buf and b"postgres" in buf)
clear(); send("2")
check("a digit answers with that option", wait_for("going with your pick"))
check("model received the chosen option", bool(re.search(r'answer\\+"\s*:\s*\\+"postgres', seen[-1])))
clear(); time.sleep(0.5)
send("name the table"); time.sleep(0.2); send("\r")
check("second question shown", wait_for("would rather ask than guess"))
send("events_v2"); time.sleep(0.3); send("\r")
check("typed words answer it", wait_for("using your words"))
check("model received the typed words", "events_v2" in seen[-1])
send("/exit\r"); pump(1)
try: p.wait(timeout=5)
except Exception: p.kill()
journal = open(d + "/state/journal.jsonl").read()
check("journal has once", "exec_operator_once" in journal)
check("journal has declined", "exec_operator_declined" in journal)
print("ALL OK" if ok else "SOME FAILED")
