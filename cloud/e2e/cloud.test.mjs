// End to end: real ifinsta accounts, the jani-cloud gateway, and real `jani ui`
// processes. Signs people in through the actual accounts login.
import { test, before, after, describe } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join } from "node:path";
import { startStack, USERS, PASSWORD, keyOf } from "./harness.mjs";
import { writeFileSync } from "node:fs";

const { chromium } = createRequire(import.meta.url)("playwright");
let stack, browser;

async function accountsLogin(page, email) {
  await page.waitForURL((u) => u.toString().startsWith(`${stack.ACCOUNTS}/login`), { timeout: 15000 });
  await page.fill("#email", email);
  await page.fill("#password", PASSWORD);
  await page.click("button[type=submit]");
}

// Opens the workspace by the plain redirect flow, which needs no JavaScript.
async function signInByRedirect(context, email) {
  const page = await context.newPage();
  await page.goto(`${stack.APP}/siwi/login?return_to=/`);
  await accountsLogin(page, email);
  await page.waitForFunction(() => document.querySelector("#acctWho")?.textContent.includes("@"), null, { timeout: 30000 });
  return page;
}

// Live workspace processes only: not the bwrap wrappers around them, and not
// zombies that a slow init has not reaped yet.
const janiPids = () => {
  try {
    return execFileSync("ps", ["-C", "jani", "-o", "pid=,stat="], { encoding: "utf8" })
      .split("\n").map((l) => l.trim().split(/\s+/)).filter(([pid, stat]) => pid && stat && !stat.startsWith("Z")).map(([pid]) => Number(pid));
  } catch {
    return [];
  }
};
const uidOf = (pid) => Number(execFileSync("ps", ["-o", "uid=", "-p", String(pid)], { encoding: "utf8" }).trim());
const usersDir = () => join(stack.data, "users");

describe("jani-cloud with ifinsta accounts", () => {
  before(async () => {
    stack = await startStack();
    browser = await chromium.launch();
  });
  after(async () => {
    await browser?.close();
    await stack?.stop();
  });

  test("signed out: the sign-in page, a locked API, and no workspace is started", async () => {
    const page = await (await browser.newContext()).newPage();
    const res = await page.goto(stack.APP + "/");
    assert.equal(res.status(), 200);
    const csp = res.headers()["content-security-policy"];
    assert.match(csp, /frame-ancestors 'none'/);
    assert.match(csp, new RegExp(`frame-src 'self' ${stack.ACCOUNTS}`), "may frame accounts, nothing else");
    assert.ok(await page.locator("[data-testid=sign-in]").count(), "sign-in button present");
    const api = await fetch(stack.APP + "/api/overview");
    assert.equal(api.status, 401);
    assert.equal((await api.json()).error, "login_required");
    const stop = await fetch(stack.APP + "/cloud/stop", { method: "POST", headers: { origin: stack.APP } });
    assert.equal(stop.status, 401);
    assert.equal((await fetch(stack.APP + "/etc/passwd", { redirect: "manual" })).status, 303);
    assert.equal((await fetch(stack.APP + "/health")).status, 200);
    assert.ok(!existsSync(usersDir()) || readdirSync(usersDir()).length === 0, "no workspace for an anonymous visitor");
  });

  test("redirect sign-in lands in the person's own workspace, with no token in the browser", async () => {
    const context = await browser.newContext();
    const page = await signInByRedirect(context, USERS.alice);
    assert.equal(new URL(page.url()).pathname, "/");
    await page.waitForFunction(() => !document.querySelector("#acct").hidden, null, { timeout: 10000 });
    assert.equal(await page.textContent("#acctWho"), USERS.alice);
    const html = await page.content();
    const state = await page.evaluate(() => JSON.stringify({ hash: location.hash, ls: { ...localStorage }, ss: { ...sessionStorage } }));
    assert.ok(!/token=[0-9a-f]{32}/.test(html + state), "the workspace token is never in the page or storage");
    const cookies = await context.cookies(stack.APP);
    const session = cookies.find((c) => c.name.endsWith("siwi_session"));
    assert.ok(session.httpOnly && session.sameSite === "Lax", "session cookie is HttpOnly and SameSite=Lax");
    // Same-origin API works through the proxy; the workspace is read from the real state.
    const overview = await page.evaluate(async () => (await fetch("/api/overview", { credentials: "same-origin" })).json());
    assert.match(overview.principal, /^ifinsta-[0-9a-f]{24}$/, "journal attribution is the hashed ifinsta subject, not an email");
    assert.ok(!JSON.stringify(overview).includes(USERS.alice));
    assert.equal(overview.proxied, true);
    assert.equal(await page.textContent("#wt"), "your workspace", "the host path is not shown to the person");
    assert.equal(overview.authority.allowWrite, true);
    assert.equal(overview.authority.allowExec, false);
    await context.close();
  });

  test("the sign-in button works from the page, without a manual reload", async () => {
    const context = await browser.newContext();
    const page = await context.newPage();
    await page.goto(stack.APP + "/");
    const popupP = context.waitForEvent("page", { timeout: 45000 });
    await page.locator("[data-testid=sign-in]").locator("button").click();
    // The accounts sign-in opens in the modal, then escalates to a popup for the password.
    const escalate = page.frameLocator("[data-siwi-modal] iframe").locator("[data-escalate=password]");
    await escalate.waitFor({ state: "visible", timeout: 20000 });
    await escalate.click();
    const popup = await popupP;
    await accountsLogin(popup, USERS.alice);
    await page.waitForFunction(() => document.querySelector("#acctWho")?.textContent.includes("@"), null, { timeout: 30000 });
    assert.equal(new URL(page.url()).pathname, "/");
    await context.close();
  });

  test("a task runs in the person's workspace and is journaled under their hashed identity", async () => {
    const context = await browser.newContext();
    const page = await signInByRedirect(context, USERS.alice);
    await page.click("[data-v=term]");
    await page.click("#tIn");
    await page.keyboard.type("Read the README and write notes");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector(".k-jani"), null, { timeout: 30000 });
    assert.match(await page.locator(".k-jani .t-txt").last().textContent(), /notes/);
    const dirs = readdirSync(usersDir());
    const alice = dirs.find((d) => existsSync(join(usersDir(), d, "repo/out/notes.txt")));
    assert.ok(alice, "the write landed in alice's own worktree");
    const journal = readFileSync(join(usersDir(), alice, "state/journal.jsonl"), "utf8").trim().split("\n").map(JSON.parse);
    assert.ok(journal.length >= 5);
    assert.ok(journal.every((e) => e.actor_id === `ifinsta-${alice}`), "every event is attributed to her hashed identity");
    await page.click("[data-v=graph]");
    await page.waitForSelector(".node[data-id]");
    assert.ok((await page.locator(".node").count()) >= 5);
    await context.close();
  });

  test("two people get separate workspaces, state and Unix users", async () => {
    const ac = await browser.newContext();
    const bc = await browser.newContext();
    const a = await signInByRedirect(ac, USERS.alice);
    const b = await signInByRedirect(bc, USERS.bob);
    const runsA = await a.evaluate(async () => (await fetch("/api/runs")).json());
    const runsB = await b.evaluate(async () => (await fetch("/api/runs")).json());
    assert.ok(runsA.length >= 1, "alice sees her own run");
    assert.equal(runsB.length, 0, "bob sees none of alice's runs");
    const [pa, pb] = [await a.evaluate(async () => (await fetch("/api/overview")).json()), await b.evaluate(async () => (await fetch("/api/overview")).json())];
    assert.notEqual(pa.principal, pb.principal);
    if (stack.sandbox === "off") assert.notEqual(pa.worktree, pb.worktree);
    else assert.equal(pa.worktree, "/work/repo", "inside the sandbox each person sees only their own /work");
    const dirs = readdirSync(usersDir());
    assert.equal(dirs.length, 2);
    if (stack.uidIsolation) {
      const uids = janiPids().map(uidOf);
      assert.equal(new Set(uids).size, uids.length, "each workspace runs as its own Unix user");
      assert.ok(uids.every((u) => u >= 41000), "none runs as root");
      const owners = dirs.map((d) => statSync(join(usersDir(), d)).uid);
      assert.equal(new Set(owners).size, 2);
      for (const d of dirs) assert.equal(statSync(join(usersDir(), d)).mode & 0o777, 0o700, "private directory");
    }
    if (stack.sandbox === "bwrap") {
      // From outside, look at what each workspace can see as its root.
      const roots = janiPids().map((pid) => `/proc/${pid}/root`);
      for (const root of roots) {
        const top = readdirSync(root);
        for (const hidden of ["home", "root", "data", "var", "opt-host"]) assert.ok(!top.includes(hidden) || hidden === "var", `sandbox root exposes /${hidden}`);
        assert.ok(!top.includes("home"), "the host's /home is not visible");
        assert.deepEqual(readdirSync(join(root, "work")).sort(), ["home", "repo", "state", "ui.sock"], "only its own directory is mounted");
        assert.ok(!existsSync(join(root, stack.data)), "the gateway's data directory is not visible");
      }
      assert.equal(roots.length, 2);
    }
    await ac.close();
    await bc.close();
  });

  test("the gateway enforces same-origin and forwards only the workspace's own routes", async () => {
    const context = await browser.newContext();
    const page = await signInByRedirect(context, USERS.alice);
    const cookie = (await context.cookies(stack.APP)).map((c) => `${c.name}=${c.value}`).join("; ");
    const send = (extra) => fetch(stack.APP + "/api/session/send", { method: "POST", headers: { cookie, "content-type": "application/json", ...extra }, body: JSON.stringify({ text: "/help" }) });
    assert.equal((await send({})).status, 403, "no Origin");
    assert.equal((await send({ origin: "http://attacker.example" })).status, 403, "foreign Origin");
    assert.equal((await send({ origin: stack.APP, "sec-fetch-site": "cross-site" })).status, 403, "cross-site fetch");
    assert.equal((await send({ origin: stack.APP })).status, 202, "same origin is accepted");
    const get = (p) => fetch(stack.APP + p, { headers: { cookie }, redirect: "manual" });
    assert.equal((await get("/api/overview")).status, 200);
    assert.equal((await get("/etc/passwd")).status, 404, "unlisted paths are never forwarded");
    // A page the browser loads must not be able to read or set the workspace token.
    const leaked = await page.evaluate(async () => {
      const r = await fetch("/api/overview", { headers: { "X-Jani-Token": "bogus" } });
      return r.status;
    });
    assert.equal(leaked, 200, "the gateway's token replaces whatever the browser sends");
    await context.close();
  });

  test("sign out stops the workspace, ends the session, and the old cookie is dead", async () => {
    const context = await browser.newContext();
    const page = await signInByRedirect(context, USERS.bob);
    const cookie = (await context.cookies(stack.APP)).map((c) => `${c.name}=${c.value}`).join("; ");
    const before = janiPids().length;
    await page.click("#signout");
    await page.waitForFunction(() => document.querySelector("[data-testid=sign-in]"), null, { timeout: 20000 });
    const after = janiPids().length;
    assert.ok(after < before, `a workspace process was stopped (${before} → ${after})`);
    const replay = await fetch(stack.APP + "/api/overview", { headers: { cookie } });
    assert.equal(replay.status, 401, "the replayed session cookie no longer works");
    // Files and journal stay: signing back in restores the same workspace.
    // Signing out here is local: the ifinsta session survives, so coming back is
    // one tap with no password prompt, and the same workspace returns.
    const again = await context.newPage();
    await again.goto(`${stack.APP}/siwi/login?return_to=/`);
    await again.waitForFunction(() => document.querySelector("#acctWho")?.textContent.includes("@"), null, { timeout: 30000 });
    assert.equal(await again.locator("#acctWho").textContent(), USERS.bob);
    assert.ok(!again.url().startsWith(stack.ACCOUNTS), "no credential prompt on return");
    await context.close();
  });

  test("sign out everywhere ends the ifinsta session too", async () => {
    const context = await browser.newContext();
    const page = await signInByRedirect(context, USERS.alice);
    await page.click("#signoutAll");
    await page.waitForURL((u) => u.toString().startsWith(`${stack.ACCOUNTS}/oidc/logout`), { timeout: 20000 });
    await page.click("button:has-text('Sign out')");
    await page.waitForURL(`${stack.APP}/`, { timeout: 20000 });
    await page.waitForSelector("[data-testid=sign-in]", { timeout: 10000 });
    // With the ifinsta session gone, signing in again asks for credentials.
    await page.goto(`${stack.APP}/siwi/login?return_to=/`);
    await page.waitForURL((u) => u.toString().startsWith(`${stack.ACCOUNTS}/login`), { timeout: 15000 });
    await context.close();
  });

  test("the operator cannot be tricked into a missing-isolation start", async () => {
    // Unit-level refusal is covered in cargo tests; here, check the running process obeyed it.
    assert.match(stack.logs.gateway, /workspace isolation/);
    assert.ok(!/panicked/.test(stack.logs.gateway), stack.logs.gateway.slice(-600));
  });
});

describe("operating limits", () => {
  test("idle workspaces are stopped and come back on the next request", async () => {
    const own = await startStack({ gatewayEnv: { JANI_CLOUD_IDLE_SECONDS: "4" } });
    const b = await chromium.launch();
    try {
      stack = own;
      const context = await b.newContext();
      const page = await signInByRedirect(context, USERS.alice);
      const running = () => janiPids().length;
      assert.equal(running(), 1);
      // A visible tab polls and so counts as activity. Close it, then let the workspace go idle.
      await page.close();
      await new Promise((r) => setTimeout(r, 9000));
      assert.equal(running(), 0, "the idle workspace was stopped");
      // The session is still valid, so the next request starts a fresh one over the same files.
      const again = await context.newPage();
      const status = (await again.goto(stack.APP + "/")).status();
      assert.equal(status, 200);
      await again.waitForFunction(() => document.querySelector("#acctWho")?.textContent.includes("@"), null, { timeout: 20000 });
      assert.equal(running(), 1, "restarted on demand");
    } finally {
      await b.close();
      await own.stop();
    }
  });

  test("when every workspace slot is taken, the next person gets a clear 503, not a hang", async () => {
    const own = await startStack({ gatewayEnv: { JANI_CLOUD_MAX_ACTIVE: "1" } });
    const b = await chromium.launch();
    try {
      stack = own;
      const a = await signInByRedirect(await b.newContext(), USERS.alice);
      assert.equal(await a.evaluate(async () => (await fetch("/api/overview")).status), 200);
      const bobContext = await b.newContext();
      const bobPage = await bobContext.newPage();
      await bobPage.goto(`${own.APP}/siwi/login?return_to=/`);
      await accountsLogin(bobPage, USERS.bob);
      await bobPage.waitForLoadState("networkidle");
      const body = JSON.parse(await bobPage.textContent("body"));
      assert.equal(body.error, "workspace_unavailable");
      assert.match(body.message, /busy/i);
      assert.ok(!JSON.stringify(body).includes("/"), "no internal detail is exposed");
    } finally {
      await b.close();
      await own.stop();
    }
  });

  for (const [signal, label] of [["SIGTERM", "is stopped"], ["SIGKILL", "is killed"]]) {
    // Under docker/gateway.sh the process the harness signals is the `docker run`
    // client, and killing that leaves the container running. The container's own
    // answer (tini exits with the gateway) is checked by hand; see the README.
    const viaDocker = signal === "SIGKILL" && process.env.GATEWAY_IMAGE && "SIGKILL reaches only the docker client here";
    test(`no workspace outlives the gateway when it ${label}`, { skip: viaDocker }, async () => {
      const own = await startStack();
      const b = await chromium.launch();
      try {
        stack = own;
        await signInByRedirect(await b.newContext(), USERS.alice);
        const running = () => janiPids().length;
        assert.equal(running(), 1);
        await new Promise((ok) => { own.gateway.once("exit", ok); own.gateway.kill(signal); });
        await new Promise((r) => setTimeout(r, 1500));
        assert.equal(running(), 0, "the workspace process is gone");
      } finally {
        await b.close();
        await own.stop();
      }
    });
  }
});

// ---- network isolation and resource limits -------------------------------

const hasBwrap = (() => { try { execFileSync("which", ["bwrap"]); return true; } catch { return false; } })();
const hasCgroup = existsSync("/sys/fs/cgroup/memory") || existsSync("/sys/fs/cgroup/cgroup.controllers");

async function script(own, turns) {
  const r = await fetch(`http://127.0.0.1:${own.providerPort}/__script`, { method: "POST", body: JSON.stringify({ turns }) });
  assert.equal(r.status, 204);
}

// Send one turn from the page and wait for it to finish; return the transcript.
async function turn(page, text, { timeout = 90000 } = {}) {
  const before = await page.evaluate(async () => {
    const r = await fetch("/api/session?after=-1");
    if (!r.ok) throw new Error("session unavailable: " + r.status + " " + (await r.text()).slice(0, 120));
    return (await r.json()).entries.length;
  });
  const sent = await page.evaluate(async (t) => (await fetch("/api/session/send", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ text: t }) })).status, text);
  assert.equal(sent, 202);
  const until = Date.now() + timeout;
  for (;;) {
    const s = await page.evaluate(async () => (await fetch("/api/session?after=-1")).json());
    if (!s.busy && s.entries.length > before) return s.entries.slice(before);
    if (Date.now() > until) throw new Error("turn did not finish");
    await new Promise((r) => setTimeout(r, 300));
  }
}

// A probe is a pytest file the scripted model writes and runs inside the workspace.
// Jani's tool `ok` only means "the command ran", so the result is read from the
// JUnit report the probe leaves in the workspace, never from that flag.
const probe = (name, code) => [
  { tool: "write", args: { path: `out/${name}_test.py`, content: code } },
  { tool: "command", args: { argv: ["python3", "-m", "pytest", "-q", "-p", "no:cacheprovider", `--junitxml=out/${name}.xml`, `out/${name}_test.py`], timeout_secs: 90 } },
  { final: "done" },
];

describe("network isolation and resource limits", { skip: !hasBwrap && "needs bubblewrap" }, () => {
  let own, browserB, page;
  before(async () => {
    own = await startStack({
      gatewayEnv: {
        JANI_CLOUD_SANDBOX: "bwrap",
        JANI_CLOUD_QUOTAS: "auto",
        JANI_CLOUD_JANI_ARGS: "--allow-write --write-path out --allow-exec --exec-program python3 --api-base http://127.0.0.1:{PROVIDER}/v1 --model fake-1",
        JANI_CLOUD_LIMIT_MEMORY_MB: "256",
        JANI_CLOUD_LIMIT_PIDS: "128",
        JANI_CLOUD_LIMIT_OPEN_FILES: "128",
        JANI_CLOUD_LIMIT_DISK_MB: "20",
        JANI_CLOUD_LIMIT_CPUS: "0.5",
        JANI_CLOUD_DISK_CHECK_SECONDS: "1",
      },
    });
    stack = own;
    browserB = await chromium.launch();
    page = await signInByRedirect(await browserB.newContext(), USERS.alice);
  });
  after(async () => {
    await browserB?.close();
    await own?.stop();
  });

  const outDir = () => join(own.data, "users", readdirSync(join(own.data, "users"))[0], "repo/out");
  // What the probe really did: counts from its own JUnit report, or null if it never finished.
  const report = (name) => {
    const f = join(outDir(), `${name}.xml`);
    if (!existsSync(f)) return null;
    const attr = (k) => Number((readFileSync(f, "utf8").match(new RegExp(`${k}="(\\d+)"`)) || [])[1]);
    return { tests: attr("tests"), failures: attr("failures"), errors: attr("errors"), skipped: attr("skipped") };
  };
  const detail = (name) => { try { return [...readFileSync(join(outDir(), `${name}.xml`), "utf8").matchAll(/<failure message="([^"]*)"/g)].map((m) => m[1].slice(0, 300)).join(" | "); } catch { return "no report"; } };
  const passed = (name, tests) => assert.deepEqual(report(name), { tests, failures: 0, errors: 0, skipped: 0 }, `${name}: every check ran and passed. ${detail(name)}`);

  test("the workspace has its own empty network namespace, and the host's is out of reach", async () => {
    const pid = janiPids()[0];
    const mine = (await import("node:fs")).readlinkSync(`/proc/${own.gateway.pid}/ns/net`);
    const theirs = (await import("node:fs")).readlinkSync(`/proc/${pid}/ns/net`);
    assert.notEqual(mine, theirs, "a different network namespace from the gateway's");
    await script(own, [probe("net", `
import os, socket, pytest
def test_only_loopback_exists():
    # /proc/net/dev is per network namespace: this is the workspace's own, not the host's.
    names = [l.split(":")[0].strip() for l in open("/proc/net/dev").read().splitlines()[2:]]
    assert names == ["lo"], names
def test_the_hosts_loopback_ports_are_unreachable():
    for port in (${own.appPort}, ${own.providerPort}):
        with pytest.raises(OSError):
            socket.create_connection(("127.0.0.1", port), timeout=2)
def test_the_internet_is_unreachable_directly():
    with pytest.raises(OSError):
        socket.create_connection(("1.1.1.1", 80), timeout=2)
`)]);
    await turn(page, "check the network");
    passed("net", 3);
  });

  test("the only way out is the allowlist proxy: the model provider, nothing else", async () => {
    await script(own, [probe("egress", `
import http.client, urllib.error, urllib.request, pytest
def test_the_model_provider_is_reachable_through_the_proxy():
    try:
        urllib.request.urlopen("http://127.0.0.1:${own.providerPort}/", timeout=5)
    except urllib.error.HTTPError as e:
        assert e.code in (404, 501), e.code      # the provider answered; the proxy let it through
def test_another_host_is_refused_for_plain_http():
    with pytest.raises(urllib.error.HTTPError) as e:
        urllib.request.urlopen("http://example.test/", timeout=5)
    assert e.value.code == 403 and b"egress denied" in e.value.read()
def test_another_host_is_refused_for_https_tunnels():
    with pytest.raises(OSError, match="403"):      # the proxy refuses the CONNECT
        urllib.request.urlopen("https://example.test/", timeout=5)
def test_localhost_by_name_is_not_the_allowed_literal():
    with pytest.raises(urllib.error.HTTPError) as e:
        urllib.request.urlopen("http://localhost:${own.appPort}/", timeout=5)
    assert e.value.code == 403
def test_the_gateway_port_is_not_allowed_even_as_a_literal():
    with pytest.raises(urllib.error.HTTPError) as e:
        urllib.request.urlopen("http://127.0.0.1:${own.appPort}/", timeout=5)
    assert e.value.code == 403
`)]);
    await turn(page, "check egress");
    passed("egress", 5);
    assert.match(own.logs.gateway, /egress denied/, "denials are logged with the workspace");
  });

  test("a workspace over its memory limit is killed and the rest of the host is untouched", async () => {
    await script(own, [probe("mem", `
def test_allocate_far_more_than_the_limit():
    open("out/mem_started", "w").close()
    blocks = []
    for _ in range(80):
        blocks.append(bytearray(b"x") * (10 * 1024 * 1024))   # really written: 800 MB against a 256 MB limit
`)]);
    await turn(page, "use lots of memory");
    assert.ok(existsSync(join(outDir(), "mem_started")), "the probe started");
    assert.equal(report("mem"), null, "the probe was killed before it could finish, so it left no report");
    const ok = await page.evaluate(async () => (await fetch("/api/overview")).status);
    assert.equal(ok, 200, "the workspace itself survived its child being killed");
  });

  test("process, file-descriptor and file-size limits hold", async () => {
    await script(own, [probe("limits", `
import os, subprocess, pytest
def test_cannot_start_unbounded_processes():
    kids = []
    try:
        with pytest.raises(OSError):
            for _ in range(2000):
                kids.append(subprocess.Popen(["sleep", "30"]))
        assert len(kids) < 2000
    finally:
        for k in kids:
            k.kill()
def test_open_file_limit():
    fds = []
    try:
        with pytest.raises(OSError):
            for _ in range(1000):
                fds.append(os.open("/dev/null", os.O_RDONLY))
    finally:
        for fd in fds:
            os.close(fd)
def test_a_single_file_cannot_exceed_the_disk_limit():
    # A sparse file: it hits the file-size limit without using any disk.
    fd = os.open("big.bin", os.O_WRONLY | os.O_CREAT)
    try:
        os.lseek(fd, 25 * 1024 * 1024, os.SEEK_SET)
        with pytest.raises(OSError):
            os.write(fd, b"x")
    finally:
        os.close(fd)
        os.remove("big.bin")
`)]);
    await turn(page, "hit the other limits");
    passed("limits", 3);
  });

  test("CPU is held to the configured share", { skip: !hasCgroup && "needs a writable cgroup" }, async () => {
    await script(own, [probe("cpu", `
import time
def test_busy_loop_gets_about_half_a_cpu():
    wall0, cpu0 = time.time(), time.process_time()
    while time.time() - wall0 < 4:
        pass
    wall, cpu = time.time() - wall0, time.process_time() - cpu0
    assert cpu / wall < 0.8, (cpu, wall)      # limit is 0.5 CPU
`)]);
    await turn(page, "burn cpu");
    passed("cpu", 1);
  });

  test("limits are visible on the host: the workspace sits in its own cgroup", { skip: !hasCgroup && "needs a writable cgroup" }, async () => {
    const pid = janiPids()[0];
    const cg = readFileSync(`/proc/${pid}/cgroup`, "utf8");
    assert.match(cg, /jani-cloud\/[0-9a-f]{24}/, cg);
    const limits = readFileSync(`/proc/${pid}/limits`, "utf8");
    assert.match(limits, /Max open files\s+128\s+128/);
    assert.match(limits, /Max core file size\s+0\s+0/);
    assert.match(limits, /Max file size\s+20971520\s+20971520/);
  });

  test("filling the disk stops the workspace, explains why, and offers a clean start", async () => {
    await script(own, [[
      { tool: "write", args: { path: "out/fill_test.py", content: `
def test_fill():
    for i in range(9):
        with open(f"filler{i}.bin", "wb") as f:
            f.write(b"x" * (4 * 1024 * 1024))
` } },
      { tool: "command", args: { argv: ["python3", "-m", "pytest", "-q", "-p", "no:cacheprovider", "out/fill_test.py"], timeout_secs: 60 } },
      { final: "filled" },
    ]]);
    // The watchdog may stop the workspace mid-turn, so do not wait for the turn to finish.
    const sent = await page.evaluate(async () => (await fetch("/api/session/send", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ text: "fill the disk" }) })).status);
    assert.equal(sent, 202);
    // The watchdog checks every second; the workspace is stopped once it is over 20 MB.
    const cookie = (await page.context().cookies(own.APP)).map((c) => `${c.name}=${c.value}`).join("; ");
    let res;
    for (let i = 0; i < 40; i++) {
      res = await fetch(own.APP + "/", { headers: { cookie } });
      if (res.status === 507) break;
      await new Promise((r) => setTimeout(r, 500));
    }
    assert.equal(res.status, 507, "over-quota workspaces are refused");
    assert.match(await res.text(), /workspace is full/i);
    const api = await fetch(own.APP + "/api/overview", { headers: { cookie } });
    assert.equal(api.status, 507);
    assert.equal((await api.json()).error, "storage_full");
    // The only way back is to delete files: same-origin, and the journal is kept.
    const denied = await fetch(own.APP + "/cloud/reset", { method: "POST", headers: { cookie, "content-type": "application/json", origin: "http://attacker.example", "x-siwi": "1" }, body: "{}" });
    assert.equal(denied.status, 403);
    const reset = await fetch(own.APP + "/cloud/reset", { method: "POST", headers: { cookie, "content-type": "application/json", origin: own.APP, "x-siwi": "1" }, body: "{}" });
    assert.equal(reset.status, 200);
    const again = await fetch(own.APP + "/", { headers: { cookie } });
    assert.equal(again.status, 200, "the workspace starts again");
    const dirs = readdirSync(join(own.data, "users"));
    assert.ok(!existsSync(join(own.data, "users", dirs[0], "repo/filler0.bin")), "the files are gone");
    assert.ok(existsSync(join(own.data, "users", dirs[0], "state/journal.jsonl")), "the audit journal is kept");
  });
});

describe("negative control", { skip: !hasBwrap && "needs bubblewrap" }, () => {
  test("the same probes FAIL when the network is shared, so a pass above means something", async () => {
    const own = await startStack({
      gatewayEnv: {
        JANI_CLOUD_SANDBOX: "bwrap",
        JANI_CLOUD_NET: "shared",
        JANI_CLOUD_JANI_ARGS: "--allow-write --write-path out --allow-exec --exec-program python3 --api-base http://127.0.0.1:{PROVIDER}/v1 --model fake-1",
      },
    });
    const b = await chromium.launch();
    try {
      stack = own;
      const page = await signInByRedirect(await b.newContext(), USERS.alice);
      await script(own, [probe("net", `
import socket, pytest
def test_the_hosts_loopback_ports_are_unreachable():
    with pytest.raises(OSError):
        socket.create_connection(("127.0.0.1", ${own.appPort}), timeout=2)
`)]);
      await turn(page, "check the network");
      const user = readdirSync(join(own.data, "users"))[0];
      const xml = readFileSync(join(own.data, "users", user, "repo/out/net.xml"), "utf8");
      assert.match(xml, /failures="1"/, "with a shared network the gateway's port IS reachable, so the probe fails");
    } finally {
      await b.close();
      await own.stop();
    }
  });
});


// ---- approvals -------------------------------------------------------------

describe("approvals by a designated approver", () => {
  let own, b, alice, bob, aliceSub, bobSub, approversFile;
  const cookieOf = async (page) => (await page.context().cookies(own.APP)).map((c) => `${c.name}=${c.value}`).join("; ");
  const me = (page) => page.evaluate(async () => (await fetch("/cloud/me")).json());
  const aliceDir = () => join(own.data, "users", keyOf(aliceSub));
  const store = () => JSON.parse(readFileSync(join(aliceDir(), "state/approvals.json"), "utf8"));

  before(async () => {
    own = await startStack({
      gatewayEnv: {
        JANI_CLOUD_JANI_ARGS: "--allow-write --write-path out --api-base http://127.0.0.1:{PROVIDER}/v1 --model fake-1",
        JANI_CLOUD_SETTINGS: "{DIR}/settings.json",
        JANI_CLOUD_APPROVERS_FILE: "{DIR}/approvers",
      },
      files: {
        // Writes under out/ ask first, which is what puts an approval in the way.
        "settings.json": JSON.stringify({ version: 1, permissions: { defaultMode: "default", allow: ["Read(./**)"], ask: ["Write(./out/**)"], deny: ["Read(./.env)"] } }),
        approvers: "# nobody yet\n",
      },
    });
    stack = own;
    approversFile = join(own.dir, "approvers");
    b = await chromium.launch();
    alice = await signInByRedirect(await b.newContext(), USERS.alice);
    bob = await signInByRedirect(await b.newContext(), USERS.bob);
    aliceSub = (await me(alice)).sub;
    bobSub = (await me(bob)).sub;
  });
  after(async () => {
    await b?.close();
    await own?.stop();
  });

  test("a held action waits for an approver, and the requester cannot approve it", async () => {
    await script(own, [
      [{ tool: "write", args: { path: "out/a.txt", content: "approved content" } }, { final: "held" }],
      [{ tool: "write", args: { path: "out/a.txt", content: "approved content" } }, { final: "written" }],
      [{ tool: "write", args: { path: "out/b.txt", content: "second" } }, { final: "held again" }],
    ]);
    await turn(alice, "write a.txt");
    assert.ok(!existsSync(join(aliceDir(), "repo/out/a.txt")), "nothing was written while the approval is pending");
    assert.equal(store().filter((r) => r.status === "pending").length, 1);
    await alice.click("[data-v=graph]");
    await alice.click("#dockTabs [data-t=approvals]");
    await alice.waitForFunction(() => document.querySelector("#dockBody")?.textContent.includes("Waiting for an approver"));
    assert.equal(await alice.locator("#dockBody .btn").count(), 0, "no Approve button for the person who asked");
    const cookie = await cookieOf(alice);
    assert.equal((await fetch(own.APP + "/cloud/approvals", { headers: { cookie } })).status, 403, "alice is not an approver");
    assert.equal((await fetch(own.APP + "/cloud/approver", { headers: { cookie } })).status, 403);
    assert.equal((await me(alice)).approver, false);
  });

  test("an unverified email address does not make anyone an approver", async () => {
    writeFileSync(approversFile, `email:${USERS.bob}\n`);
    const m = await me(bob);
    assert.equal(m.emailVerified, false, "the development accounts are unverified");
    assert.equal(m.approver, false, "so an email entry must not grant approval");
  });

  test("an approver named by subject sees the request and approves it in the approver page", async () => {
    writeFileSync(approversFile, `# approvers\nsub:${bobSub}\n`);
    assert.equal((await me(bob)).approver, true, "picked up without a restart");
    await bob.reload();
    await bob.waitForFunction(() => !document.querySelector("#apLink").hidden, null, { timeout: 20000 });
    assert.match(await bob.textContent("#apLink"), /Approvals\s*\(1\)/, "a badge counts the waiting requests");
    await bob.click("#apLink");
    await bob.waitForSelector("[data-testid=approvals] table");
    const row = (await bob.locator("[data-testid=approvals] tbody tr").first().textContent()).replace(/\s+/g, " ");
    assert.match(row, new RegExp(USERS.alice), "shows who asked");
    assert.match(row, /repo\.write/);
    assert.match(row, /repo:out\/a\.txt/);
    const id = store().find((r) => r.status === "pending").approvalId;
    // Deciding takes two clicks, so one stray click cannot approve anything.
    await bob.click(`[data-testid=approve-${id}]`);
    assert.equal(store().find((r) => r.approvalId === id).status, "pending", "one click only arms it");
    await bob.click(`[data-testid=approve-${id}]`);
    await bob.waitForFunction(() => document.querySelector("#status")?.textContent === "Approved.");
    const decided = store().find((r) => r.approvalId === id);
    assert.equal(decided.status, "approved");
    assert.equal(decided.approver, `ifinsta-${keyOf(bobSub)}`, "the store records who approved");
    if (own.uidIsolation) assert.equal(statSync(join(aliceDir(), "state/approvals.json")).uid, statSync(aliceDir()).uid, "the file is still alice's, not root's");
  });

  test("the requester attaches the decision and the action runs once", async () => {
    await alice.click("#dockTabs [data-t=approvals]");
    await alice.waitForFunction(() => /Attach to session/.test(document.querySelector("#dockBody")?.textContent), null, { timeout: 15000 });
    await alice.click("#dockBody .btn:has-text('Attach to session')");
    await alice.waitForSelector(".k-note:has-text('Attached approval')", { timeout: 15000 });
    await turn(alice, "write a.txt now");
    assert.equal(readFileSync(join(aliceDir(), "repo/out/a.txt"), "utf8"), "approved content");
    assert.equal(store().find((r) => r.capability === "repo.write").status, "consumed", "the approval is single use");
  });

  test("every decision is recorded in a hash-chained audit journal", async () => {
    const audit = join(own.data, "audit");
    const out = execFileSync(own.janiBin, ["audit", "verify", "--state-dir", audit], { encoding: "utf8" });
    assert.equal(JSON.parse(out).ok, true, "the chain verifies");
    const events = readFileSync(join(audit, "journal.jsonl"), "utf8").trim().split("\n").map(JSON.parse);
    const e = events.find((x) => x.action === "approval.decide");
    assert.equal(e.actor_id, `ifinsta-${keyOf(bobSub)}`);
    assert.equal(e.decision, "permit");
    assert.equal(e.approval_id, store().find((r) => r.capability === "repo.write").approvalId);
  });

  test("an approver cannot approve their own request, and the store says so", async () => {
    writeFileSync(approversFile, `sub:${bobSub}\nsub:${aliceSub}\n`);
    await turn(alice, "write b.txt");
    const pending = store().find((r) => r.status === "pending");
    assert.ok(pending, "a second request is waiting");
    const cookie = await cookieOf(alice);
    const list = await (await fetch(own.APP + "/cloud/approvals", { headers: { cookie } })).json();
    assert.equal(list.pending.find((p) => p.approvalId === pending.approvalId).own, true, "flagged as her own");
    const res = await fetch(own.APP + "/cloud/approvals/decide", { method: "POST", headers: { cookie, origin: own.APP, "content-type": "application/json" }, body: JSON.stringify({ user: keyOf(aliceSub), id: pending.approvalId, approve: true }) });
    assert.equal(res.status, 409);
    assert.match((await res.json()).message, /separation of duties/i);
    assert.equal(store().find((r) => r.approvalId === pending.approvalId).status, "pending", "unchanged");
  });

  test("deciding needs a same-origin request and a well-formed target", async () => {
    const cookie = await cookieOf(bob);
    const pending = store().find((r) => r.status === "pending");
    const post = (extra, body) => fetch(own.APP + "/cloud/approvals/decide", { method: "POST", headers: { cookie, "content-type": "application/json", ...extra }, body: JSON.stringify(body) });
    const ok = { user: keyOf(aliceSub), id: pending.approvalId, approve: false };
    assert.equal((await post({}, ok)).status, 403, "no Origin");
    assert.equal((await post({ origin: "http://attacker.example" }, ok)).status, 403, "foreign Origin");
    assert.equal((await post({ origin: own.APP, "sec-fetch-site": "cross-site" }, ok)).status, 403);
    for (const bad of [{ ...ok, user: "../../etc" }, { ...ok, user: "z".repeat(24) }, { ...ok, id: "../x" }]) {
      assert.equal((await post({ origin: own.APP }, bad)).status, 409, JSON.stringify(bad));
    }
    assert.equal(store().find((r) => r.approvalId === pending.approvalId).status, "pending", "nothing changed");
    // And rejecting works, and is recorded as a denial.
    assert.equal((await post({ origin: own.APP }, ok)).status, 200);
    assert.equal(store().find((r) => r.approvalId === pending.approvalId).status, "rejected");
  });
});
