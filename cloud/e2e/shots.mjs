import { createRequire } from "node:module";
import { startStack, USERS, PASSWORD } from "./harness.mjs";
const { chromium } = createRequire(import.meta.url)("playwright");
const stack = await startStack();
const b = await chromium.launch();
for (const [name, vp, scheme] of [["d", { width: 1280, height: 800 }, "light"], ["p", { width: 390, height: 780 }, "dark"]]) {
  const ctx = await b.newContext({ viewport: vp, colorScheme: scheme });
  const p = await ctx.newPage();
  await p.goto(stack.APP + "/"); await p.waitForTimeout(800);
  await p.screenshot({ path: `/tmp/cloud_login_${name}.png` });
  // open the real sign-in surface
  await p.locator("[data-testid=sign-in]").locator("button").click();
  await p.waitForTimeout(2500);
  await p.screenshot({ path: `/tmp/cloud_modal_${name}.png` });
  await ctx.close();
}
const ctx = await b.newContext({ viewport: { width: 1280, height: 800 } });
const p = await ctx.newPage();
await p.goto(`${stack.APP}/siwi/login?return_to=/`);
await p.waitForURL((u) => u.toString().startsWith(stack.ACCOUNTS + "/login"));
await p.fill("#email", USERS.alice); await p.fill("#password", PASSWORD); await p.click("button[type=submit]");
await p.waitForFunction(() => document.querySelector("#wt")?.textContent.startsWith("/"));
await p.waitForTimeout(1200);
await p.screenshot({ path: "/tmp/cloud_ws.png" });
await b.close(); await stack.stop();
