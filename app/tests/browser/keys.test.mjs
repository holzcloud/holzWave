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
}

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
  const prevented = await keydown(page, { key: "é", code: "KeyE" });
  assert.equal(prevented, true, "a plain character press is handled by the page");
  await keydown(page, { key: "\u{1F600}", code: "" });
  await keydown(page, { key: "a", code: "KeyA", repeat: true });
  await keydown(page, { key: "A", code: "KeyA", shiftKey: true });
  const calls = (await sent(page)).slice(before);
  assert.deepEqual(calls, [
    ["key", { pane: 2, code: "KeyE", key: "é", mods: 0, repeat: false, release: false, composing: false }],
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
  await keydown(page, { key: "ü", code: "BracketLeft", metaKey: true });
  await keydown(page, { key: "¨", code: "BracketRight", metaKey: true });
  await keydown(page, { key: "¡", code: "Digit1", metaKey: true, altKey: true });
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
