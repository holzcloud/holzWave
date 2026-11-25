// Opening a link by touch on the phone and tablet layouts (docs/design.md 6.6, the paragraph on opening a link, and 6.31, By touch): a tap
// never opens a link; a long press on a printed `https`, `http` or `mailto` address, or on a link the program marked with one of those
// schemes, selects exactly the link's cells and the selection menu offers `Open` before `Copy` and `Paste`. `Open` goes through the same
// opener as hint mode (`activateLink`), so a marked link whose text names another place asks first and a refusal of the host is said where
// every refusal is said: on a phone the notice line over the stage, on a tablet the status line.
//
// No touch screen and no Android WebView is used: the platform is mocked (`app_info` answers `android`), the screen size is overridden, the
// touches are synthetic `PointerEvent`s with `pointerType: "touch"` dispatched into the desktop engines, and the host is the scripted mock.
// What only a device shows (a real finger, the system browser or mail app taking over, Android's own text selection fighting the page) is
// not covered here.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";
import { down, forget, frame, frames, logWheel, open, run, selects, shown, up, wait } from "./touch-lib.mjs";

// The touch layouts belong to the Android shell, which the web backend never is.
export const backends = ["tauri"];
export const backendNote = "the phone and tablet layouts belong to the Android shell, which the web backend never is";

const REFUSAL = "No app on this device can open this link.";
const URL_TEXT = "https://example.com/a";
/** Row 0 of the pane: the address starts at column 4 and ends at column 24, both inclusive. */
const ROW0 = `see ${URL_TEXT} now`;
const START = 4;
const END = START + URL_TEXT.length - 1;

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(6000);
  for (const [name, fn] of [
    ["a long press on a printed address offers Open", addressCase],
    ["a refusal after Open is the phone's warn line", refusalCase],
  ]) {
    try {
      await fn(page, url, engine);
    } catch (e) {
      // The case is named, and the line of the test that failed is kept, because the runner prints one line.
      const where = String(e?.stack ?? "").split("\n").find((l) => l.includes("touch-link.test.mjs"))?.trim() ?? "";
      throw new Error(`${name}: ${e?.message ?? e} ${where}`);
    }
  }
  assert.deepEqual(errors, [], `the page reported errors (${engine})`);
}

/** One row of `cols` cells with `text` in it, and the marked links of the row when given. */
const rowWith = (y, text, cols, links) => ({ y, runs: [[text.padEnd(cols), cols, 0xf6efe6, 0x14091a, 0]], ...(links ? { links } : {}) });

/** What has the keyboard focus, as a short description that a test can compare. */
const focusOf = (page) =>
  page.evaluate(() => {
    const a = document.activeElement;
    return a ? `${a.tagName}#${a.id}.${a.className}` : "none";
  });

const menuWords = (page) => page.locator(".touch-menu .touch-button").allTextContents();

/** Each button of the menu as its text and its size. */
const menuBoxes = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll(".touch-menu .touch-button")].map((b) => {
      const r = b.getBoundingClientRect();
      return { text: b.textContent, w: r.width, h: r.height };
    }),
  );

/**
 * Open the phone with `text` in row 0 of its pane, press the cell (`cx`, 0) for 560 ms and lift. Answers the pane's geometry and a function
 * that sends a frame with row 0 (and its marked links) and the selection spans given, as a full frame carries the row every time.
 */
async function pressed(page, url, text, cx, links, openOpts = {}) {
  await open(page, url, openOpts);
  await logWheel(page);
  const g = await shown(page, 2);
  const show = (selection = [], rowText = text, rowLinks = links) =>
    emit(page, "frame", frame(2, g.cols, g.rows, selection, { changed: [rowWith(0, rowText, g.cols, rowLinks)] }));
  await show();
  await page.evaluate(() => document.getElementById("term").focus());
  await frames(page);
  await forget(page);
  await run(page, [down(1, g.pt(cx, 0)), wait(560), up(1, g.pt(cx, 0))]);
  return { g, show };
}

/** Wait for the menu of a selection and answer its words. */
async function menuAfter(page) {
  await page.locator(".touch-menu").waitFor({ state: "visible" });
  await frames(page);
  return menuWords(page);
}

/**
 * A long press inside a printed address selects exactly its cells (a cell-mode begin at its first column, an extend to its last), the menu
 * reads Open, Copy, Paste with 44 px targets, and Open asks the host once for the address, clears the selection and moves no focus.
 */
async function addressCase(page, url) {
  const { show } = await pressed(page, url, ROW0, 10);
  assert.deepEqual(await selects(page), [
    { pane: 2, kind: "begin", x: START, y: 0, mode: "cell" },
    { pane: 2, kind: "extend", x: END, y: 0 },
  ], "the long press selects the whole address, not the word");
  await show([[0, START, END]]);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"], "Open comes first, before the spec's two words");
  for (const b of await menuBoxes(page)) assert.ok(b.w >= 43.99 && b.h >= 43.99, `${b.text} is ${b.w} by ${b.h}`);
  const focus = await focusOf(page);
  await forget(page);
  await page.locator(".touch-menu .touch-button", { hasText: "Open" }).click();
  await frames(page);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: URL_TEXT }], "the host is asked once, for the address");
  assert.deepEqual(await selects(page), [{ pane: 2, kind: "clear" }], "the selection is cleared");
  assert.equal(await page.locator(".touch-handle, .touch-menu").count(), 0, "the handles and the menu are gone");
  assert.equal(await focusOf(page), focus, "Open moves no focus");
  await show();
}

/** The host refuses: on a phone the sentence is the notice line, warn, with the `!` mark and an underline; the status line stays hidden. */
async function refusalCase(page, url) {
  const { show } = await pressed(page, url, ROW0, 12);
  await page.evaluate((text) => window.__MOCK__.on("open_url", () => { throw text; }), REFUSAL);
  await show([[0, START, END]]);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"]);
  await page.locator(".touch-menu .touch-button", { hasText: "Open" }).click();
  await page.waitForFunction(() => document.querySelector("#phone-notice .phone-notice-line") !== null);
  const line = await page.evaluate(() => {
    const n = document.querySelector("#phone-notice .phone-notice-line");
    const mark = n.querySelector(".phone-notice-mark");
    return { text: n.querySelector(".phone-notice-text")?.textContent, tone: n.dataset.tone, mark: mark?.textContent, decoration: getComputedStyle(n).textDecorationLine };
  });
  assert.equal(line.text, REFUSAL, "the host's sentence is the line");
  assert.equal(line.tone, "warn");
  assert.equal(line.mark, "!");
  assert.match(line.decoration, /underline/);
  assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById("status")).display !== "none"), false, "the status line stays hidden");
  assert.equal((await callsOf(page, "open_url")).length, 1);
  await show();
}
