// The page boots against the stub: it asks for the layout, draws the bar and the divider, and starts every pane.
import assert from "node:assert/strict";

export default async function ({ page, url }) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");

  const view = await page.evaluate(() => window.__MOCK__.view());
  const paneIds = view.panes.map(([id]) => id).sort((a, b) => a - b);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("start_pane").length >= n, paneIds.length);
  await page.waitForTimeout(100); // let a duplicate start_pane show up if there is one

  assert.ok((await page.locator("#bar button").count()) >= 1, "the tab bar has no button");
  assert.equal((await page.locator(".divider").count()), view.dividers.length, "one .divider per divider of the view");

  const calls = await page.evaluate(() => window.__MOCK__.calls.map((c) => c[0]));
  assert.ok(calls.includes("get_layout"), "get_layout was not called");

  const started = (await page.evaluate(() => window.__MOCK__.callsOf("start_pane"))).map((a) => a.pane).sort((a, b) => a - b);
  assert.deepEqual(started, paneIds, "start_pane must be called exactly once per pane of the view");
}
