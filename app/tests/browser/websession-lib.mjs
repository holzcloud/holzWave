// Helpers for the tests of the session marks, the unlock dialog, `Confirm it is you` and the account dialog: the main page on
// the web backend with a scripted server (`installRoutes` of login-lib.mjs), and readers for the status line and the layers.
import { installRoutes } from "./login-lib.mjs";

export { anonymous, errorText, focusedName, homeStub, installAuthenticator, installClipboard, installRoutes, refusal } from "./login-lib.mjs";

/** The account the scripted sessions belong to. */
export const ACCOUNT = { id: "acc1", username: "holz", display_name: "Holz", role: "admin" };

/** The answer of `GET /api/session` for a signed-in person. `over` replaces members. */
export function session(over = {}) {
  return { state: "signed_in", host: "holz.example.net", csrf: "csrf-token-1", account: ACCOUNT, seconds: 1500, step_up: 0, ...over };
}

/** The route for `GET /api/session`. */
export const sessionRoute = (over = {}) => ({ path: "/api/session", body: session(over) });

/**
 * Script the server and load the main page on the web backend. `table` is the list of routes (`installRoutes`); the page is
 * ready when the mock core has been asked to start a pane. Returns the recorder of `installRoutes`.
 */
export async function openMain(page, url, table, suffix = "?renderer=canvas") {
  const origin = new URL(url).origin;
  await page.unrouteAll({ behavior: "wait" });
  const server = await installRoutes(page, table, { origin });
  await page.goto(`${url}index.html${suffix}`);
  await page.waitForFunction(() => window.__MOCK__ && window.__MOCK__.callsOf("start_pane").length >= 1);
  return server;
}

/** What a recorded server must not have seen: a request to another origin, or an unscripted request that is not a static file or the icon. */
export function leftover(server) {
  return {
    foreign: server.foreign,
    unscripted: server.unscripted.filter((u) => !u.endsWith("/favicon.ico")),
  };
}

/** The words of the marks in the status line, in order. */
export const marksOf = (page) =>
  page.evaluate(() => Array.from(document.querySelectorAll("#status .web-mark")).map((m) => m.querySelector(".web-word")?.textContent ?? ""));

/** The text of the whole `web` segment of the status line. */
export const webText = (page) => page.evaluate(() => document.querySelector("#status .seg.web")?.textContent ?? "");

/** The titles of the actions the page has registered, as the palette lists them. */
export const actionTitles = (page) =>
  page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).listActions().map((a) => a.title));

/** Whether the keyboard is with the terminal. */
export const terminalHasFocus = (page) => page.evaluate(() => document.activeElement?.id === "ime");

/** Texts the page must never hold: found in the address, the title, Web Storage, cookies or the markup (attributes and text) of the page. */
export function holdsSecret(page, secrets) {
  return page.evaluate((list) => {
    const found = [];
    const html = document.documentElement.outerHTML;
    const storage = JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)]);
    const places = { address: location.href, title: document.title, cookie: document.cookie, storage, markup: html };
    for (const secret of list) {
      for (const [name, value] of Object.entries(places)) if (value.includes(secret)) found.push(`${name} has ${secret.length} characters of a secret`);
    }
    return found;
  }, secrets);
}
