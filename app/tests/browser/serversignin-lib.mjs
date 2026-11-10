// The helpers of the files about the sign-in dialog (serversignin*.test.mjs): the stub's hook `__MOCK__.serverLink` plays the host.

export const GOOD = "https://holz.example.net";
export const VIEW = { state: "signed_out", url: "", name: "Holz MacBook", locked: false, sync_on: false, remote_on: false, error: null };

/**
 * Load the page against the stub. Every case registers a complete init script (the whole view, the polls, the interval and the start
 * refusal): an earlier case's script stays and runs first, so a knob this one does not set would leak from the earlier one.
 */
export async function open(page, url, { view = {}, polls = [], interval = 1, startFails = null } = {}) {
  await page.addInitScript(
    ([v, p, i, s]) => {
      const link = window.__MOCK__.serverLink;
      link.reset();
      link.set(v);
      link.polls = p;
      link.setInterval(i);
      link.startFails(s);
    },
    [{ ...VIEW, ...view }, polls, interval, startFails],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
}

/** Open the dialog the way the popover does and keep its result in `window.__RESULT`. */
export async function openDialog(page) {
  await page.evaluate(() => {
    window.__RESULT = undefined;
    void import(new URL("serversignin.js", location.href).href)
      .then((m) => m.openServerSignIn())
      .then((view) => {
        window.__RESULT = { view };
      });
  });
  await page.getByRole("dialog", { name: "Sign in to the server" }).waitFor();
}

export const dialog = (page) => page.getByRole("dialog", { name: "Sign in to the server" });
export const link = (page, cmd) => page.evaluate((c) => window.__MOCK__.serverLink.calls.filter((x) => x[0] === c).map((x) => x[1]), cmd);
export const count = async (page, cmd) => (await link(page, cmd)).length;
export const result = (page) => page.evaluate(() => window.__RESULT);
export const gone = (page) => dialog(page).waitFor({ state: "detached" });
export const buttons = (page) => page.locator(".server-signin button").evaluateAll((nodes) => nodes.map((n) => n.textContent));
export const stateText = (page) => page.locator(".server-signin .ss-state").textContent();

