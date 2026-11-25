"""Drives the real `jani` in a pseudo-terminal to check the consent prompts. Not part of
`cargo test` (needs python3 and a built binary): run `cargo build`, then
`python3 backend/server/tests/pty/line_e2e.py`. JANI_BIN overrides the binary."""
import os as _os
JANI = _os.environ.get("JANI_BIN") or _os.path.join(_os.path.dirname(_os.path.abspath(__file__)), "../../../../target/debug/jani")

import select
import os, pty, select, subprocess, tempfile, threading, time, json, fcntl, termios, struct
from http.server import BaseHTTPRequestHandler, HTTPServer

class Provider:
    def __init__(self, replies):
        self.replies, self.n = replies, 0
        outer = self
        class H(BaseHTTPRequestHandler):
            def log_message(self, *a): pass
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
                kind, val = outer.replies[outer.n]; outer.n += 1
                msg = {"role": "assistant", "content": None}
                if kind in ("tool", "ask"):
                    name, args = ("command", {"argv": val, "timeout_secs": 20}) if kind == "tool" else ("ask", {"question": val[0], "options": val[1]})
                    msg["tool_calls"] = [{"id": "c1", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]
                else:
                    msg["content"] = val
                usage = {"prompt_tokens": 5, "completion_tokens": 2}
                if body.get("stream"):
                    self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
                    w = lambda o: self.wfile.write(f"data: {json.dumps(o)}\n\n".encode())
                    if kind in ("tool", "ask"):
                        w({"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0, **msg["tool_calls"][0]}]}}]})
                    else:
                        w({"choices":[{"index":0,"delta":{"role":"assistant","content":val}}]})
                    w({"choices":[],"usage":usage}); self.wfile.write(b"data: [DONE]\n\n")
                else:
                    out = json.dumps({"choices":[{"message":msg}],"usage":usage}).encode()
                    self.send_response(200); self.send_header("content-type","application/json"); self.send_header("content-length",str(len(out))); self.end_headers(); self.wfile.write(out)
        self.srv = HTTPServer(("127.0.0.1", 0), H); threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.srv.server_port}/v1"

ok = True
def check(name, cond):
    global ok; ok &= bool(cond); print(("PASS " if cond else "FAIL ") + name)

def run(args, replies, script, tty=True):
    """Start jani with stdin/stderr on a pty (or pipes), stdout on a pipe. `script` drives the pty."""
    prov = Provider(replies)
    d = tempfile.mkdtemp(prefix="jani-line-"); os.makedirs(d + "/repo")
    env = dict(os.environ, JANI_API_KEY="k", JANI_API_BASE=prov.base, JANI_MODEL="scripted", TERM="dumb")
    full = [JANI] + args + ["--exec", "smart", "--repo", d + "/repo", "--state-dir", d + "/state"]
    if tty:
        master, slave = pty.openpty()
        p = subprocess.Popen(full, stdin=slave, stderr=slave, stdout=subprocess.PIPE, env=env, close_fds=True)
    else:
        master = None
        p = subprocess.Popen(full, stdin=subprocess.PIPE, stderr=subprocess.PIPE, stdout=subprocess.PIPE, env=env)
    buf = [b""]
    def pump(t=0.3):
        end = time.time() + t
        while time.time() < end and master is not None:
            r, _, _ = select.select([master], [], [], 0.05)
            if r:
                try: buf[0] += os.read(master, 65536)
                except OSError: return
    def wait_for(text, timeout=15):
        end = time.time() + timeout
        while time.time() < end:
            pump(0.2)
            if text.encode() in buf[0]: return True
        return False
    ctx = dict(d=d, p=p, master=master, wait_for=wait_for, pump=pump, buf=buf, prov=prov)
    script(ctx)
    try: out, _ = p.communicate(timeout=10) if tty else (b"", None)
    except Exception: p.kill(); out = b""
    return ctx, out.decode(errors="replace")

# --- --print: answer y
def s_print_yes(c):
    check("--print asks on the terminal", c["wait_for"]("needs your OK"))
    check("--print shows the command", b"touch made.txt" in c["buf"][0])
    check("--print waits: file not made yet", not os.path.exists(c["d"] + "/repo/made.txt"))
    os.write(c["master"], b"y\n"); c["pump"](1.0)
ctx, out = run(["--print", "make a file"], [("tool", ["touch", "made.txt"]), ("text", "made it")], s_print_yes)
check("--print y: command ran", os.path.exists(ctx["d"] + "/repo/made.txt"))
check("--print y: answer on stdout, not mixed with the prompt", "made it" in out and "needs your OK" not in out)

# --- --print: Enter (default) denies
def s_print_default(c):
    check("--print asks again", c["wait_for"]("needs your OK"))
    os.write(c["master"], b"\n"); c["pump"](1.0)
ctx, out = run(["--print", "make a file"], [("tool", ["touch", "made.txt"]), ("text", "declined, carrying on")], s_print_default)
check("--print Enter: denied by default", not os.path.exists(ctx["d"] + "/repo/made.txt"))
check("--print Enter: model told, finishes", "declined, carrying on" in out)
check("--print journal: declined", "exec_operator_declined" in open(ctx["d"] + "/state/journal.jsonl").read())

# --- repl line mode (stdout is a pipe): session answer covers the same prefix again
def s_repl(c):
    c["pump"](1.0)
    os.write(c["master"], b"make a file\n")
    check("repl asks", c["wait_for"]("needs your OK"))
    c["buf"][0] = b""
    os.write(c["master"], b"s\n")
    time.sleep(1.5); c["pump"](0.5)
    os.write(c["master"], b"and again\n")
    time.sleep(2.0); c["pump"](0.5)
    check("repl: no second question for a session-allowed prefix", c["buf"][0].count(b"needs your OK") == 0)
    os.write(c["master"], b"/exit\n"); c["pump"](0.5)
ctx, out = run(["repl"], [("tool", ["touch", "made.txt"]), ("text", "first done"), ("tool", ["touch", "made.txt"]), ("text", "second done")], s_repl)
check("repl: both turns finished", "first done" in out and "second done" in out)
check("repl: journal has the session grant", "exec_session_grant" in open(ctx["d"] + "/state/journal.jsonl").read())

# --- --print asks a question on the terminal: a number picks an option
def s_question(c):
    check("--print puts the question", c["wait_for"]("the agent has a question"))
    check("--print lists options", b"[1] sqlite" in c["buf"][0] and b"[2] postgres" in c["buf"][0])
    os.write(c["master"], b"2\n"); c["pump"](1.0)
ctx, out = run(["--print", "add a cache"], [("ask", ("Which database should the cache use?", ["sqlite", "postgres"])), ("text", "going with postgres")], s_question)
check("--print question: the answer reached the model", "going with postgres" in out)
check("--print question: journal has agent.ask", "agent.ask" in open(ctx["d"] + "/state/journal.jsonl").read())

# --- Enter leaves it for later: the turn ends on the question
def s_later(c):
    check("--print asks again", c["wait_for"]("the agent has a question"))
    os.write(c["master"], b"\n"); c["pump"](1.0)
ctx, out = run(["--print", "add a cache"], [("ask", ("Which database should the cache use?", ["sqlite", "postgres"])), ("text", "never reached")], s_later)
check("Enter leaves it unanswered: the turn ends on the question", "I need your input before I continue" in out and "never reached" not in out)

# --- piped stdin (CI/scripts): not asked, refused with instructions
prov = Provider([("tool", ["touch", "made.txt"]), ("text", "reported")])
d = tempfile.mkdtemp(prefix="jani-pipe-"); os.makedirs(d + "/repo")
env = dict(os.environ, JANI_API_KEY="k", JANI_API_BASE=prov.base, JANI_MODEL="scripted")
r = subprocess.run([JANI, "--print", "make a file", "--exec", "smart", "--repo", d + "/repo", "--state-dir", d + "/state"],
                   input=b"y\n", capture_output=True, env=env, timeout=30)
check("piped stdin: not run (a line on stdin is not an answer)", not os.path.exists(d + "/repo/made.txt"))
check("piped stdin: refused for lack of an operator", "exec_consent_required" in open(d + "/state/journal.jsonl").read())
print("ALL OK" if ok else "SOME FAILED")
