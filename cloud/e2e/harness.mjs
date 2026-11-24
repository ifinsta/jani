// Starts the real stack: ifinsta accounts-server, the jani-cloud gateway, a
// scripted model provider, and (through the gateway) one `jani ui` per person.
// The gateway is configured only from what `accounts-server register-service`
// prints, exactly as an operator would.
//
//   ACCOUNTS_BIN  accounts-server   (default ../../../accounts/target/debug/accounts-server)
//   GATEWAY_BIN   jani-cloud        (default ../target/debug/jani-cloud)
//   JANI_BIN      jani              (default ../../target/debug/jani)

import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, existsSync, chmodSync } from "node:fs";
import { rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { generateKeyPairSync } from "node:crypto";

const HERE = dirname(fileURLToPath(import.meta.url));
const ACCOUNTS_BIN = resolve(process.env.ACCOUNTS_BIN || join(HERE, "../../../accounts/target/debug/accounts-server"));
const GATEWAY_BIN = resolve(process.env.GATEWAY_BIN || join(HERE, "../target/debug/jani-cloud"));
const JANI_BIN = resolve(process.env.JANI_BIN || join(HERE, "../../target/debug/jani"));

export const PASSWORD = "a-development-only-password-1";
export const USERS = { alice: "alice@example.test", bob: "bob@example.test" };

async function freePort(exclude = []) {
  for (;;) {
    const l = createServer();
    await new Promise((ok, fail) => { l.once("error", fail); l.listen(0, "127.0.0.1", ok); });
    const p = l.address().port;
    await new Promise((ok) => l.close(ok));
    if (!exclude.includes(p)) return p;
  }
}

async function waitFor(url, child, ms = 30000) {
  const until = Date.now() + ms;
  for (;;) {
    if (child.exitCode !== null || child.signalCode !== null) throw new Error(`process exited before ${url} was ready`);
    try { if ((await fetch(url)).ok) return; } catch (_) {}
    if (Date.now() > until) throw new Error(`timed out waiting for ${url}`);
    await new Promise((r) => setTimeout(r, 150));
  }
}

export async function startStack({ gatewayEnv = {} } = {}) {
  for (const [bin, what] of [[ACCOUNTS_BIN, "accounts-server"], [GATEWAY_BIN, "jani-cloud"], [JANI_BIN, "jani"]]) {
    if (!existsSync(bin)) throw new Error(`missing ${what}: ${bin}`);
  }
  const dir = mkdtempSync(join(tmpdir(), "jani-cloud-e2e-"));
  // mkdtemp is 0700; per-person Unix users must be able to traverse to their own directory.
  chmodSync(dir, 0o711);
  const procs = [];
  const logs = { accounts: "", gateway: "", provider: "" };
  const stop = async () => {
    await Promise.all(procs.map((p) => new Promise((ok) => {
      if (p.exitCode !== null || p.signalCode !== null || !p.pid) return ok();
      const t = setTimeout(() => p.kill("SIGKILL"), 3000);
      p.once("exit", () => { clearTimeout(t); ok(); });
      p.kill("SIGTERM");
    })));
    if (!dir.startsWith(join(tmpdir(), "jani-cloud-e2e-"))) throw new Error("unsafe cleanup path");
    await rm(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  };
  try {
    const accountsPort = await freePort();
    const appPort = await freePort([accountsPort]);
    const providerPort = await freePort([accountsPort, appPort]);
    const ACCOUNTS = `http://localhost:${accountsPort}`;
    const APP = `http://localhost:${appPort}`;
    const ISSUER = `${ACCOUNTS}/oidc`;
    const { privateKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
    writeFileSync(join(dir, "idp.key"), privateKey.export({ type: "pkcs8", format: "pem" }));
    mkdirSync(join(dir, "mail"));
    const base = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(ACCOUNTS_|IFINSTA_|JANI_|SIWI_)/i.test(k)));
    const accountsEnv = {
      ...base,
      ACCOUNTS_RSA_PRIVATE_KEY_FILE: join(dir, "idp.key"),
      ACCOUNTS_DATABASE: join(dir, "accounts.sqlite"),
      ACCOUNTS_ISSUER: ISSUER,
      ACCOUNTS_DEV_INSECURE_COOKIES: "1",
      ACCOUNTS_DEV_SIGNUP: "1",
      ACCOUNTS_BIND: `127.0.0.1:${accountsPort}`,
      ACCOUNTS_WEBAUTHN_ORIGIN: ACCOUNTS,
      ACCOUNTS_DEV_MAIL_DIR: join(dir, "mail"),
      RUST_LOG: "warn",
    };
    const run = (args) => execFileSync(ACCOUNTS_BIN, args, { env: accountsEnv, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    for (const email of Object.values(USERS)) run(["seed-user", email, PASSWORD]);
    // The real onboarding path: register Jani as an ifinsta service.
    const registration = JSON.parse(run(["register-service", "jani", APP, "--client-id", "jani-cloud", "--frame-origin", APP, "--logout-path", "/", "--format", "json"]));

    const spawnLogged = (name, bin, args, env) => {
      const p = spawn(bin, args, { env, stdio: ["ignore", "pipe", "pipe"] });
      p.stdout.on("data", (d) => (logs[name] = (logs[name] + d).slice(-65536)));
      p.stderr.on("data", (d) => (logs[name] = (logs[name] + d).slice(-65536)));
      procs.push(p);
      return p;
    };
    const accounts = spawnLogged("accounts", ACCOUNTS_BIN, ["serve"], accountsEnv);
    await waitFor(`${ACCOUNTS}/health/live`, accounts);
    const provider = spawnLogged("provider", "python3", [join(HERE, "fake_provider.py"), String(providerPort)], base);
    await new Promise((r) => setTimeout(r, 500));
    if (provider.exitCode !== null) throw new Error("scripted provider did not start");

    const isRoot = process.getuid?.() === 0;
    // E2E_SANDBOX=bwrap runs every workspace inside bubblewrap; E2E_NO_UID=1 drops the per-person Unix user.
    const sandbox = process.env.E2E_SANDBOX || "off";
    const uidIsolation = isRoot && process.env.E2E_NO_UID !== "1";
    const data = join(dir, "data");
    const gatewayEnvFull = {
      ...base,
      IFINSTA_ISSUER: registration.env.SIWI_ISSUER,
      IFINSTA_CLIENT_ID: registration.env.SIWI_CLIENT_ID,
      IFINSTA_CLIENT_SECRET: registration.env.SIWI_CLIENT_SECRET,
      IFINSTA_REDIRECT_URI: registration.env.SIWI_REDIRECT_URI,
      IFINSTA_POST_LOGOUT_REDIRECT_URI: `${APP}/`,
      IFINSTA_SCOPES: "openid email offline_access",
      IFINSTA_DEV_ALLOW_INSECURE_LOOPBACK: "1",
      IFINSTA_RATE_LIMIT_PER_MINUTE: "5000",
      JANI_CLOUD_BIND: `127.0.0.1:${appPort}`,
      JANI_CLOUD_DATA: data,
      JANI_CLOUD_JANI_BIN: JANI_BIN,
      JANI_CLOUD_JANI_ARGS: `--allow-write --write-path out --api-base http://127.0.0.1:${providerPort}/v1 --model fake-1`,
      JANI_CLOUD_MAX_ACTIVE: "4",
      // Isolation: a separate Unix user per person when running as root,
      // otherwise the explicit development opt-out. bubblewrap is not assumed.
      JANI_CLOUD_SANDBOX: sandbox,
      ...(uidIsolation ? { JANI_CLOUD_UID_BASE: "41000" } : {}),
      ...(!uidIsolation && sandbox === "off" ? { JANI_CLOUD_ALLOW_UNISOLATED: "1" } : {}),
      JANI_API_KEY: "test-key",
      RUST_LOG: "info",
      ...gatewayEnv,
    };
    const gateway = spawnLogged("gateway", GATEWAY_BIN, [], gatewayEnvFull);
    await waitFor(`${APP}/health`, gateway);
    return { dir, data, logs, ACCOUNTS, APP, ISSUER, isRoot, janiBin: JANI_BIN, registration, gateway, sandbox, uidIsolation, stop };
  } catch (error) {
    await stop();
    throw error;
  }
}
