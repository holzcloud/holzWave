// Keyboard input in the real page against the Tauri stub: a key press becomes one `key` command with DOM facts
// only (no escape sequence is built in the page), shortcuts match by logical and physical key, and the handler
// stack still sees keys first. Every case reads the commands the page sent.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";

/** Open the app against the stub and wait until the layout is on screen and every pane was started. */
async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

/** Every command the page sent so far, as [cmd, args], without the start-up ones. */
const sent = (page) =>
  page.evaluate(() => window.__MOCK__.calls.filter(([c]) => c !== "get_layout" && c !== "start_pane" && c !== "resize"));

/** Press `chord` and return the commands it caused, in order. */
async function press(page, chord) {
  const before = (await sent(page)).length;
  await page.keyboard.press(chord);
  return (await sent(page)).slice(before);
}

/** Dispatch a keydown the way a layout that types `key` for physical `code` would. */
const keydown = (page, init) =>
  page.evaluate((i) => {
    const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, composed: true, ...i });
    (document.getElementById("ime") ?? window).dispatchEvent(e);
    return e.defaultPrevented;
  }, init);

export default async function ({ page, url }) {
  await plainKeyCase(page, url);
  await facts(page, url);
  await shortcutCase(page, url);
  await handlerCase(page, url);
  await inputElementCase(page, url);
  await composeCase(page, url);
  await commitCase(page, url);
  await placementCase(page, url);
  await focusCase(page, url);
}

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const ROW_PX = 18; // one cell row: ceil(14 px font * 1.25)
const HEADER = 20; // CSS px of a pane's header strip at the base font size (state.headerPx); the grid starts below it

/** One frame message for `pane` with the cursor at (x, y) in a 60 x 4 grid. */
const frame = (pane, seq, x, y) => ({
  pane, seq, full: true, cols: 60, rows: 4, fg: 0xf6efe6, bg: 0x14091a,
  cursor: { x, y, style: "block", blink: false },
  scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null,
  modes: { alt: false, bracketed_paste: false, mouse: false }, changed: [],
});

/** Wait for two animation frames: the page has drawn and moved whatever it moves per frame. */
const settle = (page) =>
  page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done()))));

/** The textarea's place inside the stage in CSS pixels. */
const imeAt = (page) =>
  page.evaluate(() => {
    const t = document.getElementById("ime");
    return { left: parseFloat(t.style.left), top: parseFloat(t.style.top), height: parseFloat(t.style.height) };
  });

/** Fire the composition events of one commit the way an engine does; `order` picks WebKit's or Chromium's. */
const compose = (page, data, order = "webkit") =>
  page.evaluate(
    ([d, o]) => {
      const t = document.getElementById("ime");
      const comp = (type, data) => t.dispatchEvent(new CompositionEvent(type, { bubbles: true, data }));
      const input = (isComposing) =>
        t.dispatchEvent(new InputEvent("input", { bubbles: true, data: d, inputType: "insertCompositionText", isComposing }));
      comp("compositionstart", "");
      t.value = d;
      comp("compositionupdate", d);
      if (o === "chromium") {
        input(true); // Chromium: the last input event comes before compositionend and still says composing
        comp("compositionend", d);
      } else {
        comp("compositionend", d);
        t.value = d; // WebKit: the text lands in the field after compositionend
        input(false);
      }
    },
    [data, order],
  );

/** A plain key press is a `key` command for the focused pane: down, then up. Nothing is called `input`. */
async function plainKeyCase(page, url) {
  await open(page, url);
  const up = await press(page, "ArrowUp");
  assert.deepEqual(up, [
    ["key", { pane: 2, code: "ArrowUp", key: "ArrowUp", mods: 0, repeat: false, release: false, composing: false }],
    ["key", { pane: 2, code: "ArrowUp", key: "ArrowUp", mods: 0, repeat: false, release: true, composing: false }],
  ], "ArrowUp is a press and a release with the facts of the event");

  const ctrlC = await press(page, "Control+c");
  const down = ctrlC.find(([cmd, a]) => cmd === "key" && !a.release);
  assert.ok(down, `Ctrl+C sent ${JSON.stringify(ctrlC)}`);
  assert.deepEqual(
    down[1],
    { pane: 2, code: "KeyC", key: "c", mods: 4, repeat: false, release: false, composing: false },
    "Ctrl+C is mods 4 with key c; the core makes the byte",
  );
  assert.equal(ctrlC.filter(([cmd]) => cmd === "key").length, 2, "only the C press and release: no modifier key events");

  assert.deepEqual(await callsOf(page, "input"), [], "the removed input command is never called");
  assert.deepEqual(await callsOf(page, "text"), [], "a key press is not text");
}

/** Characters beyond ASCII, repeats and shifted keys arrive as they are. */
async function facts(page, url) {
  await open(page, url);
  const before = (await sent(page)).length;
  const prevented = await keydown(page, { key: "\u00e9", code: "KeyE" });
  assert.equal(prevented, true, "a plain character press is handled by the page");
  await keydown(page, { key: "\u{1F600}", code: "" });
  await keydown(page, { key: "a", code: "KeyA", repeat: true });
  await keydown(page, { key: "A", code: "KeyA", shiftKey: true });
  const calls = (await sent(page)).slice(before);
  assert.deepEqual(calls, [
    ["key", { pane: 2, code: "KeyE", key: "\u00e9", mods: 0, repeat: false, release: false, composing: false }],
    ["key", { pane: 2, code: "", key: "\u{1F600}", mods: 0, repeat: false, release: false, composing: false }],
    ["key", { pane: 2, code: "KeyA", key: "a", mods: 0, repeat: true, release: false, composing: false }],
    ["key", { pane: 2, code: "KeyA", key: "A", mods: 1, repeat: false, release: false, composing: false }],
  ], "the page forwards what the event says and nothing else");
}

/** Shortcuts run actions, not keys; an unowned Cmd chord goes nowhere; Swiss and Option layouts still match. */
async function shortcutCase(page, url) {
  await open(page, url);
  assert.deepEqual(await press(page, "Meta+d"), [["split", { column: false }]], "Cmd+D splits and sends no key");
  assert.deepEqual(await press(page, "Meta+j"), [], "an unbound Cmd chord sends nothing");

  // A Swiss layout types "u-umlaut" for the physical bracket key; Cmd+[ must still move the focus.
  const before = (await sent(page)).length;
  await keydown(page, { key: "\u00fc", code: "BracketLeft", metaKey: true });
  await keydown(page, { key: "\u00a8", code: "BracketRight", metaKey: true });
  await keydown(page, { key: "\u00a1", code: "Digit1", metaKey: true, altKey: true });
  assert.deepEqual((await sent(page)).slice(before), [
    ["cycle_focus", { step: -1 }],
    ["cycle_focus", { step: 1 }],
    ["select_workspace", { index: 0 }],
  ], "shortcuts match by physical key");
}

/** A pushed handler gets the key before the shortcut and the terminal; a handler that passes changes nothing. */
async function handlerCase(page, url) {
  await open(page, url);
  await page.evaluate(async () => {
    const mod = await import(new URL("keyboard.js", location.href).href);
    window.__pop = mod.pushKeyHandler((e) => e.key === "d" || e.key === "x");
  });
  assert.deepEqual(await press(page, "Meta+d"), [], "a consuming handler stops the shortcut");
  assert.deepEqual(await press(page, "x"), [], "and the key for the terminal");
  const passed = await press(page, "y");
  assert.equal(passed.filter(([c]) => c === "key").length, 2, "a key the handler passes still reaches the core");
  await page.evaluate(() => window.__pop());
  assert.deepEqual(await press(page, "Meta+d"), [["split", { column: false }]], "popping restores the shortcut");
}

/** The hidden input is a one-pixel textarea with autocorrection off, and it has the keyboard. */
async function inputElementCase(page, url) {
  await open(page, url);
  const info = await page.evaluate(() => {
    const t = document.getElementById("ime");
    return {
      tag: t?.tagName,
      label: t?.getAttribute("aria-label"),
      spellcheck: t?.spellcheck,
      autocomplete: t?.getAttribute("autocomplete"),
      autocorrect: t?.getAttribute("autocorrect"),
      autocapitalize: t?.getAttribute("autocapitalize"),
      focused: document.activeElement === t,
    };
  });
  assert.deepEqual(info, {
    tag: "TEXTAREA", label: "Terminal input", spellcheck: false, autocomplete: "off", autocorrect: "off",
    autocapitalize: "off", focused: true,
  });
}

/** Composing keys belong to the input method: no command, and the browser's default is left alone. */
async function composeCase(page, url) {
  await open(page, url);
  const prevented = await page.evaluate(() => {
    const t = document.getElementById("ime");
    const send = (init) => {
      const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
      t.dispatchEvent(e);
      return e.defaultPrevented;
    };
    return [
      send({ key: "Process", code: "KeyA", keyCode: 229, isComposing: true }),
      send({ key: "a", code: "KeyA", keyCode: 229 }),
      send({ key: "Dead", code: "KeyE", altKey: true }),
    ];
  });
  assert.deepEqual(prevented, [false, false, false], "a composing keydown must not be preventDefault-ed");
  assert.deepEqual(await callsOf(page, "key"), [], "a composing keydown must not reach the core");
}

/** A commit is one `text` command, whatever order the engine fires its events in. */
async function commitCase(page, url) {
  await open(page, url);
  const texts = () => callsOf(page, "text");

  await compose(page, "\u3042", "webkit");
  assert.deepEqual(await texts(), [{ pane: 2, text: "\u3042" }], "a WebKit commit is sent once (compositionend, then input)");
  assert.equal(await page.evaluate(() => document.getElementById("ime").value), "", "the field is emptied");

  await page.waitForTimeout(150); // the double-delivery guard is for the same commit only
  await compose(page, "\u3042", "chromium");
  assert.deepEqual(
    await texts(),
    [{ pane: 2, text: "\u3042" }, { pane: 2, text: "\u3042" }],
    "a Chromium commit (input while composing, then compositionend) is sent once",
  );

  // An empty commit (the composition was cancelled) sends nothing.
  await page.evaluate(() => {
    const t = document.getElementById("ime");
    t.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true, data: "" }));
    t.value = "x";
    t.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "" }));
  });
  assert.equal((await texts()).length, 2, "an empty commit sends nothing");
  assert.equal(await page.evaluate(() => document.getElementById("ime").value), "", "and still empties the field");

  // Text that arrives outside a composition (a dead-key result, the emoji picker, dictation).
  await page.evaluate(() => {
    const t = document.getElementById("ime");
    for (const text of ["\u00e9", "\u{1F600}"]) {
      t.value = text;
      t.dispatchEvent(new InputEvent("input", { bubbles: true, data: text, inputType: "insertText" }));
    }
  });
  assert.deepEqual((await texts()).slice(2), [{ pane: 2, text: "\u00e9" }, { pane: 2, text: "\u{1F600}" }]);
  assert.equal(await page.evaluate(() => document.getElementById("ime").value), "", "the field is emptied");
  assert.deepEqual(await callsOf(page, "key"), [], "a commit is text, never a key");
  assert.deepEqual(await callsOf(page, "paste"), [], "and never a paste: it is not bracketed");

  // WebKit fires keydown for the Enter that committed the composition after compositionend: it is not input.
  await page.waitForTimeout(150);
  await page.evaluate(() => {
    const t = document.getElementById("ime");
    t.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true, data: "" }));
    t.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "\u3044" }));
    t.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true, cancelable: true }));
  });
  assert.deepEqual(await callsOf(page, "key"), [], "the Enter that ended a composition is not sent");
  await page.waitForTimeout(150);
  await page.keyboard.press("Enter");
  assert.equal((await callsOf(page, "key")).filter((k) => k.key === "Enter" && !k.release).length, 1, "a later Enter is");
}

/** While a composition is active the textarea sits at the cursor cell of the focused pane. */
async function placementCase(page, url) {
  await open(page, url);
  await page.evaluate(() => {
    document.getElementById("ime").dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true, data: "" }));
  });
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(2, 1, 5, 2));
  await settle(page);
  const a = await imeAt(page);
  assert.equal(a.top, HEADER + 2 * ROW_PX, "the row of the cursor cell");
  assert.equal(a.height, ROW_PX, "one cell high, so the candidate window opens under the cursor row");

  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(2, 2, 15, 3));
  await settle(page);
  const b = await imeAt(page);
  assert.equal(b.top, HEADER + 3 * ROW_PX);
  const cellW = (b.left - a.left) / 10;
  assert.ok(cellW > 3 && cellW < 30, `cell width ${cellW}`);
  const width = await page.evaluate(() => document.getElementById("term").getBoundingClientRect().width);
  assert.ok(Math.abs(a.left - (0.5 * width + PAD + 5 * cellW)) < 0.01, `left ${a.left} is the cell column of pane 2`);

  // A frame of the other pane does not move it, and neither does a frame after the composition ended.
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(1, 1, 1, 0));
  await settle(page);
  assert.deepEqual(await imeAt(page), b, "a frame of an unfocused pane");
  await page.evaluate(() => {
    document.getElementById("ime").dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "" }));
  });
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(2, 3, 30, 0));
  await settle(page);
  assert.deepEqual(await imeAt(page), b, "a frame after the composition ended");
}

/** The terminal input keeps the keyboard: a click on the canvas or on nothing gives it back. */
async function focusCase(page, url) {
  await open(page, url);
  const active = () => page.evaluate(() => document.activeElement && document.activeElement.id);
  const c = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width, height: r.height };
  });
  await page.mouse.click(c.left + c.width * 0.25, c.top + c.height * 0.5);
  assert.equal(await active(), "ime", "a click on the canvas leaves the keyboard with the terminal input");

  await page.evaluate(() => document.getElementById("ime").blur());
  await page.waitForFunction(() => document.activeElement && document.activeElement.id === "ime", undefined, { timeout: 2000 });

  const before = (await sent(page)).length;
  await page.keyboard.press("a");
  const keys = (await sent(page)).slice(before).filter(([cmd]) => cmd === "key");
  assert.equal(keys.length, 2, "typing works after the focus came back");

  // A text field of the page keeps its own focus and its own keys.
  await page.evaluate(() => {
    const input = document.createElement("input");
    input.id = "other";
    document.body.append(input);
    input.focus();
  });
  await page.waitForTimeout(50);
  assert.equal(await active(), "other", "the terminal input does not take the focus from a text field");
  const mid = (await sent(page)).length;
  await page.keyboard.type("hi");
  assert.deepEqual((await sent(page)).slice(mid), [], "typing in a page field reaches no pane");
}
