// End to end: real ifinsta accounts, the jani-cloud gateway, and real `jani ui`
// processes. Signs people in through the actual accounts login.
import { test, before, after, describe } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join } from "node:path";
import { startStack, USERS, PASSWORD } from "./harness.mjs";

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
        assert.deepEqual(readdirSync(join(root, "work")).sort(), ["home", "repo", "state"], "only its own directory is mounted");
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
    test(`no workspace outlives the gateway when it ${label}`, async () => {
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
