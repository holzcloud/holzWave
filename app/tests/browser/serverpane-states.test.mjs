// The states of a pane that shows a session of a server, the older lines on scroll, the grid rule and the phone (docs/design.md 6.34).
// The stub plays the host: its `remoteSessions` hook puts the remote member into the answer of `pane_connections`, and frames are
// delivered as the core's `frame` event.
import assert from "node:assert/strict";
import { callsOf, currentBackend, emit } from "./lib.mjs";
import { open as openPhone } from "./touch-lib.mjs";

// Both builds: the browser's worker shows a session in a pane of its window with the same states. The phone is the Android app's
// (the browser build has no phone layout), so that case runs on the desktop backend only.
export const backends = ["tauri", "ws"];

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await statesCase(page, url);
  await olderCase(page, url);
  await gridCase(page, url);
  if (currentBackend() === "tauri") await phoneCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const MARK = 0x20c040;
const head = (page, pane) => page.locator(`.panehead[data-pane="${pane}"]`);

async function open(page, url, panes) {
  await page.addInitScript((p) => {
    const sessions = window.__MOCK__.remoteSessions;
    sessions.reset();
    for (const [pane, partial] of Object.entries(p)) sessions.setPane(Number(pane), partial);
  }, panes);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

const setPane = (page, pane, partial) => page.evaluate(([p, x]) => window.__MOCK__.remoteSessions.setPane(p, x), [pane, partial]);
const ctxIs = (page, pane, text) =>
  page.waitForFunction(([p, t]) => document.querySelector(`.panehead[data-pane="${p}"] .ph-ctx`)?.textContent === t, [pane, text], { timeout: 4000 });

/** The size of the tile of `pane` in cells, as the page told the core. */
const tile = (page, pane) =>
  page.evaluate(async (id) => (await import(new URL("state.js", location.href).href)).state.sizes.get(id), pane);

/** A full frame of `pane`: `cols` by `rows`, every row painted in `MARK` when `mark`, scrolled to `offset` of `total` history rows. */
const frame = (pane, seq, cols, rows, { mark = false, offset = 0, total = rows, bg = 0x101010 } = {}) => ({
  pane, seq, full: true, cols, rows, fg: 0xf6efe6, bg, cursor: null, scrollbar: [offset, total, rows], selection: [], matches: [], search: null,
  copy_cursor: null, modes: { app_cursor: false, app_keypad: false, bracketed_paste: false, mouse: false },
  changed: mark ? Array.from({ length: rows }, (_, y) => ({ y, runs: [[" ".repeat(cols), cols, 0xf6efe6, MARK, 0]] })) : [],
});

async function statesCase(page, url) {
  await open(page, url, { 2: { remote: { writer: "writing", cols: 80, rows: 24 } } });
  const sizes = await tile(page, 2);
  await setPane(page, 2, { remote: { cols: sizes.cols, rows: sizes.rows } });
  await ctxIs(page, 2, "sync.example.test");
  const look = () =>
    head(page, 2).evaluate((h) => ({
      dot: h.querySelector(".ph-dot").dataset.state,
      tone: h.querySelector(".ph-ctx").dataset.tone ?? null,
      label: h.querySelector(".ph-writer")?.textContent ?? null,
      mark: getComputedStyle(h.querySelector(".ph-ctx > span"), "::before").content,
      underline: getComputedStyle(h.querySelector(".ph-ctx")).textDecorationLine,
      dotBorder: getComputedStyle(h.querySelector(".ph-dot")).borderTopStyle,
    }));
  const cases = [
    [{ state: "connecting", remote: { phase: "" } }, "Attaching…", { dot: "wait", tone: null, label: null, dotBorder: "dotted" }],
    [{ state: "connected", remote: { phase: "attaching" } }, "Attaching…", { dot: "wait", label: null }],
    [{ state: "connected", remote: { phase: "loading_older" } }, "Loading older lines…", { tone: null, label: "WRITING" }],
    [{ state: "connected", remote: { phase: "resyncing" } }, "Resyncing…", { dot: "wait", label: "WRITING" }],
    [{ state: "connected", remote: { phase: "input_dropped" } }, "Input dropped", { tone: "warn", label: "WRITING", underline: "underline" }],
    [{ state: "reconnecting", attempt: 2, of: 5, secs: 3, remote: { phase: "" } }, "Reconnecting…", { dot: "wait", tone: null, label: null }],
    [{ state: "failed", remote: { phase: "" } }, "Server unreachable", { dot: "failed", tone: "warn", label: null, underline: "underline" }],
    [{ state: "closed", remote: { phase: "" } }, "Session ended", { dot: "failed", tone: "warn", label: null, underline: "underline" }],
    [{ state: "connected", remote: { phase: "exploding" } }, "sync.example.test", { tone: null, label: "WRITING" }],
  ];
  for (const [partial, word, want] of cases) {
    await setPane(page, 2, partial);
    await ctxIs(page, 2, word);
    const got = await look();
    for (const [key, value] of Object.entries(want)) assert.equal(got[key], value, `${word}: ${key}`);
    if (want.tone === "warn") assert.match(got.mark, /!/, `${word} carries the mark !`);
  }
  // The tries of a reconnect are in the title; the context says the record's word.
  await setPane(page, 2, { state: "reconnecting", attempt: 2, of: 5, secs: 3, remote: { phase: "" } });
  await ctxIs(page, 2, "Reconnecting…");
  assert.equal(await head(page, 2).locator(".ph-ctx").getAttribute("title"), "Reconnecting… 2/5 in 3 s");

  // Forced colours, higher contrast and reduced motion keep the words and the shapes.
  for (const media of [{ forcedColors: "active" }, { contrast: "more" }, { reducedMotion: "reduce" }]) {
    await page.emulateMedia(media);
    const got = await look();
    assert.equal(got.dotBorder, "dotted", `the dotted ring is a static ring under ${JSON.stringify(media)}`);
    assert.equal(await head(page, 2).locator(".ph-ctx").textContent(), "Reconnecting…");
    const moving = await head(page, 2).evaluate((h) => [h, ...h.querySelectorAll("*")].some((n) => getComputedStyle(n).animationName !== "none"));
    assert.equal(moving, false, "nothing moves");
  }
  await page.emulateMedia({ forcedColors: "none", contrast: "no-preference", reducedMotion: "no-preference" });
}

async function olderCase(page, url) {
  await open(page, url, { 2: { remote: { writer: "watching", more: true } } });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-writer')?.textContent === "WATCHING");
  const size = await tile(page, 2);
  let seq = 1;
  const send = (offset) => emit(page, "frame", frame(2, seq++, size.cols, size.rows, { offset, total: 3000 }));
  const asks = async () => (await callsOf(page, "pane_load_older")).length;
  const scrolls = async () => (await callsOf(page, "scroll")).length;
  await send(200);
  assert.equal(await asks(), 0, "not at the top: nothing is asked");
  const scrolled = await scrolls();
  // The person reached the top: one ask, and a held key at the top asks no more.
  for (let i = 0; i < 6; i++) await send(0);
  assert.deepEqual(await callsOf(page, "pane_load_older"), [{ pane: 2 }], "one ask at the top");
  // The host says what the pane is busy with; the header says so.
  await setPane(page, 2, { remote: { phase: "loading_older", more: true } });
  await ctxIs(page, 2, "Loading older lines…");
  await send(0);
  // The answer came with the older lines above: the host kept the distance from the bottom, the page did not scroll on its own.
  await setPane(page, 2, { remote: { phase: "", more: true } });
  await headCtx(page);
  await send(1000);
  assert.equal(await scrolls(), scrolled, "the page sends no scroll of its own");
  assert.equal(await asks(), 1);
  await send(0);
  assert.equal(await asks(), 2, "at the top again with more: a second ask");
  // When the server has nothing older the page stops asking.
  await setPane(page, 2, { remote: { phase: "", more: false } });
  await page.waitForTimeout(1300);
  await send(500);
  await send(0);
  assert.equal(await asks(), 2, "no ask once more is false");
}

/** The context text of pane 2 once it is no longer a state word. */
async function headCtx(page) {
  await page.waitForFunction(() => {
    const t = document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent ?? "";
    return t.startsWith("sync.example.test");
  }, null, { timeout: 4000 });
  return head(page, 2).locator(".ph-ctx").textContent();
}

/** The colour of one canvas pixel at client coordinates as 0xRRGGBB. */
const pixel = (page, x, y) =>
  page.evaluate(([px, py]) => {
    const canvas = document.getElementById("term");
    const r = canvas.getBoundingClientRect();
    const d = canvas.getContext("2d").getImageData(Math.floor(px - r.left), Math.floor(py - r.top), 1, 1).data;
    return (d[0] << 16) | (d[1] << 8) | d[2];
  }, [x, y]);

/** The grid rectangle of `pane` in client pixels, from the page's own metrics and rules. */
const grid = (page, pane) =>
  page.evaluate(async (id) => {
    const { state } = await import(new URL("state.js", location.href).href);
    const { gridRect, paneRect } = await import(new URL("geometry.js", location.href).href);
    const c = document.getElementById("term").getBoundingClientRect();
    const unit = state.view.panes.find(([p]) => p === id)[1];
    const g = gridRect(state.metrics, unit, state.headerPx);
    const p = paneRect(state.metrics, unit);
    return { left: c.left + g.x, top: c.top + g.y, right: c.left + g.x + g.w, bottom: c.top + g.y + g.h, paneTop: c.top + p.y, cw: state.metrics.cellW, ch: state.metrics.cellH };
  }, pane);

async function gridCase(page, url) {
  await open(page, url, { 1: { remote: { writer: "watching", cols: 120, rows: 40 } } });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="1"] .ph-writer')?.textContent === "WATCHING");
  const small = await tile(page, 1);
  assert.ok(small.cols < 120 && small.rows < 40, `the tile is smaller than the session (${small.cols}×${small.rows})`);
  const resizesBefore = (await callsOf(page, "resize")).length;
  // A larger grid in a smaller tile: drawn from the top-left and clipped by the tile; the neighbour and the header are untouched.
  await emit(page, "frame", frame(2, 1, 40, 10, { bg: 0x303030 }));
  await emit(page, "frame", frame(1, 1, 120, 40, { mark: true }));
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  const g1 = await grid(page, 1);
  const g2 = await grid(page, 2);
  assert.equal(await pixel(page, g1.left + 1, g1.top + 1), MARK, "the top-left cell is the session's first cell");
  assert.equal(await pixel(page, g1.right - 2, g1.bottom - 2), MARK, "the tile is filled to its edge");
  assert.equal(await pixel(page, g2.left + 2, g2.top + 2), 0x303030, "nothing is drawn past the tile into the neighbour");
  assert.notEqual(await pixel(page, g1.left + 2, g1.paneTop + 2), MARK, "nothing is drawn under the header");
  const scrollable = await page.evaluate(() => document.scrollingElement.scrollWidth > innerWidth || document.scrollingElement.scrollHeight > innerHeight);
  assert.equal(scrollable, false, "no scrollbar");
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="1"] .ph-ctx')?.textContent === "sync.example.test · 120×40");
  // A smaller grid in the same tile: drawn from the top-left over the empty ground of the pane.
  await setPane(page, 1, { remote: { cols: 30, rows: 8 } });
  await emit(page, "frame", frame(1, 2, 30, 8, { mark: true }));
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  assert.equal(await pixel(page, g1.left + 1, g1.top + 1), MARK);
  assert.equal(await pixel(page, g1.left + 30 * g1.cw - 2, g1.top + 8 * g1.ch - 2), MARK, "the last cell of the session");
  assert.equal(await pixel(page, g1.left + 30 * g1.cw + 3, g1.top + 2), 0x101010, "right of the grid: the empty ground");
  assert.equal(await pixel(page, g1.left + 2, g1.top + 8 * g1.ch + 3), 0x101010, "under the grid: the empty ground");
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="1"] .ph-ctx')?.textContent === "sync.example.test · 30×8");
  // The frame set the drawn grid; the page's own resize of the tile is the same command as for every pane, with the tile's cells.
  await page.setViewportSize({ width: 900, height: 600 });
  await page.waitForFunction((n) => window.__MOCK__.callsOf("resize").length > n, resizesBefore);
  const last = (await callsOf(page, "resize")).filter((a) => a.pane === 1).at(-1);
  const now = await tile(page, 1);
  assert.deepEqual([last.cols, last.rows], [now.cols, now.rows], "the page asks for the tile's size; the host decides the session's");
  assert.deepEqual(Object.keys(last).sort(), ["cell_h", "cell_w", "cols", "pane", "rows"], "the resize command is unchanged");
  await page.setViewportSize({ width: 1000, height: 640 });
}

async function phoneCase(page, url) {
  await page.addInitScript(() => {
    const sessions = window.__MOCK__.remoteSessions;
    sessions.reset();
    sessions.set([{ id: 1, title: "cargo build", cwd: "/home/holz/holzWave", cols: 120, rows: 40, started: 1_700_000_000, attached: 1, held: true }]);
    const link = window.__MOCK__.serverLink;
    link.reset();
    link.set({ state: "signed_in", url: "https://holz.example.net", name: "Pixel", locked: false, sync_on: false, remote_on: true, error: null });
  });
  await openPhone(page, url, { width: 360, height: 760, screen: [360, 760] });
  const focus = await page.evaluate(() => window.__MOCK__.view().focus);
  await setPane(page, focus, { remote: { writer: "watching" } });
  await page.waitForFunction(() => document.querySelector("#phone-chip .ph-writer")?.textContent === "WATCHING");
  const strip = await page.evaluate(() => {
    const top = document.getElementById("phone-top").getBoundingClientRect();
    const take = document.querySelector("#phone-chip .ph-take");
    const label = document.querySelector("#phone-chip .ph-writer");
    const t = take.getBoundingClientRect();
    const l = label.getBoundingClientRect();
    return {
      take: t.height,
      boxes: JSON.stringify({ top: [top.left, top.right, top.top, top.bottom], t: [t.left, t.right, t.top, t.bottom], l: [l.left, l.right], sw: [take.scrollWidth, take.clientWidth, label.scrollWidth, label.clientWidth], o: [document.scrollingElement.scrollWidth, innerWidth] }),
      inside: t.right <= top.right + 0.5 && l.left >= top.left && t.bottom <= top.bottom + 0.5,
      whole: take.scrollWidth <= take.clientWidth + 1 && label.scrollWidth <= label.clientWidth + 1,
      overflow: document.scrollingElement.scrollWidth > innerWidth,
    };
  });
  assert.ok(strip.take >= 44, `Take over is 44 px on a phone (${strip.take})`);
  assert.ok(strip.inside && strip.whole && !strip.overflow, `the label and the button fit the top strip ${strip.boxes}`);

  // The Connections view is a full-screen sheet with rows of at least 44 px.
  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("connections.open"));
  await page.waitForSelector(".conns .cx-sess");
  const sheet = await page.evaluate(() => {
    const box = document.querySelector(".dlg.conns").getBoundingClientRect();
    const heights = [".cx-srv", ".cx-sess", ".cx-srv-acts button", ".cx-sess-acts button"].map((s) => document.querySelector(`.conns ${s}`).getBoundingClientRect().height);
    const sess = document.querySelector(".conns .cx-sess");
    const title = sess.querySelector(".cx-sess-title").getBoundingClientRect();
    const writer = sess.querySelector(".cx-sess-writer").getBoundingClientRect();
    const meta = sess.querySelector(".cx-sess-meta").getBoundingClientRect();
    return {
      full: Math.round(box.width) === innerWidth && Math.round(box.height) === innerHeight,
      heights,
      firstLine: Math.abs(title.top - writer.top) < 4 && meta.top >= title.bottom - 1,
      overflow: document.scrollingElement.scrollWidth > innerWidth,
    };
  });
  assert.ok(sheet.full, "a full-screen sheet");
  for (const h of sheet.heights) assert.ok(h >= 44, `rows and buttons are at least 44 px (${sheet.heights})`);
  assert.ok(sheet.firstLine, "the title and held or free on the first line, the rest under it");
  assert.equal(sheet.overflow, false, "nothing overflows");
  // The focus order of the sessions list is the visual order.
  const order = await page.evaluate(() => Array.from(document.querySelectorAll(".conns .cx-server button")).map((b) => b.textContent));
  assert.deepEqual(order, ["Sign out", "New session", "Attach", "End"]);
  await page.keyboard.press("Escape");
}
