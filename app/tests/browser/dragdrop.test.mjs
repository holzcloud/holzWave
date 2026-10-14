// DESK-20 / DESK-29: dragging a pane by its header and a tab in the strip sends one move command with ids and an edge,
// shows an outline while it goes, and sends nothing on Escape, on a plain click or on a drop on nothing. The terminal
// never hears of it.
//
// DESK-29 part 2 (the tear-off cases at the end): a drag released 24 px or more outside the window sends the pane's id
// to the shell (`release_outside`), a palette action does the same without the mouse, a drop that comes from another
// window (`incoming-drop`) is answered with the zone under the pointer, and a pane that left the window is forgotten by
// its page and gets a full frame when it comes back. While a drag is outside its window the other window shows the
// drop outline (`drag-hover`, `drag-end`); two pages stand for two windows and the test plays the shell between them.
import assert from "node:assert/strict";
import { callsOf, emit, newPage } from "./lib.mjs";

const VIEW = {
  workspaces: [{ name: "alpha", active: true }, { name: "beta", active: false }],
  tabs: [
    { title: "one", active: true, focus: 1 },
    { title: "two", active: false, focus: 3 },
    { title: "three", active: false, focus: 4 },
  ],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
};

async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  await emit(page, "layout", VIEW);
  await page.waitForSelector("#bar .tab-item:nth-child(3)");
}

/** A pane's grid area on the window: the canvas box and the unit rectangle of the pane. */
async function paneBox(page, unit) {
  const c = await page.locator("canvas").first().boundingBox();
  return { x: c.x + unit.x * c.width, y: c.y + unit.y * c.height, w: unit.w * c.width, h: unit.h * c.height };
}

async function drag(page, from, to, { release = true, steps = 6 } = {}) {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps });
  if (release) await page.mouse.up();
}

const sent = async (page, cmd) => callsOf(page, cmd);

export default async function ({ page, url }) {
  await open(page, url);
  const right = await paneBox(page, VIEW.panes[1][1]);
  const left = await paneBox(page, VIEW.panes[0][1]);
  const headerPoint = async (box) => {
    // The right part of the header strip, left of the ×, is the canvas's (the title is a node of the page): both start a drag.
    return { x: box.x + box.w - 60, y: box.y + 8 };
  };

  // A pane header dragged to the bottom edge of the other pane: one move_pane, edge bottom.
  await drag(page, await headerPoint(left), { x: right.x + right.w / 2, y: right.y + right.h - 10 }, { release: false });
  assert.equal(await page.locator(".drop-outline").count(), 1, "an outline shows where the pane would go");
  const o = await page.locator(".drop-outline").boundingBox();
  assert.ok(Math.abs(o.y - (right.y + right.h / 2)) < 2 && Math.abs(o.height - right.h / 2) < 2, `the lower half of the target: ${JSON.stringify({ o, right })}`);
  await page.mouse.up();
  assert.equal(await page.locator(".drop-outline").count(), 0, "the outline is gone after the drop");
  assert.deepEqual(await sent(page, "move_pane"), [{ pane: 1, target: 2, edge: "bottom" }]);

  // The title is a node of the page: pressing on it drags too. The middle of the target swaps.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  await drag(page, { x: (await page.locator('.panehead[data-pane="1"] .ph-title').boundingBox()).x + 4, y: left.y + 8 }, { x: right.x + right.w / 2, y: right.y + right.h / 2 });
  assert.deepEqual(await sent(page, "move_pane"), [{ pane: 1, target: 2, edge: "center" }]);

  // Escape cancels, a drop on the pane itself and a plain click change nothing.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  await drag(page, await headerPoint(left), { x: right.x + 40, y: right.y + right.h / 2 }, { release: false });
  await page.keyboard.press("Escape");
  await page.mouse.up();
  assert.equal(await page.locator(".drop-outline").count(), 0);
  await drag(page, await headerPoint(left), { x: left.x + 100, y: left.y + 100 });
  await page.mouse.click(right.x + right.w - 60, right.y + 8);
  assert.deepEqual(await sent(page, "move_pane"), [], "Escape, a drop on itself and a click send no move");

  // A pane onto another tab moves into it, next to that tab's focus pane.
  await drag(page, await headerPoint(left), await page.locator("#bar .tab").nth(1).boundingBox().then((b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 })));
  assert.deepEqual(await sent(page, "move_pane"), [{ pane: 1, target: 3, edge: "right" }]);

  // Tabs: to another place in the strip, and onto a workspace chip.
  const centre = (b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });
  await page.evaluate(() => window.__MOCK__.reset());
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  await drag(page, centre(await page.locator("#bar .tab").nth(0).boundingBox()), centre(await page.locator("#bar .tab").nth(2).boundingBox()));
  assert.deepEqual(await sent(page, "move_tab"), [{ from: 0, to: 2 }]);
  await drag(page, centre(await page.locator("#bar .tab").nth(0).boundingBox()), centre(await page.locator("#bar .ws-chip").nth(1).boundingBox()));
  assert.deepEqual(await sent(page, "move_tab_to_workspace"), [{ tab: 0, workspace: 1 }]);
  assert.deepEqual(await sent(page, "select_tab"), [], "the click that ends a drag does not select a tab");

  // A pane onto a workspace chip: the core puts it next to the focus pane of that workspace's shown tab. The shown
  // workspace's own chip is no target, and the click that ends the drag selects nothing.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  const chip = centre(await page.locator("#bar .ws-chip").nth(1).boundingBox());
  await drag(page, await headerPoint(left), chip, { release: false });
  const over = await page.locator(".drop-outline").boundingBox();
  const item = await page.locator("#bar .ws-item").nth(1).boundingBox();
  assert.ok(Math.abs(over.x - item.x) < 2 && Math.abs(over.width - item.width) < 2, "the outline is the chip");
  await page.mouse.up();
  assert.deepEqual(await sent(page, "move_pane_to_workspace"), [{ pane: 1, workspace: 1 }]);
  assert.deepEqual(await sent(page, "move_tab_to_workspace"), [], "a pane is not a tab");
  assert.deepEqual(await sent(page, "select_workspace"), [], "the click that ends a drag does not select a workspace");
  await drag(page, await headerPoint(left), centre(await page.locator("#bar .ws-chip").nth(0).boundingBox()), { release: false });
  assert.equal(await page.locator(".drop-outline").count(), 0, "the workspace the pane is in is no target");
  await page.mouse.up();
  assert.equal((await sent(page, "move_pane_to_workspace")).length, 1);

  // None of it reaches the program.
  for (const cmd of ["key", "text", "mouse", "select", "paste"]) {
    assert.deepEqual(await sent(page, cmd), [], `${cmd} is not sent while dragging`);
  }

  await releaseOutsideCase(page, url);
  await paletteCase(page, url);
  await forgetCase(page, url);
  await incomingCase(page, url);
  await hoverCase(page, url);
  await noticeCase(page, url); // last: it installs a fake clock that stays on the page
}

// ---- between windows ----------------------------------------------------------------------------------------------

const PANE_2_BG = 0x2a6fdb;
const MODES = { alt: false, bracketed_paste: false, mouse: false };
const fullFrame = (pane, seq, bg) => ({
  pane, seq, full: true, cols: 20, rows: 4, fg: 0xffffff, bg, cursor: null,
  scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed: [],
});

/** The view of one tab with the given panes side by side, as the core would send it for that window. */
const viewOf = (ids, rev, extra = {}) => ({
  ...VIEW,
  rev,
  tabs: VIEW.tabs.map((t, i) => (i === 0 ? { ...t, focus: ids[0] } : t)),
  focus: ids[0],
  panes: ids.map((id, i) => [id, { x: i / ids.length, y: 0, w: 1 / ids.length, h: 1 }]),
  dividers: ids.length > 1 ? VIEW.dividers : [],
  ...extra,
});

const reset = async (page, view) => {
  await page.evaluate(() => window.__MOCK__.reset());
  await page.evaluate((v) => window.__MOCK__.setView(v), view);
};

/** The colour (0xRRGGBB) at a point of the canvas in CSS px, read in the task of a draw (WebGL is not readable later). */
const pixel = (page, x, y) =>
  page.evaluate(
    async ([px, py]) => {
      await new Promise((done) => requestAnimationFrame(() => done()));
      const c = document.getElementById("term");
      const gl = c.getContext("webgl2");
      const out = new Uint8Array(4);
      if (gl) gl.readPixels(Math.floor(px), gl.drawingBufferHeight - 1 - Math.floor(py), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out);
      else out.set(c.getContext("2d").getImageData(Math.floor(px), Math.floor(py), 1, 1).data);
      return (out[0] << 16) | (out[1] << 8) | out[2];
    },
    [x, y],
  );

/** A release at least 24 px outside the window asks the shell for a window; a slip over the edge does nothing. */
async function releaseOutsideCase(page, url) {
  await open(page, url);
  const width = await page.evaluate(() => window.innerWidth);
  const left = await paneBox(page, VIEW.panes[0][1]);
  const grip = { x: left.x + left.w - 60, y: left.y + 8 };
  const centre = (b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });

  // Beyond the margin: the pane's id and its kind, once, and no move.
  await drag(page, grip, { x: width + 40, y: grip.y + 10 });
  assert.deepEqual(await sent(page, "release_outside"), [{ pane: 1, kind: "pane" }]);
  assert.equal(await page.locator(".drop-outline").count(), 0);
  assert.deepEqual(await sent(page, "move_pane"), []);

  // Just over the edge: a slip, cancelled like a drop on nothing. Escape outside cancels too.
  await reset(page, VIEW);
  await drag(page, grip, { x: width + 10, y: grip.y });
  assert.deepEqual(await sent(page, "release_outside"), [], "10 px over the edge is a slip");
  await drag(page, grip, { x: width + 60, y: grip.y }, { release: false });
  await page.keyboard.press("Escape");
  await page.mouse.up();
  assert.deepEqual(await sent(page, "release_outside"), [], "Escape outside cancels");

  // A tab: named by the pane that has its focus.
  await drag(page, centre(await page.locator("#bar .tab").nth(1).boundingBox()), { x: width + 50, y: 200 });
  assert.deepEqual(await sent(page, "release_outside"), [{ pane: 3, kind: "tab" }]);
  assert.deepEqual(await sent(page, "select_tab"), [], "the click that ends the drag does not select the tab");

  // The shell says the pane or tab cannot go (alone in its window): the page keeps everything.
  await reset(page, VIEW);
  await page.evaluate(() => window.__MOCK__.on("release_outside", () => ({ outcome: "cancelled" })));
  await drag(page, grip, { x: width + 40, y: 100 });
  assert.equal((await sent(page, "release_outside")).length, 1);
  assert.deepEqual(await sent(page, "start_pane"), [], "nothing was forgotten");
  assert.equal(await notice(page), "Pane stays here: it cannot get a window of its own", "the status line says why nothing happened");

  // The same for a tab, and for a window that could not be built (the shell answers an error).
  await reset(page, VIEW);
  await page.evaluate(() => window.__MOCK__.on("release_outside", () => ({ outcome: "cancelled" })));
  await drag(page, centre(await page.locator("#bar .tab").nth(1).boundingBox()), { x: width + 50, y: 200 });
  assert.equal(await notice(page), "Tab stays here: it cannot get a window of its own");
  await reset(page, VIEW);
  await page.evaluate(() => window.__MOCK__.on("release_outside", () => { throw "cannot open a window: the window system refused"; }));
  await drag(page, grip, { x: width + 40, y: 100 });
  assert.equal(await notice(page), "Pane not moved: cannot open a window: the window system refused");
  assert.deepEqual(await sent(page, "start_pane"), [], "nothing was forgotten");

  // A window that is going itself is refused with `no such window`: nothing is left to tell.
  await open(page, url);
  await page.evaluate(() => window.__MOCK__.on("release_outside", () => { throw "no such window"; }));
  await drag(page, grip, { x: width + 40, y: 100 });
  await page.waitForFunction(() => window.__MOCK__.callsOf("release_outside").length === 1);
  assert.equal(await notice(page), "", "no message for a window that is going");
}

/** The text of the notice segment of the status line (empty when there is none). */
const notice = (page) => page.locator("#status .seg.notice").evaluate((n) => n.textContent);

/** The palette actions give the focused pane or the shown tab a window beside this one. */
async function paletteCase(page, url) {
  await open(page, url);
  const run = async (text) => {
    await page.keyboard.press("Meta+k");
    await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
    await page.keyboard.type(text);
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector(".palette-backdrop")?.hidden === true);
  };
  await run("Move pane to new window");
  assert.deepEqual(await sent(page, "tear_off_pane"), [{ pane: 1, at_cursor: false }]);
  assert.deepEqual(await sent(page, "tear_off_tab"), []);
  await run("Move tab to new window");
  assert.deepEqual(await sent(page, "tear_off_tab"), [{ pane: 1, at_cursor: false }], "the shown tab, named by its focus pane");
  assert.equal((await sent(page, "tear_off_pane")).length, 1);

  // Refused (a lone pane): the page does not throw and keeps its panes.
  await page.evaluate(() => window.__MOCK__.on("tear_off_pane", () => { throw "a pane that is alone in its window cannot be torn off"; }));
  await run("Move pane to new window");
  assert.equal((await sent(page, "tear_off_pane")).length, 2);
  assert.ok(await page.locator(".panehead").count() >= 2, "the layout is as it was");
  assert.equal(await notice(page), "Pane not moved: a pane that is alone in its window cannot be torn off", "and the status line says why");
}

/**
 * A drag that is outside its window shows the outline of the zone in the window under the pointer, live, and takes it away
 * on every way a drag can end. Two pages: `page` is the window the drag started in, `second` is the window under the
 * pointer; the test plays the shell, which reads the pointer (here a point it is told) and sends `drag-hover` to the
 * window under it and `drag-end` to the one it left.
 */
async function hoverCase(page, url) {
  await open(page, url);
  const other = await newPage(page.context().browser());
  const b = other.page;
  try {
    await b.goto(`${url}index.html?renderer=canvas&window=window-2`);
    await b.waitForSelector(".divider");
    await b.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
    // The shell, between the two: `__POINT__` is the pointer in the second window's client coordinates, or null.
    await page.exposeFunction("__toSecond__", (name, payload) =>
      b.evaluate(([n, p]) => window.__MOCK__.emitTo("window-2", n, p), [name, payload]),
    );
    await page.evaluate(() => {
      window.__POINT__ = null;
      window.__MOCK__.on("drag_over", (a) => {
        if (window.__POINT__) return window.__toSecond__("drag-hover", { kind: a.kind, pane: a.pane, ...window.__POINT__ });
        return window.__toSecond__("drag-end", null);
      });
      window.__MOCK__.on("drag_end", () => window.__toSecond__("drag-end", null));
    });
    const point = (p) => page.evaluate((v) => { window.__POINT__ = v; }, p);
    const outlines = () => b.locator(".drop-outline").count();
    // Quicker than the second window's own timeout (1 s): it was told, it did not give up by itself.
    const gone = () => b.waitForFunction(() => document.querySelector(".drop-outline") === null, null, { timeout: 500 });
    const calls = () => page.evaluate(() => window.__MOCK__.calls.map(([cmd]) => cmd));
    const polls = () => page.evaluate(() => window.__MOCK__.callsOf("drag_over").length);
    const box = async () => b.locator(".drop-outline").boundingBox();
    const near = (got, want, why) => {
      for (const k of ["x", "y", "width", "height"]) assert.ok(Math.abs(got[k] - want[k]) < 2, `${why}: ${k} ${JSON.stringify(got)} against ${JSON.stringify(want)}`);
    };

    const width = await page.evaluate(() => window.innerWidth);
    const left = await paneBox(page, VIEW.panes[0][1]);
    const grip = { x: left.x + left.w - 60, y: left.y + 8 };
    // The second window shows the mock's default layout: panes 1 and 2 side by side, one workspace, one tab.
    const right2 = await paneBox(b, { x: 0.5, y: 0, w: 0.5, h: 1 });
    const bottomOf2 = { x: right2.x + right2.w / 2, y: right2.y + right2.h - 10 };

    // Inside the window nothing is asked; outside, past the margin, the second window shows the zone under the pointer.
    await point(bottomOf2);
    await drag(page, grip, { x: width / 2, y: 300 }, { release: false });
    assert.equal(await polls(), 0, "inside the window the shell is not asked");
    await page.mouse.move(width + 40, 300, { steps: 4 });
    await b.waitForSelector(".drop-outline");
    near(await box(), { x: right2.x, y: right2.y + right2.h / 2, width: right2.w, height: right2.h / 2 }, "the lower half of pane 2 of the other window");
    assert.deepEqual((await callsOf(page, "drag_over"))[0], { pane: 1, kind: "pane" }, "only a pane id and its kind");
    assert.equal(await page.locator(".drop-outline").count(), 0, "this window shows none: the pointer is not over it");

    // It is live: the pointer moves, the outline follows without a new event from the page's pointer.
    await point({ x: right2.x + right2.w - 10, y: right2.y + right2.h / 2 });
    await b.waitForFunction(() => { const r = document.querySelector(".drop-outline")?.getBoundingClientRect(); return r && r.x > 600; });
    near(await box(), { x: right2.x + right2.w / 2, y: right2.y, width: right2.w / 2, height: right2.h }, "the right half of pane 2");
    const chip2 = await b.locator("#bar .ws-item").first().boundingBox();
    await point({ x: chip2.x + chip2.width / 2, y: chip2.y + chip2.height / 2 });
    await b.waitForFunction(() => document.querySelector(".drop-outline")?.getBoundingClientRect().height < 40);
    near(await box(), chip2, "over a chip: the chip");
    await point(null);
    await gone();
    assert.equal(await outlines(), 0, "the pointer is over no window of the app: the outline goes");

    // It comes back and the pointer returns into the window past the margin of 24 px: the outline goes (end sent once).
    await point(bottomOf2);
    await b.waitForSelector(".drop-outline");
    const before = (await calls()).filter((c) => c === "drag_end").length;
    await page.mouse.move(width - 100, 300, { steps: 4 });
    await gone();
    assert.equal((await calls()).filter((c) => c === "drag_end").length, before + 1, "the end is sent once");
    await page.waitForTimeout(200);
    assert.equal(await outlines(), 0, "and nothing renews it while the pointer is inside");

    // Escape outside ends it, and the release that follows does nothing.
    await page.mouse.move(width + 60, 300, { steps: 3 });
    await b.waitForSelector(".drop-outline");
    await page.keyboard.press("Escape");
    await gone();
    await page.mouse.up();
    await page.waitForTimeout(250);
    assert.equal(await outlines(), 0, "Escape: no outline, and none comes back");
    assert.deepEqual(await sent(page, "release_outside"), [], "Escape: nothing is released");
    assert.deepEqual(await sent(page, "move_pane"), []);

    // A release outside, over the other window: the outline is gone before the window is asked where the pane goes.
    await reset(page, VIEW);
    await page.evaluate(() => window.__MOCK__.on("drag_over", (a) => window.__toSecond__("drag-hover", { kind: a.kind, pane: a.pane, ...window.__POINT__ })));
    await page.evaluate(() => window.__MOCK__.on("drag_end", () => window.__toSecond__("drag-end", null)));
    await drag(page, grip, { x: width + 60, y: 300 }, { release: false });
    await b.waitForSelector(".drop-outline");
    await page.mouse.up();
    await gone();
    await page.waitForFunction(() => window.__MOCK__.callsOf("release_outside").length === 1);
    const order = await calls();
    assert.ok(order.lastIndexOf("drag_over") < order.indexOf("release_outside"), `no hover after the release: ${order}`);
    assert.ok(order.indexOf("drag_end") < order.indexOf("release_outside"), "the end comes first");
    assert.deepEqual(await sent(page, "release_outside"), [{ pane: 1, kind: "pane" }]);
    await page.waitForTimeout(250);
    assert.equal(await outlines(), 0);

    // A tab: named by the pane that has its focus; it lands in a strip, so the strip is outlined (a chip: the chip).
    await reset(page, VIEW);
    await page.evaluate(() => window.__MOCK__.on("drag_over", (a) => window.__toSecond__("drag-hover", { kind: a.kind, pane: a.pane, ...window.__POINT__ })));
    await page.evaluate(() => window.__MOCK__.on("drag_end", () => window.__toSecond__("drag-end", null)));
    const tab = await page.locator("#bar .tab").nth(1).boundingBox();
    await drag(page, { x: tab.x + tab.width / 2, y: tab.y + tab.height / 2 }, { x: width + 50, y: 200 }, { release: false });
    await b.waitForSelector(".drop-outline");
    assert.deepEqual((await callsOf(page, "drag_over")).at(-1), { pane: 3, kind: "tab" });
    near(await box(), await b.locator("#bar").boundingBox(), "a tab over a pane: the strip");
    await page.keyboard.press("Escape");
    await page.mouse.up();
    await gone();

    // The window the drag started in sends nothing the terminal could hear, and the other one moved nothing.
    for (const cmd of ["key", "text", "mouse", "select", "paste", "move_pane", "move_tab", "attach_pane", "attach_tab"]) {
      assert.deepEqual(await callsOf(b, cmd), [], `${cmd} was not sent by the other window`);
    }

    // A window that hears nothing more takes the outline away by itself (the other page crashed, an event was lost).
    await emit(b, "drag-hover", { kind: "pane", pane: 1, x: bottomOf2.x, y: bottomOf2.y });
    assert.equal(await outlines(), 1);
    await b.waitForFunction(() => document.querySelector(".drop-outline") === null, null, { timeout: 3000 });

    // Over no zone, or with a payload that is not one, there is no outline and no error; a drop answers and clears it.
    for (const junk of [null, 7, "x", { kind: "window", pane: 1, x: 5, y: 5 }, { kind: "pane", pane: "1", x: 5, y: 5 }, { kind: "pane", pane: 1, x: Number.NaN, y: 5 }, { kind: "pane", pane: 1, x: -50, y: -50 }]) {
      await emit(b, "drag-hover", junk);
      assert.equal(await outlines(), 0, JSON.stringify(junk));
    }
    await emit(b, "drag-hover", { kind: "pane", pane: 1, x: bottomOf2.x, y: bottomOf2.y });
    assert.equal(await outlines(), 1);
    await emit(b, "incoming-drop", { kind: "pane", pane: 1, x: bottomOf2.x, y: bottomOf2.y });
    assert.equal(await outlines(), 0, "the drop itself clears the preview");
    await emit(b, "drag-hover", { kind: "pane", pane: 1, x: bottomOf2.x, y: bottomOf2.y });
    await emit(b, "drag-end", null);
    assert.equal(await outlines(), 0);
    assert.deepEqual(other.errors, [], "the second window reported errors");
  } finally {
    await b.close().catch(() => {});
  }
}

/** The notice goes by itself, a newer one replaces it, and it is announced as a status. */
async function noticeCase(page, url) {
  await page.clock.install({ time: new Date(2026, 0, 5, 10, 15, 40) });
  await open(page, url);
  const run = async (text) => {
    await page.keyboard.press("Meta+k");
    await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
    await page.keyboard.type(text);
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector(".palette-backdrop")?.hidden === true);
  };
  await page.evaluate(() => window.__MOCK__.on("tear_off_tab", () => { throw "this cannot be a window of its own"; }));
  assert.equal(await notice(page), "", "no notice before anything was refused");
  await run("Move tab to new window");
  assert.equal(await notice(page), "Tab not moved: this cannot be a window of its own");
  assert.equal(await page.locator("#status .seg.notice").getAttribute("role"), "status");
  await page.clock.fastForward(4000);
  assert.match(await notice(page), /^Tab not moved/, "still there after four seconds");
  await page.clock.fastForward(1500);
  assert.equal(await notice(page), "", "gone by itself");
  assert.equal(await page.locator("#status .seg.notice").isVisible(), false, "an empty segment takes no room");
}

/** A pane that leaves is forgotten and is a first appearance when it comes back; a pane in another tab is not. */
async function forgetCase(page, url) {
  await open(page, url);
  const starts = async (pane) => (await sent(page, "start_pane")).filter((a) => a.pane === pane).length;
  const fulls = async (pane) => (await sent(page, "request_full")).filter((a) => a.pane === pane).length;
  const right = await paneBox(page, VIEW.panes[1][1]);
  const c = await page.locator("canvas").first().boundingBox();
  const at = { x: right.x - c.x + right.w / 2, y: 20 + 12 }; // inside pane 2's grid, below its header

  // Pane 2 has a mirror, painted blue.
  await emit(page, "frame", fullFrame(2, 1, PANE_2_BG));
  assert.equal(await pixel(page, at.x, at.y), PANE_2_BG, "pane 2 is drawn from its mirror");
  await page.evaluate(() => window.__MOCK__.on("release_outside", () => ({ outcome: "torn_off", label: "window-2" })));

  // Tab switch: pane 2 is not in the view of tab "two" but it did not leave. Nothing is forgotten.
  const other = viewOf([3], 2, { tabs: VIEW.tabs.map((t, i) => ({ ...t, active: i === 1 })) });
  await emit(page, "layout", other);
  await emit(page, "layout", viewOf([1, 2], 3));
  assert.equal(await starts(2), 1, "a pane of another tab keeps its record: no second start");
  assert.equal(await pixel(page, at.x, at.y), PANE_2_BG, "and its mirror");

  // Release pane 2 outside the window, then the layout without it arrives: forgotten.
  const width = await page.evaluate(() => window.innerWidth);
  await drag(page, { x: right.x + right.w - 60, y: right.y + 8 }, { x: width + 50, y: 100 });
  assert.deepEqual(await sent(page, "release_outside"), [{ pane: 2, kind: "pane" }]);
  await emit(page, "layout", viewOf([1], 4));
  // A frame that was on its way to this window when the pane left is dropped, not stored.
  await emit(page, "frame", fullFrame(2, 9, 0xaa3366));
  await emit(page, "title", { pane: 2, title: "left behind" });

  // It comes back (another window handed it over): a first appearance, not the old mirror.
  await emit(page, "layout", viewOf([1, 2], 5));
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").filter((a) => a.pane === 2).length >= 2);
  assert.equal(await starts(2), 2, "start_pane again: the size record was forgotten");
  await page.waitForFunction(() => window.__MOCK__.callsOf("request_full").filter((a) => a.pane === 2).length >= 2);
  assert.equal(await fulls(2), 2, "and a full frame is asked for");
  assert.notEqual(await pixel(page, at.x, at.y), PANE_2_BG, "the old mirror is gone");
  assert.notEqual(await pixel(page, at.x, at.y), 0xaa3366, "and the frame that arrived late was dropped");
  assert.notEqual(await page.locator('.panehead[data-pane="2"] .ph-title').textContent(), "left behind", "so was the title");
  await emit(page, "frame", fullFrame(2, 1, 0x336699));
  assert.equal(await pixel(page, at.x, at.y), 0x336699, "the full frame of the new window draws as any frame does");
  assert.equal(await starts(1), 1, "the pane that stayed was never touched");

  // A cancelled release does not forget: the shell kept the pane in the window.
  await reset(page, VIEW);
  await page.evaluate(() => window.__MOCK__.on("release_outside", () => ({ outcome: "cancelled" })));
  await drag(page, { x: right.x + right.w - 60, y: right.y + 8 }, { x: width + 50, y: 100 });
  await page.waitForFunction(() => window.__MOCK__.callsOf("release_outside").length === 1);
  await emit(page, "layout", viewOf([1], 6)); // e.g. the pane went to another tab of this window
  await emit(page, "layout", viewOf([1, 2], 7));
  assert.equal(await starts(2), 0, "a pane the shell kept is not forgotten (the calls were cleared above: no new start)");
}

/** A drop that comes from another window is answered with the zone under the pointer; nothing under it, no answer. */
async function incomingCase(page, url) {
  await open(page, url);
  const right = await paneBox(page, VIEW.panes[1][1]);
  const left = await paneBox(page, VIEW.panes[0][1]);
  const c = await page.locator("canvas").first().boundingBox();
  const drop = (kind, point, pane = 9) => emit(page, "incoming-drop", { kind, pane, x: point.x, y: point.y });
  const centre = (b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });
  const answers = async () => (await page.evaluate(() => window.__MOCK__.calls.map(([cmd, args]) => [cmd, args]))).filter(([cmd]) => cmd.startsWith("attach_"));

  // A pane onto the bottom edge of pane 2: splits it there. Coordinates are this window's client coordinates.
  await drop("pane", { x: right.x + right.w / 2, y: right.y + right.h - 10 });
  await page.waitForFunction(() => window.__MOCK__.callsOf("attach_pane").length === 1);
  assert.deepEqual(await answers(), [["attach_pane", { pane: 9, target: 2, edge: "bottom" }]]);

  // Onto the middle: a swap; onto a tab: next to that tab's pane; onto a chip: a tab of its own in that workspace.
  await reset(page, VIEW);
  await drop("pane", centre({ x: left.x, y: left.y, width: left.w, height: left.h }));
  await drop("pane", centre(await page.locator("#bar .tab").nth(1).boundingBox()));
  await drop("pane", centre(await page.locator("#bar .ws-chip").nth(1).boundingBox()));
  await page.waitForFunction(() => window.__MOCK__.callsOf("attach_pane").length === 3);
  assert.deepEqual(await answers(), [
    ["attach_pane", { pane: 9, target: 1, edge: "center" }],
    ["attach_pane", { pane: 9, target: 3, edge: "right" }],
    ["attach_pane", { pane: 9, workspace: 1 }],
  ]);

  // A tab: into the workspace of the chip, or into the shown workspace anywhere else.
  await reset(page, VIEW);
  await drop("tab", centre(await page.locator("#bar .ws-chip").nth(1).boundingBox()), 11);
  await drop("tab", { x: right.x + 30, y: right.y + 30 }, 11);
  await page.waitForFunction(() => window.__MOCK__.callsOf("attach_tab").length === 2);
  assert.deepEqual(await answers(), [
    ["attach_tab", { pane: 11, workspace: 1 }],
    ["attach_tab", { pane: 11, workspace: 0 }],
  ]);

  // Over nothing (the status line, outside the window, junk): no answer at all.
  await reset(page, VIEW);
  const status = await page.locator("#status, .status").first().boundingBox().catch(() => null);
  if (status) await drop("pane", centre({ x: status.x, y: status.y, width: status.width, height: status.height }));
  await drop("pane", { x: -50, y: -50 });
  await drop("tab", { x: c.x + c.width + 500, y: 4000 });
  await emit(page, "incoming-drop", { kind: "folder", pane: 9, x: 10, y: 10 });
  await emit(page, "incoming-drop", { kind: "pane", pane: "9", x: 10, y: 10 });
  await emit(page, "incoming-drop", { kind: "pane", pane: 9, x: Number.NaN, y: 10 });
  await emit(page, "incoming-drop", null);
  await page.waitForTimeout(100);
  assert.deepEqual(await answers(), [], "a miss sends nothing");

  // A swap sends pane 2 to the other window: this page forgets it, and a pane it never held is just a new one.
  await reset(page, VIEW);
  const starts = async (pane) => (await sent(page, "start_pane")).filter((a) => a.pane === pane).length;
  const swapped = viewOf([1, 9], 8);
  await page.evaluate((v) => window.__MOCK__.on("attach_pane", () => v), swapped); // the answer: the view with the swap done
  await drop("pane", centre({ x: right.x, y: right.y, width: right.w, height: right.h }));
  await page.waitForFunction(() => window.__MOCK__.callsOf("attach_pane").length === 1);
  await emit(page, "layout", swapped);
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").some((a) => a.pane === 9));
  assert.equal(await starts(9), 1, "the pane that arrived goes through the first-appearance path");
  await page.waitForFunction(() => window.__MOCK__.callsOf("request_full").some((a) => a.pane === 9));
  const before = await starts(2);
  await emit(page, "layout", viewOf([1, 2], 9));
  await page.waitForFunction((n) => window.__MOCK__.callsOf("start_pane").filter((a) => a.pane === 2).length > n, before);
  assert.equal(await starts(2), before + 1, "the swapped-out pane is a first appearance when it returns");
}
