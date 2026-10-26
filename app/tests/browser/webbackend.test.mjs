// The web backend of the bridge: the page has no Tauri global and talks to the server over one socket. This file runs
// on the ws backend only (the fake socket of ws-mock.js); the rest of the suite runs on both backends.
import assert from "node:assert/strict";

export const backends = ["ws"];
export const backendNote = "the subject is the socket backend itself";

export default async function ({ page, url }) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");

  assert.equal(await page.evaluate(() => typeof window.__TAURI__), "undefined", "the page must run without the Tauri global");

  // One socket, on the page's own host, path /ws, plain ws for the http test page.
  const urls = await page.evaluate(() => window.__WS_MOCK__.urls);
  assert.equal(urls.length, 1, `one socket expected, got ${JSON.stringify(urls)}`);
  assert.equal(urls[0], `ws://${new URL(url).host}/ws`);

  // The first frame is the request for the layout, with id 1 and no arguments.
  const sent = await page.evaluate(() => window.__WS_MOCK__.sent);
  const first = JSON.parse(sent[0]);
  console.log(`tracer: first frame ${sent[0]}, address ${urls[0]}`);
  assert.ok(first.cmd, "the first frame names a command");
  assert.equal(first.id, 1);
  assert.deepEqual(first.args, {});

  // An event of the core reaches the page as an event frame and redraws it.
  const view = await page.evaluate(() => window.__MOCK__.view());
  view.tabs[0].title = "from the wire";
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), view);
  await page.waitForFunction(() => document.body.textContent.includes("from the wire"));
}
