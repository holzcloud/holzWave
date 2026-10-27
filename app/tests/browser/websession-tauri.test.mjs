// The desktop page must be what it was: the session code of the browser build starts nothing here. No request to
// `/api/session`, no action `unlock.open`, `account.open` or `account.signout`, nothing in the `web` segment of the status line, and
// no style sheet of the web layers.
import assert from "node:assert/strict";
import { installRoutes } from "./login-lib.mjs";

export const backends = ["tauri"];
export const backendNote = "this file checks that the desktop backend starts none of the browser build's session code";

export default async function ({ page, url, errors }) {
  const origin = new URL(url).origin;
  const server = await installRoutes(page, [{ path: "/api/session", body: { state: "locked", csrf: "x", account: { id: "a", username: "u", display_name: "U", role: "admin" }, seconds: 100 } }], { origin });
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForFunction(() => window.__MOCK__ && window.__MOCK__.callsOf("start_pane").length >= 1);
  await page.waitForTimeout(500);
  assert.equal(server.count("GET", "/api/session"), 0, "the desktop page makes no request to /api/session");
  assert.deepEqual(
    server.log.map((r) => `${r.method} ${r.path}`).filter((r) => !r.startsWith("GET /") || r.includes("/api/") || r.includes("/auth/")),
    [],
    "no request of the web build",
  );
  const titles = await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).listActions().map((a) => a.id));
  for (const id of ["unlock.open", "account.open", "account.signout"]) assert.ok(!titles.includes(id), `no action ${id} on the desktop`);
  assert.equal(await page.locator("#status .seg.web").count(), 1, "the segment exists");
  assert.equal(await page.locator("#status .seg.web").evaluate((el) => el.textContent === "" && el.children.length === 0), true, "and is empty");
  assert.equal(await page.locator("#status .seg.web").evaluate((el) => getComputedStyle(el).display), "none", "so nothing is shown");
  const sheets = await page.evaluate(() => Array.from(document.querySelectorAll('link[rel="stylesheet"]')).map((l) => new URL(l.href).pathname));
  assert.ok(!sheets.some((s) => /websession|account/.test(s)), "no style sheet of the web layers is loaded");
  assert.deepEqual(server.foreign, [], "a request left the page's origin");
  assert.deepEqual(errors, [], "the page reported errors");
}
