// Helpers for the tests of the account dialog: the fixtures of `GET /api/me`, a route whose answer follows a state the test
// changes, and the readers for the dialog. The scripted server is the one of login-lib.mjs.
import { focusedName, holdsSecret, installAuthenticator, leftover, openMain, sessionRoute } from "./websession-lib.mjs";

export { focusedName, holdsSecret, installAuthenticator, leftover, openMain, sessionRoute };
export { homeStub } from "./websession-lib.mjs";

const iso = (secondsAgo) => new Date(Date.now() - secondsAgo * 1000).toISOString();

/** A password account with an authenticator, two passkeys, three sessions and open saved passwords. A fresh copy each call. */
export function fixture(over = {}) {
  return {
    account: { id: "acc1", username: "holz", display_name: "Holz", role: "admin" },
    oidc: null,
    password: true,
    totp: true,
    passkeys: [
      { id: "k1", name: "MacBook", created_at: iso(900_000), last_used: iso(100) },
      { id: "k2", name: "Phone", created_at: iso(900_000), last_used: iso(3 * 86_400) },
    ],
    secrets: { state: "unlocked", passphrase: false, recovery: true },
    sessions: [
      { id: "s2", device: "Chrome on Linux", addr: "198.51.100.24", created_at: iso(20_000), last_seen: iso(3 * 3600), current: false },
      { id: "s1", device: "Firefox on macOS", addr: "203.0.113.7", created_at: iso(30_000), last_seen: iso(5), current: true },
      { id: "s3", device: "Safari on iOS", addr: "192.0.2.9", created_at: iso(40_000), last_seen: iso(7 * 3600), current: false },
    ],
    ...over,
  };
}

/** The route for `GET /api/me`: it answers what `state.me` is at that moment, so a test changes the account between requests. */
export const meRoute = (state) => ({ path: "/api/me", body: () => structuredClone(state.me) });

/** The step the server asks for: a 403 that says a recent sign-in is needed. */
export const STEP = { status: 403, body: { code: "forbidden", step_up: true } };
/** The answer of `POST /auth/step-up`. */
export const stepUpRoute = { method: "POST", path: "/auth/step-up", body: { step_up: 300 } };

/** Open the main page with the scripted server and the account dialog. `table` is added to the session and `/api/me`. */
export async function openAccountDialog(page, url, state, table = [], suffix = "?renderer=canvas") {
  const server = await openMain(page, url, [sessionRoute(), meRoute(state), ...table], suffix);
  await runAction(page, "account.open");
  await page.getByRole("dialog", { name: "Account", exact: true }).waitFor();
  await page.waitForSelector(".acc-group");
  return server;
}

/** Run a registered action as the palette would. */
export const runAction = (page, id) => page.evaluate(async (a) => (await import(new URL("actions.js", location.href).href)).runAction(a), id);

export const dialog = (page) => page.getByRole("dialog", { name: "Account", exact: true });
export const layer = (page) => page.getByRole("dialog", { name: "Confirm it is you" });
export const statusLine = (page) => page.locator(".account .set-status").textContent();
export const legends = (page) => page.evaluate(() => Array.from(document.querySelectorAll(".account .acc-group > legend")).map((l) => l.textContent));
/** The row of a group: its name, its state word and its button. */
export const rowOf = (page, id) =>
  page.evaluate((g) => {
    const row = document.querySelector(`.account [data-group="${g}"] .acc-row`);
    if (!row || row.hidden) return null;
    return { name: row.querySelector(".acc-name")?.textContent, state: row.querySelector(".acc-state")?.textContent ?? null, button: row.querySelector("button")?.textContent ?? null };
  }, id);
/** The words of the rows of a list under a group (passkeys, sessions). */
export const itemsOf = (page, id) =>
  page.evaluate((g) => Array.from(document.querySelectorAll(`.account [data-group="${g}"] .acc-item`)).map((li) => Array.from(li.children).map((c) => c.textContent).join(" | ")), id);

/** Confirm it is you, in the layer, with `password`. */
export async function confirmWith(page, password) {
  await layer(page).waitFor();
  await page.locator("#web-step-field").fill(password);
  await page.keyboard.press("Enter");
  await layer(page).waitFor({ state: "detached" });
}

/** The places that must be clean after a flow, checked against every secret of the flow. */
export const clean = async (page, server, secrets, what) => {
  const found = await holdsSecret(page, secrets);
  if (found.length > 0) throw new Error(`${what}: ${found.join("; ")}`);
  const l = leftover(server);
  if (l.foreign.length > 0) throw new Error(`${what}: a request left the page's origin: ${l.foreign.join(", ")}`);
  if (l.unscripted.length > 0) throw new Error(`${what}: a request nobody scripted: ${l.unscripted.join(", ")}`);
};
