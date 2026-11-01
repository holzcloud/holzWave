// The clipboard of the browser build: a paste and a copy through the gesture paths a browser allows, never through the
// native clipboard command of the desktop shell, and nothing a program prints reaches the clipboard. The paste chord, the
// browser's own paste and `Shift+Insert` fire the page's `paste` event; when none comes within 100 ms the page reads the
// clipboard once from that gesture. Copy by the key writes the cached selection inside the key press; copy on select
// writes inside the pointer release, with a clipboard item whose text is a promise that the page resolves from
// `selection_text`. The desktop's own paths are covered by paste.test.mjs and selection.test.mjs, which run unchanged.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { callsOf, emit } from "./lib.mjs";

export const backends = ["ws"];
export const backendNote = "the web paths of the clipboard exist only on the web backend; the desktop's are in paste.test.mjs and selection.test.mjs";

const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

const REFUSED = "The browser did not allow reading the clipboard.";
const SETTLE_MS = 350; // longer than the page's wait for a `paste` event (100 ms) and its late-event window
const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const HEADER = 24; // CSS px of a pane's header strip at the base font size
const BG = 0x14091a;
const MODES = { alt: false, bracketed_paste: false, mouse: false };

const CTRL_SHIFT_V = { code: "KeyV", key: "V", ctrlKey: true, shiftKey: true };
const SHIFT_INSERT = { code: "Insert", key: "Insert", shiftKey: true };
const CTRL_SHIFT_C = { code: "KeyC", key: "C", ctrlKey: true, shiftKey: true };

const frame = (pane, seq, selection = [], over = {}) => ({
  pane, seq, full: false, cols: 20, rows: 4, fg: 0xf6efe6, bg: BG, cursor: null, scrollbar: [0, 4, 4],
  selection, matches: [], search: null, copy_cursor: null, modes: MODES, changed: [], ...over,
});

/** Open the page against the stub as the web backend: `app_info` names the platform `web` before the page starts. */
async function open(page, url) {
  await page.addInitScript((i) => window.__MOCK__.on("app_info", () => i), { version: "0.0.1-alpha1", os: "web", hostname: "woodbox" });
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").addEventListener("pointerdown", (e) => { window.__pid = e.pointerId; }, true));
}

/** The same with the geometry helpers of the selection test, and the mirror of both panes at 20 x 4 cells. */
async function openGrid(page, url) {
  await open(page, url);
  const start = (await callsOf(page, "start_pane"))[0];
  const c = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });
  const origin = (pane) => c.left + (pane === 1 ? 0 : c.width / 2) + PAD;
  await emit(page, "frame", frame(1, 1, [], { full: true }));
  await emit(page, "frame", frame(2, 1, [], { full: true }));
  return { pt: (pane, cx, cy) => [origin(pane) + (cx + 0.5) * start.cell_w, c.top + HEADER + (cy + 0.5) * start.cell_h] };
}

/** A `paste` event on the terminal input, as the browser fires it for a paste chord. Returns defaultPrevented. */
const paste = (page, text) =>
  page.evaluate((t) => {
    const data = new DataTransfer();
    if (t !== null) data.setData("text/plain", t);
    const e = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
    document.getElementById("ime").dispatchEvent(e);
    return e.defaultPrevented;
  }, text);

/** A `keydown` on the terminal input (untrusted, so the browser's own default never runs). Returns defaultPrevented. */
const keydown = (page, init) =>
  page.evaluate((i) => {
    const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...i });
    document.getElementById("ime").dispatchEvent(e);
    return e.defaultPrevented;
  }, init);

/**
 * Replace the page's clipboard with a recording one: `readText` answers `text` after `delay` ms (or rejects when `text`
 * is null), `writeText` and `write` record, `execCommand` records the value of the focused field. `window.__log` holds
 * `[kind, ...]` in order; `write` waits for every item the way a browser does, so a promise that fails fails the write.
 */
const recordClipboard = (page, text, delay = 0) =>
  page.evaluate(
    ([t, d]) => {
      window.__log = [];
      window.__reads = 0;
      window.__writeFails = false;
      document.execCommand = (cmd) => {
        const a = document.activeElement;
        window.__log.push(["exec", cmd, a && a.value]);
        return true;
      };
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: {
          readText: () => {
            window.__reads++;
            window.__log.push(["readText"]);
            return t === null ? Promise.reject(new Error("denied")) : new Promise((done) => setTimeout(() => done(t), d));
          },
          writeText: async (x) => void window.__log.push(["writeText", x]),
          write: async (items) => {
            window.__log.push(["write", items.length, items.map((i) => [...i.types])]);
            window.__items = items;
            if (window.__writeFails) throw new Error("not allowed");
            for (const item of items) for (const type of item.types) await item.getType(type);
          },
        },
      });
    },
    [text, delay],
  );
const logOf = (page) => page.evaluate(() => window.__log);
const kinds = async (page) => (await logOf(page)).map((l) => l[0]);
const pastes = (page) => callsOf(page, "paste");
const wait = (page, ms) => page.waitForTimeout(ms);
const notice = (page) => page.evaluate(() => document.querySelector("#status .notice")?.textContent ?? "");
const frames = (page, n = 2) =>
  page.evaluate(async (count) => {
    for (let i = 0; i < count; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  }, n);

export default async function ({ page, url, errors }) {
  await eventRouteCase(page, url);
  await fallbackRouteCase(page, url);
  await refusedReadCase(page, url);
  await confirmCase(page, url);
  await copyKeyCase(page, url);
  await copyOnSelectCase(page, url);
  await copyOnSelectFallbackCase(page, url);
  await copyOnSelectRefusedCase(page, url);
  await terminalClipboardCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The primary route: a `paste` event within 100 ms pastes its text once; nothing else is called. */
async function eventRouteCase(page, url) {
  await open(page, url);
  await recordClipboard(page, "from readText");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  for (const [n, chord] of [CTRL_SHIFT_V, SHIFT_INSERT].entries()) {
    assert.equal(await keydown(page, chord), false, `${JSON.stringify(chord)} keeps its default so the browser pastes`);
    assert.equal(await paste(page, `event ${n}`), true, "the page takes the paste event itself");
    await wait(page, SETTLE_MS);
    assert.equal((await pastes(page)).length, n + 1, `exactly one paste for chord ${n}`);
    assert.deepEqual((await pastes(page))[n], { pane: 2, text: `event ${n}`, allow_unsafe: false });
  }
  assert.equal((await callsOf(page, "clipboard_text")).length, 0, "the native clipboard command is never called on the web");
  assert.deepEqual(await logOf(page), [], "readText is not asked when the event came");
  assert.equal((await callsOf(page, "key")).length, 0, "the chords are not terminal input");
}

/** No event: the page reads the clipboard once from that gesture and pastes through the same path. */
async function fallbackRouteCase(page, url) {
  for (const chord of [CTRL_SHIFT_V, SHIFT_INSERT]) {
    await open(page, url);
    await recordClipboard(page, "from readText");
    await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
    assert.equal(await keydown(page, chord), false);
    await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length >= 1);
    assert.deepEqual(await pastes(page), [{ pane: 2, text: "from readText", allow_unsafe: false }]);
    assert.equal(await page.evaluate(() => window.__reads), 1, "the clipboard was read once");
    assert.equal((await callsOf(page, "clipboard_text")).length, 0, "the native clipboard command is never called on the web");
    await paste(page, "late event");
    await wait(page, SETTLE_MS);
    assert.equal((await pastes(page)).length, 1, "a late paste event after the fallback does not paste a second time");
  }

  // An empty clipboard sends nothing, and there is no cue: the cue is for macOS only.
  await open(page, url);
  await recordClipboard(page, "");
  await keydown(page, SHIFT_INSERT);
  await wait(page, SETTLE_MS);
  assert.equal(await page.evaluate(() => window.__reads), 1);
  assert.equal((await pastes(page)).length, 0, "an empty clipboard sends nothing");
  assert.equal((await callsOf(page, "paste_cue")).length, 0, "and no cue");
  assert.equal((await callsOf(page, "clipboard_text")).length, 0);
  assert.equal(await notice(page), "", "an empty clipboard is not a refusal");
}

/** A read that the browser refuses says so in the status line, sends nothing and calls no native command. */
async function refusedReadCase(page, url) {
  await open(page, url);
  await recordClipboard(page, null);
  await keydown(page, SHIFT_INSERT);
  await page.waitForFunction((s) => document.querySelector("#status .notice")?.textContent === s, REFUSED);
  assert.equal(await notice(page), REFUSED);
  assert.equal((await pastes(page)).length, 0, "a refused read sends nothing");
  assert.equal((await callsOf(page, "clipboard_text")).length, 0);

  // A page without a clipboard object at all says the same.
  await open(page, url);
  await page.evaluate(() => Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined }));
  await keydown(page, CTRL_SHIFT_V);
  await page.waitForFunction((s) => document.querySelector("#status .notice")?.textContent === s, REFUSED);
  assert.equal((await pastes(page)).length, 0);
}

/** Both routes end in the one paste path: several lines ask first, exactly as on the desktop. */
async function confirmCase(page, url) {
  for (const route of ["event", "read"]) {
    await open(page, url);
    const seven = ["a", "b", "c", "d", "e", "f", "g"].join("\n");
    await recordClipboard(page, seven);
    await page.evaluate(() => window.__MOCK__.on("paste", (a) => (a.allow_unsafe ? "written" : "rejected")));
    await keydown(page, CTRL_SHIFT_V);
    if (route === "event") await paste(page, seven);
    await page.locator('[role="dialog"]').waitFor({ state: "visible" });
    assert.deepEqual(await pastes(page), [{ pane: 2, text: seven, allow_unsafe: false }], "asked before anything unsafe was sent");
    await page.keyboard.press("Enter");
    await page.locator('[role="dialog"]').waitFor({ state: "detached" });
    assert.deepEqual((await pastes(page)).map((p) => p.allow_unsafe), [false, true], `${route}: confirmed once`);
    assert.equal((await callsOf(page, "clipboard_text")).length, 0);
  }
}

/** Copy by the key writes the cached selection text inside the key press; the core's copy command is not used. */
async function copyKeyCase(page, url) {
  await open(page, url);
  await recordClipboard(page, "unused");
  await page.evaluate(() => window.__MOCK__.on("selection_text", () => "abc"));
  await emit(page, "frame", frame(2, 1, [], { full: true }));
  await emit(page, "frame", frame(2, 2, [[1, 0, 3]]));
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await frames(page);

  // The key event is dispatched and the log read in one task: the write happened inside the key press or not at all.
  const inside = await page.evaluate((init) => {
    const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
    document.getElementById("ime").dispatchEvent(e);
    return { prevented: e.defaultPrevented, log: window.__log.slice() };
  }, CTRL_SHIFT_C);
  assert.equal(inside.prevented, true);
  assert.deepEqual(inside.log, [["exec", "copy", "abc"]], "the cached text was copied inside the key press");
  await wait(page, 100);
  assert.deepEqual(await kinds(page), ["exec"], "and nothing else was written");
  assert.equal((await callsOf(page, "copy_selection")).length, 0, "the core's copy command is not used on the web");
  assert.equal((await callsOf(page, "clipboard_text")).length, 0);
  assert.equal((await callsOf(page, "key")).length, 0, "the chord is not terminal input");
}

/**
 * Copy on select: after a drag the page calls `navigator.clipboard.write` once, inside the pointer release, with one
 * item of type `text/plain` whose promise resolves to the text of `selection_text`, asked after the last select command
 * was answered. The setting off writes nothing.
 */
async function copyOnSelectCase(page, url) {
  const g = await openGrid(page, url);
  await recordClipboard(page, "unused");
  await page.evaluate(() => {
    window.__MOCK__.on("select", () => new Promise((done) => setTimeout(() => { window.__selectDone = performance.now(); done(null); }, 40)));
    window.__MOCK__.on("selection_text", () => { window.__textAt = performance.now(); return "dragged text"; });
  });
  const drag = async () => {
    await page.mouse.move(...g.pt(2, 1, 1));
    await page.mouse.down();
    await page.mouse.move(...g.pt(2, 8, 3), { steps: 6 });
    await frames(page);
    assert.deepEqual(await kinds(page), [], "nothing is written while the button is down");
  };
  // The release is a synthetic event dispatched from a script, so no microtask runs between its listeners and the
  // count read straight after is the count inside the release.
  const release = (pt) =>
    page.evaluate(([x, y]) => {
      const e = new PointerEvent("pointerup", { clientX: x, clientY: y, pointerId: window.__pid, bubbles: true, button: 0, buttons: 0 });
      document.getElementById("term").dispatchEvent(e);
      return window.__log.filter((l) => l[0] === "write").length;
    }, pt);

  await drag();
  assert.equal(await release(g.pt(2, 8, 3)), 1, "the clipboard write started inside the pointer release");
  await page.mouse.up();
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await wait(page, 100);
  assert.deepEqual(await logOf(page), [["write", 1, [["text/plain"]]]], "one write of one item of type text/plain, nothing else");
  const text = await page.evaluate(async () => (await window.__items[0].getType("text/plain")).text());
  assert.equal(text, "dragged text", "the item's text is the core's selection text");
  const order = await page.evaluate(() => [window.__selectDone, window.__textAt]);
  assert.ok(order[1] >= order[0], "the text was asked after the last select command was answered");
  assert.equal((await callsOf(page, "copy_selection")).length, 0, "the core's copy command is not used on the web");
  assert.equal((await callsOf(page, "clipboard_text")).length, 0);

  // An empty selection writes nothing at all: the promise of the item fails and the clipboard keeps what it had.
  await page.evaluate(() => {
    window.__log.length = 0;
    window.__MOCK__.on("selection_text", () => null);
  });
  await page.waitForTimeout(450);
  await drag();
  assert.equal(await release(g.pt(2, 8, 3)), 1, "the write still starts inside the release");
  await page.mouse.up();
  await wait(page, 150);
  assert.deepEqual(await kinds(page), ["write"], "no text came, so no second try either");

  // A click that never moved copies nothing.
  await page.evaluate(() => { window.__log.length = 0; });
  await page.waitForTimeout(450);
  await page.mouse.click(...g.pt(2, 3, 2));
  await wait(page, 150);
  assert.deepEqual(await logOf(page), [], "a click is not a selection");

  // The setting off: nothing is written and the text is not even asked for.
  const off = structuredClone(golden.settings_view);
  off.settings.copy_on_select = false;
  await emit(page, "settings", off);
  await page.evaluate(() => {
    window.__MOCK__.reset();
    window.__MOCK__.on("selection_text", () => "never");
  });
  await page.waitForTimeout(450);
  await drag();
  assert.equal(await release(g.pt(2, 8, 3)), 0, "copy_on_select false writes nothing");
  await page.mouse.up();
  await wait(page, 150);
  assert.deepEqual(await logOf(page), []);
  assert.equal((await callsOf(page, "selection_text")).length, 0, "and the text is not asked for");
}

/** No clipboard item type, or a write that throws: the async text write is the second try, after the answer. */
async function copyOnSelectFallbackCase(page, url) {
  for (const how of ["no item type", "write throws"]) {
    const g = await openGrid(page, url);
    await recordClipboard(page, "unused");
    await page.evaluate((h) => {
      window.__MOCK__.on("selection_text", () => "second try");
      if (h === "no item type") window.ClipboardItem = undefined;
      else window.__writeFails = true;
    }, how);
    await page.mouse.move(...g.pt(2, 1, 1));
    await page.mouse.down();
    await page.mouse.move(...g.pt(2, 8, 3), { steps: 6 });
    await frames(page);
    await page.mouse.up();
    await page.waitForFunction(() => window.__log.some((l) => l[0] === "writeText"));
    await wait(page, 100);
    const log = await logOf(page);
    assert.deepEqual(log.filter((l) => l[0] === "writeText"), [["writeText", "second try"]], `${how}: one text write with the text`);
    assert.equal(log.filter((l) => l[0] === "write").length, how === "no item type" ? 0 : 1, `${how}: the item write was tried or not`);
    assert.equal((await callsOf(page, "copy_selection")).length, 0);
    assert.equal((await callsOf(page, "clipboard_text")).length, 0);
  }
}

/**
 * An empty selection, and the browser refuses the clipboard write before it looks at the item (the document has no focus). Nobody
 * reads the item's text promise then, and its rejection ("no selection") must not be reported as an uncaught error of the page for a
 * plain click. The page's own listener sees unhandled rejections in every engine; the runner's `errors` list sees them again.
 */
async function copyOnSelectRefusedCase(page, url) {
  const g = await openGrid(page, url);
  await recordClipboard(page, "unused");
  await page.evaluate(() => {
    window.__rejected = [];
    window.addEventListener("unhandledrejection", (e) => window.__rejected.push(String(e.reason && e.reason.message)));
    window.__MOCK__.on("selection_text", () => null);
    // The refusal comes at once and the item is never read, as a browser does when the document is not focused.
    navigator.clipboard.write = (items) => {
      window.__log.push(["write", items.length, items.map((i) => [...i.types])]);
      return Promise.reject(new DOMException("Document is not focused.", "NotAllowedError"));
    };
  });
  await page.mouse.move(...g.pt(2, 1, 1));
  await page.mouse.down();
  await page.mouse.move(...g.pt(2, 8, 3), { steps: 6 });
  await frames(page);
  await page.mouse.up();
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await wait(page, 250);
  assert.deepEqual(await kinds(page), ["write"], "the write was tried once, and with no text there was no second try");
  assert.deepEqual(await page.evaluate(() => window.__rejected), [], "the item's rejected text was not left unhandled");
}

/** Nothing the terminal prints can write or read the clipboard: events of that kind reach no clipboard call. */
async function terminalClipboardCase(page, url) {
  await open(page, url);
  await recordClipboard(page, "must not be read");
  const hostile = ["clipboard", "osc52", "clipboard-write", "set-clipboard", "title"];
  for (const name of hostile) {
    await emit(page, name, "c3RlYWwgdGhpcw==");
    await emit(page, name, { pane: 2, text: "steal this", title: "steal this", data: "c3RlYWwgdGhpcw==", kind: "c" });
    await emit(page, name, { pane: 1, clipboard: "steal this", selection: "c", write: true });
  }
  // The terminal also asks in-band: bytes in a frame and a pane title are only that.
  await emit(page, "frame", frame(2, 3, [], { full: true, changed: [] }));
  await wait(page, SETTLE_MS);
  assert.deepEqual(await logOf(page), [], "no write, no writeText, no execCommand and no readText");
  assert.equal(await page.evaluate(() => window.__reads), 0);
  assert.equal(await page.evaluate(() => document.querySelectorAll(".clipboard-buffer").length), 0);
  for (const name of hostile.filter((n) => n !== "title")) {
    const heard = await page.evaluate(async (n) => (await import(new URL("bridge.js", location.href).href)).listenerCount(n), name);
    assert.equal(heard, 0, `the page holds no listener for "${name}"`);
  }
  assert.equal((await callsOf(page, "clipboard_text")).length, 0);
  assert.equal((await callsOf(page, "copy_selection")).length, 0);
}
