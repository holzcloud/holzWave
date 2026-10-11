// Copy mode (DESK-21): vi-style keys over the screen and scrollback, the state in the core. The page only maps
// keys to `copy_mode` operations, shows the core's copy cursor with a double border and the mode in the status
// line, copies through the same synchronous path as a mouse selection, and keeps every key from the program.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";

const BG = 0x14091a;
const MODES = { alt: false, bracketed_paste: false, mouse: false };

const frame = (pane, seq, over = {}) => ({
  pane, seq, full: false, cols: 20, rows: 4, fg: 0xf6efe6, bg: BG, cursor: null, scrollbar: [0, 4, 4],
  selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed: [], ...over,
});
const fullFrame = (pane, seq, over = {}) => frame(pane, seq, { full: true, ...over });

/** Open the page with a scripted core: `copy_mode` keeps {active, selecting} and answers like the real one. */
async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => {
    const s = { active: false, selecting: false };
    window.__cm = s;
    window.__MOCK__.on("copy_mode", ({ op }) => {
      let yanked = null;
      if (op.kind === "enter") s.active = true;
      else if (op.kind === "exit") { s.active = false; s.selecting = false; }
      else if (op.kind === "toggle" && s.active) s.selecting = !s.selecting;
      else if (op.kind === "yank") { s.active = false; s.selecting = false; yanked = window.__yank ?? null; }
      return { active: s.active, selecting: s.selecting, yanked };
    });
    document.getElementById("term").focus();
  });
  await emit(page, "frame", fullFrame(1, 1));
  await emit(page, "frame", fullFrame(2, 1));
}

/** The operations sent to the core since `from`, as plain objects. */
const ops = async (page, from = 0) => (await callsOf(page, "copy_mode")).slice(from).map((a) => ({ pane: a.pane, ...a.op }));
const count = async (page) => (await callsOf(page, "copy_mode")).length;
const mode = (page) => page.locator("#status .seg.mode").textContent();
const shellCalls = (page) =>
  page.evaluate(() => window.__MOCK__.calls.filter(([c]) => ["key", "text", "paste", "scroll", "scroll_to_bottom"].includes(c)).map(([c]) => c));
const enter = async (page) => {
  const n = await count(page);
  await page.keyboard.press("Meta+Shift+x");
  await page.waitForFunction((k) => window.__MOCK__.callsOf("copy_mode").length > k, n);
  await page.waitForFunction(() => document.querySelector("#status .seg.mode")?.textContent === "COPY");
};

export default async function ({ page, url, errors }) {
  await enterCase(page, url);
  await keysCase(page, url);
  await selectCase(page, url);
  await exitCase(page, url);
  await yankCase(page, url);
  await cursorCase(page, url);
  await searchCase(page, url);
  await paneCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The action enters, the status line names the mode and no key reaches the program. */
async function enterCase(page, url) {
  await open(page, url);
  assert.equal(await page.locator("#status .seg.mode").count(), 1, "the status line has a mode segment");
  assert.equal(await mode(page), "", "no mode, no text");
  assert.equal(await page.locator("#status .seg.mode").isVisible(), false, "an empty segment takes no room");

  await enter(page);
  assert.deepEqual(await ops(page), [{ pane: 2, kind: "enter" }], "the shortcut sends enter for the focused pane");
  assert.equal(await mode(page), "COPY");
  assert.equal(await page.locator("#status .seg.mode").isVisible(), true);

  await page.keyboard.press("a");
  await page.keyboard.press("z");
  await page.keyboard.type("hello");
  assert.deepEqual(await shellCalls(page), [], "no key reaches the program while copy mode is active");
  assert.equal(await page.evaluate(() => document.activeElement.id), "ime", "the terminal input keeps the keyboard");

  // The palette action leaves.
  await page.evaluate(async () => {
    const { runAction } = await import(new URL("actions.js", location.href).href);
    runAction("copy_mode.exit");
  });
  await page.waitForFunction(() => document.querySelector("#status .seg.mode")?.textContent === "");
  assert.equal((await ops(page)).at(-1).kind, "exit");
}

/** Every key of the plan's table sends its motion. */
async function keysCase(page, url) {
  await open(page, url);
  await enter(page);
  const table = [
    ["h", "left"], ["ArrowLeft", "left"], ["j", "down"], ["ArrowDown", "down"], ["k", "up"], ["ArrowUp", "up"],
    ["l", "right"], ["ArrowRight", "right"], ["0", "line_start"], ["Home", "line_start"], ["Shift+4", "line_end"],
    ["End", "line_end"], ["g", "top"], ["Shift+g", "bottom"], ["w", "word_forward"], ["b", "word_back"],
    ["Control+u", "page_up"], ["PageUp", "page_up"], ["Control+d", "page_down"], ["PageDown", "page_down"],
  ];
  for (const [key, motion] of table) {
    const n = await count(page);
    await page.keyboard.press(key);
    await page.waitForFunction((k) => window.__MOCK__.callsOf("copy_mode").length > k, n);
    assert.deepEqual(await ops(page, n), [{ pane: 2, kind: "move", motion }], `${key} is ${motion}`);
  }
  assert.deepEqual(await shellCalls(page), [], "no key, no scroll command, reached the core outside copy_mode");
  assert.equal(await mode(page), "COPY", "moving does not leave the mode");

  // Keys the table does not know are consumed and do nothing.
  const n = await count(page);
  await page.keyboard.press("x");
  await page.keyboard.press("F5");
  await page.keyboard.press("Tab");
  await page.waitForTimeout(50);
  assert.equal(await count(page), n, "an unmapped key sends nothing");
  assert.deepEqual(await shellCalls(page), []);

  // Shortcuts with Cmd stay shortcuts: a split goes to the core and not to the program.
  await page.keyboard.press("Meta+d");
  await page.waitForFunction(() => window.__MOCK__.callsOf("split").length >= 1);
  assert.deepEqual(await shellCalls(page), []);
}

/** v toggles the selection and the status line says SELECT. */
async function selectCase(page, url) {
  await open(page, url);
  await enter(page);
  await page.keyboard.press("v");
  await page.waitForFunction(() => document.querySelector("#status .seg.mode")?.textContent === "SELECT");
  assert.deepEqual((await ops(page)).at(-1), { pane: 2, kind: "toggle" });
  await page.keyboard.press("v");
  await page.waitForFunction(() => document.querySelector("#status .seg.mode")?.textContent === "COPY");
}

/** Escape and q leave; afterwards the keys are the program's again and the segment is gone. */
async function exitCase(page, url) {
  await open(page, url);
  for (const key of ["Escape", "q"]) {
    await enter(page);
    const n = await count(page);
    await page.keyboard.press(key);
    await page.waitForFunction((k) => window.__MOCK__.callsOf("copy_mode").length > k, n);
    assert.deepEqual(await ops(page, n), [{ pane: 2, kind: "exit" }], `${key} sends exit`);
    await page.waitForFunction(() => document.querySelector("#status .seg.mode")?.textContent === "");
    const before = (await shellCalls(page)).length;
    await page.keyboard.press("a");
    assert.equal((await shellCalls(page)).length, before + 1, `after ${key} a key goes to the program`);
    assert.equal(await page.evaluate(() => document.activeElement.id), "ime");
  }
}

/** y and Enter copy the cached text synchronously through the clipboard path and then yank. */
async function yankCase(page, url) {
  await open(page, url);
  await page.evaluate(() => {
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
    window.__MOCK__.on("selection_text", () => "picked text");
    window.__yank = "yank text";
  });

  // With the cache filled: the cached text goes out in the key's own task, then the yank is sent.
  await enter(page);
  await emit(page, "frame", frame(2, 2, { selection: [[1, 0, 3]], copy_cursor: [3, 1] }));
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await page.waitForTimeout(50);
  const n = await count(page);
  await page.keyboard.press("y");
  assert.deepEqual(await page.evaluate(() => window.__exec), [["copy", "TEXTAREA", "picked text"]], "the cache was copied synchronously");
  await page.waitForFunction((k) => window.__MOCK__.callsOf("copy_mode").length > k, n);
  assert.deepEqual(await ops(page, n), [{ pane: 2, kind: "yank" }]);
  await page.waitForFunction(() => document.querySelector("#status .seg.mode")?.textContent === "");
  assert.deepEqual(await page.evaluate(() => window.__written), [], "the yank answer was not written over the cache");

  // Enter does the same.
  await emit(page, "frame", frame(2, 3, { selection: [], copy_cursor: null }));
  await enter(page);
  await emit(page, "frame", frame(2, 4, { selection: [[2, 0, 1]], copy_cursor: [1, 2] }));
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 2);
  await page.waitForTimeout(50);
  await page.evaluate(() => { window.__exec.length = 0; });
  const m = await count(page);
  await page.keyboard.press("Enter");
  assert.equal((await page.evaluate(() => window.__exec)).length, 1, "Enter copies too");
  await page.waitForFunction((k) => window.__MOCK__.callsOf("copy_mode").length > k, m);
  assert.deepEqual((await ops(page, m)).map((o) => o.kind), ["yank"]);

  // An empty cache: the text the yank returns is what is copied.
  await emit(page, "frame", frame(2, 5, { selection: [], copy_cursor: null }));
  await page.evaluate(() => { window.__exec.length = 0; window.__written.length = 0; });
  await enter(page);
  await page.keyboard.press("y");
  await page.waitForFunction(() => window.__exec.length + window.__written.length >= 1);
  const got = await page.evaluate(() => window.__exec.map((e) => e[2]).concat(window.__written));
  assert.deepEqual(got, ["yank text"], "the yanked text is copied when the cache was empty");
}

/** The copy cursor is a keyed box with a double border; it follows the header and goes with it. */
async function cursorCase(page, url) {
  await open(page, url);
  const cursors = () =>
    page.evaluate(() =>
      [...document.querySelectorAll("#overlay .copycursor")].map((n) => {
        const r = n.getBoundingClientRect();
        const s = getComputedStyle(n);
        return { x: r.left, y: r.top, w: r.width, h: r.height, style: s.borderBottomStyle, width: parseFloat(s.borderBottomWidth), tag: n.dataset.tag ?? "" };
      }),
    );
  assert.deepEqual(await cursors(), [], "no copy cursor in the header, no box");

  await emit(page, "frame", frame(2, 2, { copy_cursor: [3, 1] }));
  let c = await cursors();
  assert.equal(c.length, 1);
  assert.equal(c[0].style, "double", "the cursor has a double border, not only a colour");
  assert.ok(c[0].width >= 3, `a double border needs 3 px to show (${c[0].width})`);

  await page.evaluate(() => { document.querySelector("#overlay .copycursor").dataset.tag = "kept"; });
  await emit(page, "frame", frame(2, 3, { copy_cursor: [4, 2] }));
  const moved = await cursors();
  assert.equal(moved.length, 1);
  assert.equal(moved[0].tag, "kept", "the same element moved");
  assert.ok(Math.abs(moved[0].x - c[0].x - moved[0].w) < 0.6, "one column to the right");
  assert.ok(Math.abs(moved[0].y - c[0].y - moved[0].h) < 0.6, "one row down");
  assert.ok(Math.abs(moved[0].w - c[0].w) < 0.6 && moved[0].w > 0, "a cell wide");

  // A cursor scrolled out of sight is null in the header while the mode stays: no box.
  await emit(page, "frame", frame(2, 4, { copy_cursor: null }));
  assert.deepEqual(await cursors(), []);
  // Off the grid draws nothing.
  await emit(page, "frame", frame(2, 5, { copy_cursor: [99, 1] }));
  assert.deepEqual(await cursors(), []);
  // The unfocused pane's cursor is not drawn.
  await emit(page, "frame", frame(1, 2, { copy_cursor: [1, 1] }));
  assert.deepEqual(await cursors(), []);
}

/** `/` opens the search bar and gives it the keys; n and N walk the matches; Escape of the bar returns to copy mode. */
async function searchCase(page, url) {
  await open(page, url);
  await enter(page);
  const n = await count(page);
  await page.keyboard.press("/");
  await page.waitForFunction(() => document.activeElement?.getAttribute("aria-label") === "Search scrollback");
  assert.equal(await page.locator("#stage .search-bar").isVisible(), true, "/ opens the search bar");
  assert.equal(await page.getByLabel("Search scrollback").inputValue(), "", "the slash did not land in the field");
  await page.keyboard.type("hjkl");
  await page.waitForTimeout(250);
  assert.equal(await count(page), n, "keys typed into the search field are not motions");
  assert.deepEqual((await callsOf(page, "search")).map((a) => a.op), [{ kind: "set", needle: "hjkl" }]);

  await page.keyboard.press("Escape");
  assert.equal(await page.locator("#stage .search-bar").isVisible(), false);
  assert.equal(await mode(page), "COPY", "closing the bar does not leave copy mode");
  await page.keyboard.press("j");
  await page.waitForFunction((k) => window.__MOCK__.callsOf("copy_mode").length > k, n);
  assert.deepEqual((await ops(page, n)).at(-1), { pane: 2, kind: "move", motion: "down" }, "the keys are copy mode's again");

  const s = (await callsOf(page, "search")).length;
  await page.keyboard.press("n");
  await page.keyboard.press("Shift+n");
  await page.waitForFunction((k) => window.__MOCK__.callsOf("search").length >= k + 2, s);
  assert.deepEqual((await callsOf(page, "search")).slice(s).map((a) => a.op.kind), ["next", "prev"], "n is next and N is previous");
  assert.deepEqual(await shellCalls(page), []);
}

/** Copy mode belongs to a pane: losing the focus ends it, and the next one starts in the focused pane. */
async function paneCase(page, url) {
  await open(page, url);
  await enter(page);
  const n = await count(page);

  await page.evaluate(() => {
    const v = window.__MOCK__.view();
    v.focus = 1;
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  });
  await page.waitForFunction(() => document.querySelector("#status .seg.mode")?.textContent === "");
  assert.deepEqual(await ops(page, n), [{ pane: 2, kind: "exit" }], "the pane that lost the focus leaves copy mode first");

  await page.keyboard.press("Meta+Shift+x");
  await page.waitForFunction((k) => window.__MOCK__.callsOf("copy_mode").length > k, n + 1);
  assert.deepEqual((await ops(page, n)).map((o) => [o.pane, o.kind]), [[2, "exit"], [1, "enter"]]);

  // A pane that exits takes the mode with it, without a command for a pane that is gone.
  const k = await count(page);
  await emit(page, "pane-exit", { pane: 1, code: 0 });
  await page.evaluate(() => {
    const v = window.__MOCK__.view();
    v.panes = v.panes.filter(([id]) => id !== 1);
    v.focus = 2;
    v.dividers = [];
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  });
  await page.waitForFunction(() => document.querySelector("#status .seg.mode")?.textContent === "");
  assert.equal(await count(page), k, "no command for a pane that is gone");
}
