// DESK-29 part 2, the page of one window among several: it listens with its own label (a listener without a target
// hears every window), says `window_ready` once it has drawn its first layout and listens, and a layout command that
// the shell refuses with `no such window` is not an error of the page. A second page with another label stands for
// the second window; the two never see each other's events.
import assert from "node:assert/strict";
import { callsOf, emit, newPage } from "./lib.mjs";

async function open(page, url, query = "?renderer=canvas") {
  await page.goto(`${url}index.html${query}`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
}

const labelOf = (page) => page.evaluate(() => window.__TAURI_INTERNALS__.metadata.currentWindow.label);
const headTitle = (page, pane) => page.locator(`.panehead[data-pane="${pane}"] .ph-title`).textContent();

export default async function ({ page, url, errors }) {
  await readyCase(page, url);
  await labelCase(page, url);
  await rejectedCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Every window, `main` too, calls `window_ready` once, after its first layout was applied and its listeners exist. */
async function readyCase(page, url) {
  await open(page, url);
  assert.equal(await labelOf(page), "main");
  const calls = await page.evaluate(() => window.__MOCK__.calls.map(([cmd]) => cmd));
  assert.equal(calls.filter((c) => c === "window_ready").length, 1, "once");
  assert.ok(calls.indexOf("window_ready") > calls.indexOf("get_layout"), "after the first layout was asked for and applied");
  assert.ok(await page.locator(".divider").count(), "the layout is drawn when the shell is told");
  const listening = await page.evaluate(() => ["frame", "layout", "title", "cwd", "pane-exit", "incoming-drop"].map((n) => window.__MOCK__.listening(n)));
  assert.ok(listening.every((n) => n >= 1), `every listener existed by the time the shell was told: ${listening}`);
  assert.ok(calls.indexOf("window_ready") > calls.indexOf("start_pane"), "the panes were started by the layout that was applied");

  // The window label comes from the metadata the shell injects: a second window has its own.
  const second = await newPage(page.context().browser());
  try {
    await open(second.page, url, "?renderer=canvas&window=window-2");
    assert.equal(await labelOf(second.page), "window-2");
    assert.equal((await callsOf(second.page, "window_ready")).length, 1, "a torn off window says it is ready too");
    assert.deepEqual(second.errors, []);
  } finally {
    await second.page.close().catch(() => {});
  }
}

/** The page listens with its own label: what is addressed to another window is never delivered to it. */
async function labelCase(page, url) {
  await open(page, url);
  await emit(page, "title", { pane: 1, title: "for everybody" });
  assert.equal(await headTitle(page, 1), "for everybody", "an event for all windows is heard");
  const mine = await page.evaluate(() => window.__MOCK__.emitTo("main", "title", { pane: 1, title: "for main" }));
  assert.equal(mine, 1, "an event addressed to this window is heard once");
  assert.equal(await headTitle(page, 1), "for main");
  for (const name of ["frame", "layout", "title", "cwd", "pane-exit"]) {
    const other = await page.evaluate((n) => window.__MOCK__.emitTo("window-2", n, { pane: 1, title: "for the other window" }), name);
    assert.equal(other, 0, `${name}: an event for window-2 does not reach a listener of main`);
  }
  assert.equal(await headTitle(page, 1), "for main", "and nothing of it was applied");

  // Both pages at once: each draws only what is addressed to it.
  const second = await newPage(page.context().browser());
  try {
    await open(second.page, url, "?renderer=canvas&window=window-2");
    await second.page.evaluate(() => window.__MOCK__.emitTo("window-2", "title", { pane: 1, title: "second" }));
    await page.evaluate(() => window.__MOCK__.emitTo("window-2", "title", { pane: 1, title: "second" }));
    await second.page.evaluate(() => window.__MOCK__.emitTo("main", "title", { pane: 1, title: "first" }));
    assert.equal(await headTitle(second.page, 1), "second");
    assert.equal(await headTitle(page, 1), "for main");
    assert.deepEqual(second.errors, []);
  } finally {
    await second.page.close().catch(() => {});
  }
}

/** A layout command refused with `no such window` (the window is going) is no uncaught error of the page. */
async function rejectedCase(page, url) {
  await open(page, url);
  await page.evaluate(() => window.__MOCK__.failLayout("no such window"));
  await page.evaluate(() => document.querySelector("#bar .plus")?.click());
  await page.evaluate(() => document.querySelector("#bar .ws-chip, #bar .tab")?.dispatchEvent(new MouseEvent("click", { bubbles: true })));
  await page.waitForTimeout(100);
  assert.ok((await callsOf(page, "new_workspace")).length + (await callsOf(page, "select_workspace")).length + (await callsOf(page, "select_tab")).length >= 1, "a layout command was sent and refused");
  // run.mjs fails the test on any page error: the refusal must not be one.
}
