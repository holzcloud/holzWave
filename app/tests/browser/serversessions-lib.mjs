// The helpers of the files about the server in the Connections dialog (serversessions*.test.mjs): the stub's hooks `serverLink` and
// `remoteSessions` play the host.

export const ADDRESS = "https://holz.example.net";
export const SIGNED_IN = { state: "signed_in", url: ADDRESS, name: "Holz MacBook", locked: false, sync_on: false, remote_on: true, error: null };
export const ROWS = [
  { id: 4, title: "cargo build", cwd: "/home/holz/holzWave", cols: 120, rows: 40, started: 1_700_000_100, attached: 1, held: true },
  { id: 7, title: "", cwd: "/home/holz/holzWave/core", cols: 80, rows: 24, started: 1_700_000_200, attached: 0, held: false },
];

/**
 * Load the page against the stub with the link `view` and the sessions `rows`. Every case registers a complete init script: an
 * earlier case's script stays and runs first, so each one sets everything it relies on.
 */
export async function open(page, url, { view = SIGNED_IN, rows = ROWS, fails = {} } = {}) {
  await page.addInitScript(
    ([v, r, f]) => {
      const link = window.__MOCK__.serverLink;
      link.reset();
      link.set(v);
      const sessions = window.__MOCK__.remoteSessions;
      sessions.reset();
      sessions.set(r);
      for (const [command, text] of Object.entries(f)) sessions.fails(command, text);
    },
    [view, rows, fails],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
}

/** Open the Connections dialog and wait for the server row to say its state. */
export async function openDialog(page, word = "signed in") {
  await page.evaluate(() => window.__registry.runAction("connections.open"));
  await page.waitForSelector(".dlg.conns");
  await page.waitForFunction((w) => document.querySelector(".conns .cx-srv-state")?.textContent === w, word);
}

export const sessionCalls = (page, command) =>
  page.evaluate((c) => window.__MOCK__.remoteSessions.calls.filter((x) => x[0] === c).map((x) => x[1]), command);
export const linkCalls = (page, command) =>
  page.evaluate((c) => window.__MOCK__.serverLink.calls.filter((x) => x[0] === c).map((x) => x[1]), command);
export const rowButtons = (page) => page.locator(".conns .cx-srv-acts button").allTextContents();
export const sessionTitles = (page) => page.locator(".conns .cx-sess-title").allTextContents();
export const status = (page) => page.locator(".conns .set-status").textContent();
