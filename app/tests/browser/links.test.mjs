// Clickable URLs and paths and hint mode (DESK-21): targets are found in the visible rows of the mirror, Cmd+click
// (Ctrl+click off macOS) opens a URL or reveals a path through the core and never starts a selection, and hint mode
// puts a letter on every target. The page sends only what it matched; the core re-checks it (plan 23).
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const HEADER = 24; // CSS px of a pane's header strip at the base font size
const BG = 0x14091a;
const MODES = { alt: false, bracketed_paste: false, mouse: false };
const COLS = 40;

const URL_TEXT = "https://example.com/a?b=1";
const ROWS = [
  `see ${URL_TEXT} now`,
  "edit ./src/main.rs:12:3 ok",
  "commit 3f2a9c1 done",
  "nothing to click here",
];

const rowMsg = (y, text) => ({ y, runs: [[text.padEnd(COLS), COLS, 0xf6efe6, BG, 0]] });
const frame = (pane, seq, rows, over = {}) => ({
  pane, seq, full: true, cols: COLS, rows: rows.length, fg: 0xf6efe6, bg: BG, cursor: null, scrollbar: [0, rows.length, rows.length],
  selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed: rows.map((t, y) => rowMsg(y, t)), ...over,
});

/** Open the page as a macOS app, with `rows` in both panes; returns the geometry helpers. */
async function open(page, url, { rows = ROWS, os = "macos", modes = MODES } = {}) {
  await page.addInitScript((o) => window.__MOCK__.on("app_info", () => ({ os: o, version: "t", hostname: "h" })), os);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
  const start = (await callsOf(page, "start_pane"))[0];
  const c = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });
  const cw = start.cell_w;
  const ch = start.cell_h;
  const origin = (pane) => c.left + (pane === 1 ? 0 : c.width / 2) + PAD;
  await emit(page, "frame", frame(1, 1, rows, { modes }));
  await emit(page, "frame", frame(2, 1, rows, { modes }));
  return {
    cw, ch,
    pt: (pane, cx, cy) => [origin(pane) + (cx + 0.5) * cw, c.top + HEADER + (cy + 0.5) * ch],
    box: (pane, y, x0, x1) => ({ x: origin(pane) + x0 * cw, y: c.top + HEADER + y * ch, w: (x1 - x0 + 1) * cw, h: ch }),
  };
}

const reset = (page) => page.evaluate(() => window.__MOCK__.reset());
const names = (page, ...cmds) =>
  page.evaluate((set) => window.__MOCK__.calls.filter(([c]) => set.includes(c)).map(([c, a]) => [c, a]), cmds);
const frames = (page, n = 2) =>
  page.evaluate(async (count) => {
    for (let i = 0; i < count; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  }, n);

/** Hold `mod`, click at (x, y), release. */
async function modClick(page, mod, [x, y]) {
  await page.keyboard.down(mod);
  await page.mouse.click(x, y);
  await page.keyboard.up(mod);
}

/** Record what the clipboard path is asked to copy. */
const spyClipboard = (page) =>
  page.evaluate(() => {
    window.__exec = [];
    window.__written = [];
    document.execCommand = (cmd) => {
      const a = document.activeElement;
      window.__exec.push([cmd, a && a.tagName, a && a.value]);
      return true;
    };
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: async (t) => { window.__written.push(t); } },
    });
  });
const copied = (page) => page.evaluate(() => window.__exec.map((e) => e[2]).concat(window.__written));

export default async function ({ page, url, errors, engine }) {
  await clickCase(page, url);
  await hoverCase(page, url);
  await ctrlCase(page, url);
  await hintCase(page, url);
  await manyHintsCase(page, url);
  await noTargetCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Cmd+click opens the URL and reveals the path; a plain click selects as before; Cmd+click starts no selection. */
async function clickCase(page, url) {
  const g = await open(page, url);
  await emit(page, "cwd", { pane: 2, cwd: "/home/u/proj" });
  await reset(page);

  await modClick(page, "Meta", g.pt(2, 10, 0));
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: URL_TEXT }], "Cmd+click on a URL opens exactly that URL");
  assert.deepEqual((await names(page, "select", "mouse", "focus_pane")), [], "no selection, no report, no focus change");

  await modClick(page, "Meta", g.pt(2, 8, 1));
  assert.deepEqual(
    await callsOf(page, "reveal_path"),
    [{ path: "./src/main.rs", base: "/home/u/proj" }],
    "Cmd+click on a path reveals it, with the pane's working directory as the base and no :line:col",
  );
  assert.equal((await callsOf(page, "open_url")).length, 1, "a path is not opened");

  // The suffix is part of what is clickable.
  await modClick(page, "Meta", g.pt(2, 22, 1)); // the "3" of ":12:3": the path starts at column 5 and is 18 cells
  assert.equal((await callsOf(page, "reveal_path")).length, 2, "the :line:col suffix is clickable too");

  // A pane that reported no directory has no base.
  await reset(page);
  await modClick(page, "Meta", g.pt(1, 8, 1));
  assert.deepEqual(await callsOf(page, "reveal_path"), [{ path: "./src/main.rs", base: null }]);

  // A hash and plain text are not links: Cmd+click there is an ordinary press.
  await reset(page);
  await modClick(page, "Meta", g.pt(2, 10, 2));
  await frames(page);
  assert.deepEqual([(await callsOf(page, "open_url")).length, (await callsOf(page, "reveal_path")).length], [0, 0]);

  // A plain click on the same text selects.
  await reset(page);
  await page.waitForTimeout(450);
  await page.mouse.click(...g.pt(2, 10, 0));
  await frames(page);
  assert.deepEqual((await callsOf(page, "select")).map((a) => a.op.kind), ["begin", "clear"], "a plain click selects as before");
  assert.equal((await callsOf(page, "open_url")).length, 0, "a plain click opens nothing");
}

/** Holding the modifier underlines the target under the pointer and shows a pointer cursor; releasing removes it. */
async function hoverCase(page, url) {
  const g = await open(page, url);
  const hovered = () =>
    page.evaluate(() => {
      const n = document.querySelector("#overlay .linkhover");
      if (!n) return null;
      const r = n.getBoundingClientRect();
      const s = getComputedStyle(n);
      return { x: r.left, y: r.top, w: r.width, h: r.height, underline: parseFloat(s.borderBottomWidth) };
    });
  const cursor = () => page.evaluate(() => getComputedStyle(document.getElementById("term")).cursor);
  const near = (got, want, what) => {
    for (const k of ["x", "y", "w", "h"]) assert.ok(Math.abs(got[k] - want[k]) < 0.6, `${what}: ${k} ${got[k]} != ${want[k]}`);
  };

  await page.mouse.move(...g.pt(2, 10, 0));
  assert.equal(await hovered(), null, "no modifier, no underline");
  assert.notEqual(await cursor(), "pointer");

  await page.keyboard.down("Meta");
  await page.waitForFunction(() => document.querySelector("#overlay .linkhover") !== null);
  const url0 = "https://example.com/a?b=1";
  const start = "see ".length;
  const h = await hovered();
  near(h, g.box(2, 0, start, start + url0.length - 1), "the underline covers the URL");
  assert.ok(h.underline >= 1, "the cue is a line, not a colour");
  assert.equal(await cursor(), "pointer");

  // Moving to text that is not a target removes it; moving back brings it.
  await page.mouse.move(...g.pt(2, 0, 3));
  assert.equal(await hovered(), null, "plain text is not underlined");
  assert.notEqual(await cursor(), "pointer");
  await page.mouse.move(...g.pt(2, 8, 1));
  await page.waitForFunction(() => document.querySelector("#overlay .linkhover") !== null);
  near(await hovered(), g.box(2, 1, 5, 5 + "./src/main.rs:12:3".length - 1), "the path with its suffix");

  // Releasing the modifier removes it without a pointer event; pressing it again brings it back.
  await page.keyboard.up("Meta");
  await page.waitForFunction(() => document.querySelector("#overlay .linkhover") === null);
  assert.notEqual(await cursor(), "pointer");
  await page.keyboard.down("Meta");
  await page.waitForFunction(() => document.querySelector("#overlay .linkhover") !== null);
  await page.keyboard.up("Meta");
}

/** Off macOS the modifier is Ctrl, and Cmd does nothing. */
async function ctrlCase(page, url) {
  const g = await open(page, url, { os: "linux" });
  await reset(page);
  await modClick(page, "Control", g.pt(2, 10, 0));
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: URL_TEXT }], "Ctrl+click opens on linux");
  await reset(page);
  await page.waitForTimeout(450);
  await modClick(page, "Meta", g.pt(2, 10, 0));
  assert.equal((await callsOf(page, "open_url")).length, 0, "Cmd+click is not the link modifier on linux");
}

/** hints.open labels every target of the focused pane; a letter copies, Shift opens, Escape leaves. */
async function hintCase(page, url) {
  const g = await open(page, url);
  await emit(page, "cwd", { pane: 2, cwd: "/home/u/proj" });
  await spyClipboard(page);
  const labels = () =>
    page.evaluate(() => [...document.querySelectorAll("#overlay [data-hint]")].map((n) => [n.dataset.hint, n.textContent]));
  const hintBoxes = () =>
    page.evaluate(() =>
      [...document.querySelectorAll("#overlay [data-hint]")].map((n) => {
        const r = n.getBoundingClientRect();
        return { hint: n.dataset.hint, x: r.left, y: r.top };
      }),
    );
  const shell = async () => (await names(page, "key", "text", "paste")).map(([c]) => c);

  assert.deepEqual(await labels(), [], "no hints before the action");
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  assert.deepEqual(await labels(), [["a", "a"], ["b", "b"], ["c", "c"]], "one label per target in reading order");
  const boxes = await hintBoxes();
  const at = (cx, cy) => g.box(2, cy, cx, cx);
  assert.ok(Math.abs(boxes[0].x - at(4, 0).x) < 0.6 && Math.abs(boxes[0].y - at(4, 0).y) < 0.6, "the first label sits on the URL's first cell");
  assert.ok(Math.abs(boxes[1].x - at(5, 1).x) < 0.6 && Math.abs(boxes[1].y - at(5, 1).y) < 0.6, "the second on the path's");
  assert.ok(Math.abs(boxes[2].x - at(7, 2).x) < 0.6 && Math.abs(boxes[2].y - at(7, 2).y) < 0.6, "the third on the hash's");
  await reset(page);

  // A label copies the target text and leaves the mode.
  await page.keyboard.press("a");
  assert.deepEqual(await copied(page), [URL_TEXT], "the URL was copied through the clipboard path");
  assert.deepEqual(await labels(), [], "the mode ended");
  assert.equal(await page.evaluate(() => document.activeElement.id), "ime");
  assert.deepEqual(await names(page, "open_url", "reveal_path"), [], "copying opens nothing");
  assert.deepEqual(await shell(), [], "the letter did not reach the program");

  // Shift plus a label opens or reveals.
  await spyClipboard(page);
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  await page.keyboard.press("Shift+a");
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: URL_TEXT }], "Shift+a opens the URL");
  assert.deepEqual(await copied(page), [], "opening copies nothing");

  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  await page.keyboard.press("Shift+b");
  assert.deepEqual(await callsOf(page, "reveal_path"), [{ path: "./src/main.rs", base: "/home/u/proj" }]);

  // A hash cannot be opened: the label copies it, Shift or not.
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  await page.keyboard.press("Shift+c");
  assert.deepEqual(await copied(page), ["3f2a9c1"]);
  assert.equal((await callsOf(page, "open_url")).length, 1);

  // Escape and an unknown key leave without doing anything.
  for (const key of ["Escape", "1", "Enter"]) {
    await reset(page);
    await spyClipboard(page);
    await page.keyboard.press("Meta+Shift+Space");
    await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
    await page.keyboard.press(key);
    assert.deepEqual(await labels(), [], `${key} leaves the mode`);
    assert.deepEqual(await copied(page), [], `${key} copies nothing`);
    assert.deepEqual(await names(page, "open_url", "reveal_path"), [], `${key} opens nothing`);
    assert.deepEqual(await shell(), [], `${key} did not reach the program`);
  }

  // A modifier on its own is not an unknown key: Shift can be pressed before the letter.
  await reset(page);
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  await page.keyboard.down("Shift");
  assert.equal((await labels()).length, 3, "Shift alone keeps the mode");
  await page.keyboard.press("a");
  await page.keyboard.up("Shift");
  assert.equal((await callsOf(page, "open_url")).length, 1);
}

/** More targets than letters: prefix letters wait for a second letter, and the labels left are the ones that match. */
async function manyHintsCase(page, url) {
  const rows = Array.from({ length: 12 }, () => "1111111 2222222 3333333 4444444 5555555");
  await open(page, url, { rows });
  await spyClipboard(page);
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  const hints = () => page.evaluate(() => [...document.querySelectorAll("#overlay [data-hint]")].map((n) => n.dataset.hint));
  const all = await hints();
  assert.equal(all.length, 60, "one label per target");
  assert.equal(new Set(all).size, 60, "labels are unique");
  for (const a of all) for (const b of all) assert.ok(a === b || !b.startsWith(a), `"${a}" is a prefix of "${b}"`);
  const twoLetter = all.find((l) => l.length === 2);
  assert.ok(twoLetter, "two-letter labels are in use");

  await page.keyboard.press(twoLetter[0]);
  const left = await hints();
  assert.ok(left.length > 0 && left.every((l) => l.startsWith(twoLetter[0])), "only labels that continue the typed letter are shown");
  assert.deepEqual(await copied(page), [], "nothing is copied before the label is complete");
  await page.keyboard.press(twoLetter[1]);
  assert.equal((await copied(page)).length, 1, "the second letter completes the label");
  assert.match((await copied(page))[0], /^\d{7}$/);
  assert.deepEqual(await hints(), []);

  // Shift on the second letter opens as well (a hash cannot be opened: it is copied).
  assert.equal((await names(page, "key", "text")).length, 0, "no key reached the program");
}

/** With nothing to label hints.open does nothing and the keyboard stays the terminal's. */
async function noTargetCase(page, url) {
  await open(page, url, { rows: ["nothing", "to see", "here"] });
  await page.keyboard.press("Meta+Shift+Space");
  await frames(page);
  assert.equal(await page.evaluate(() => document.querySelectorAll("#overlay [data-hint]").length), 0);
  await reset(page);
  await page.keyboard.press("a");
  assert.ok((await names(page, "key")).length >= 1, "a key still goes to the program");
}
