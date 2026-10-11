// A change of the device pixel ratio is followed through the resolution media query, not through the `resize`
// event (WR-07): moving the window to a display with another scale factor sends no `resize` when the CSS size of the
// window stays the same.
//
// No engine lets a test change the ratio without a size change and tell the media query lists about it, so the page
// gets a stand-in for `matchMedia` that keeps the resolution queries and fires their `change` listeners on demand.
// What is tested is what the page does with the notification: measure again, tell the core, watch the new ratio.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";

/** Runs in the page before its scripts: `window.__DPR__` lists the resolution queries and changes the ratio. */
function installMediaStub() {
  const real = window.matchMedia.bind(window);
  const queries = [];
  window.matchMedia = (query) => {
    if (!/^\(resolution:/.test(query)) return real(query);
    const entry = { query, listeners: [] };
    queries.push(entry);
    return {
      media: query,
      matches: true,
      addEventListener(type, listener, options) {
        if (type === "change") entry.listeners.push({ listener, once: Boolean(options && options.once) });
      },
      removeEventListener() {},
      addListener() {},
      removeListener() {},
    };
  };
  window.__DPR__ = {
    queries: () => queries.map((q) => q.query),
    /** The ratio becomes `dpr`; the newest query, the one for the ratio before, reports a change. */
    change(dpr) {
      Object.defineProperty(window, "devicePixelRatio", { value: dpr, configurable: true });
      const entry = queries.at(-1);
      const fired = entry.listeners.splice(0);
      for (const { listener } of fired) listener({ type: "change", matches: false, media: entry.query });
      return fired.length;
    },
  };
}

export default async function ({ page, url }) {
  await page.addInitScript(installMediaStub);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);

  const [started] = await callsOf(page, "start_pane");
  assert.deepEqual(await page.evaluate(() => window.__DPR__.queries()), ["(resolution: 1dppx)"]);
  const resizes = (await callsOf(page, "resize")).length;

  assert.equal(await page.evaluate(() => window.__DPR__.change(2)), 1, "the page listens for the end of the ratio");

  // The size of a cell in device pixels doubles, and the core is told: it answers size reports with it.
  await page.waitForFunction((n) => window.__MOCK__.callsOf("resize").length > n, resizes);
  const resized = (await callsOf(page, "resize")).at(-1);
  assert.ok(resized.cell_w >= started.cell_w * 2 - 1, `cell_w ${started.cell_w} -> ${resized.cell_w}`);
  assert.ok(resized.cell_h >= started.cell_h * 2 - 1, `cell_h ${started.cell_h} -> ${resized.cell_h}`);
  const canvas = await page.evaluate(() => {
    const c = document.getElementById("term");
    return { width: c.width, css: c.clientWidth };
  });
  assert.equal(canvas.width, canvas.css * 2, "the canvas backing store follows the ratio");

  // The watch is renewed for the new ratio, and that one is followed as well.
  assert.deepEqual(await page.evaluate(() => window.__DPR__.queries()), ["(resolution: 1dppx)", "(resolution: 2dppx)"]);
  const before = (await callsOf(page, "resize")).length;
  assert.equal(await page.evaluate(() => window.__DPR__.change(1)), 1);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("resize").length > n, before);
  const back = (await callsOf(page, "resize")).at(-1);
  assert.ok(back.cell_w <= started.cell_w + 1, `cell_w back to ${started.cell_w}: ${back.cell_w}`);
}
