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
import { box, down, forget, frame, frames, golden, logWheel, move, open, run, selects, shown, up, wait } from "./touch-lib.mjs";

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
    ["a word, a path, a hash and another scheme get no Open", notLinkCase],
    ["marked links: at once, the question, and the schemes that get no Open", markedCase],
    ["Open leaves when the selection stops being the link", leavesCase],
    ["the cues under forced colours and high contrast", cuesCase],
    ["the tablet", tabletCase],
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

/** Another long press of 560 ms on the cell (`cx`, 0) of the pane, after the commands so far are forgotten. */
async function pressAgain(page, g, cx) {
  await forget(page);
  await run(page, [down(1, g.pt(cx, 0)), wait(560), up(1, g.pt(cx, 0))]);
}

/** Set the person's list of link schemes, as the settings event does, and wait until the page holds it. */
async function setSchemes(page, schemes) {
  const view = structuredClone(golden.settings_view);
  view.settings.link_schemes = schemes;
  await emit(page, "settings", view);
  await page.waitForFunction(
    async (want) => JSON.stringify((await import(new URL("state.js", location.href).href)).state.settings?.settings.link_schemes) === JSON.stringify(want),
    schemes,
  );
}

/** The two buttons of the question as text and size, or null while there is no question. */
const question = (page) =>
  page.evaluate(() => {
    const d = document.querySelector(".dlg-link");
    if (!d) return null;
    const size = (b) => {
      const r = b.getBoundingClientRect();
      const st = getComputedStyle(b);
      return { text: b.textContent, w: r.width, h: r.height, border: st.borderTopWidth, style: st.borderTopStyle };
    };
    return { title: d.querySelector(".dlg-title")?.textContent, buttons: [...d.querySelectorAll(".dlg-actions button")].map(size) };
  });

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

/**
 * A long press on a word, a printed path, a hash and a printed address of another scheme keeps the word selection, and the menu is exactly
 * Copy and Paste. Program text never becomes an element: a row that holds `<b>x</b>` makes no `b` element in the overlay.
 */
async function notLinkCase(page, url) {
  const text = "go /etc/hosts deadbeef12 ftp://example.com/x <b>x</b>";
  const { g, show } = await pressed(page, url, text, 0);
  for (const [what, word, first] of [["a word", "go", true], ["a path", "/etc/hosts"], ["a hash", "deadbeef12"], ["another scheme", "ftp://example.com/x"]]) {
    const at = text.indexOf(word) + 1;
    if (!first) await pressAgain(page, g, at);
    const sent = await selects(page);
    assert.deepEqual(sent, [{ pane: 2, kind: "begin", x: first ? 0 : at, y: 0, mode: "word" }], `${what}: the word selection, as before`);
    await show([[0, text.indexOf(word), text.indexOf(word) + 1]]);
    assert.deepEqual(await menuAfter(page), ["Copy", "Paste"], `${what}: no Open`);
    assert.equal((await page.locator(".touch-menu").textContent()).trim(), "CopyPaste", `${what}: the menu holds the two words only`);
    assert.equal(await page.evaluate(() => document.querySelectorAll("#overlay b").length), 0, `${what}: no element made from the row`);
    await show();
    await page.locator(".touch-menu").waitFor({ state: "detached" });
  }
  assert.equal((await callsOf(page, "open_url")).length, 0);
}

/**
 * Marked links: one whose text is its address opens at once with its pane; one whose text names another place asks first, with buttons of
 * 44 px, and Cancel opens nothing; a marked `file` link, a marked link of another scheme in the list and a `javascript` link get no Open.
 */
async function markedCase(page, url) {
  const z = "https://example.com/z";
  const { g, show } = await pressed(page, url, z, 5, [[0, z.length - 1, z]]);
  assert.deepEqual(await selects(page), [
    { pane: 2, kind: "begin", x: 0, y: 0, mode: "cell" },
    { pane: 2, kind: "extend", x: z.length - 1, y: 0 },
  ], "the marked link's cells");
  await show([[0, 0, z.length - 1]]);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"]);
  await forget(page);
  await page.locator(".touch-menu .touch-button", { hasText: "Open" }).click();
  await frames(page);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: z, pane: 2 }], "the text is the address: it opens at once, with its pane");
  assert.equal(await page.locator(".dlg-link").count(), 0, "no question");

  const docsText = "read the docs now";
  const docs = "https://example.org/docs";
  const docsLinks = [[9, 12, docs]];
  await show([], docsText, docsLinks);
  await pressAgain(page, g, 10);
  await show([[0, 9, 12]], docsText, docsLinks);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"]);
  await page.locator(".touch-menu .touch-button", { hasText: "Open" }).click();
  await page.locator(".dlg-link").waitFor({ state: "visible" });
  const asked = await question(page);
  assert.equal(asked.title, "Open this link?");
  assert.deepEqual(asked.buttons.map((b) => b.text), ["Cancel", "Open"]);
  for (const b of asked.buttons) assert.ok(b.w >= 43.99 && b.h >= 43.99, `the question's ${b.text} is ${b.w} by ${b.h}`);
  await page.locator(".dlg-link .dlg-cancel").click();
  await page.locator(".dlg-link").waitFor({ state: "detached" });
  assert.equal((await callsOf(page, "open_url")).length, 0, "Cancel opens nothing");
  await show([], docsText, docsLinks);
  await pressAgain(page, g, 11);
  await show([[0, 9, 12]], docsText, docsLinks);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"]);
  await page.locator(".touch-menu .touch-button", { hasText: "Open" }).click();
  await page.locator(".dlg-link .dlg-open").click();
  await frames(page);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: docs, pane: 2 }], "the question's Open opens the address once, with its pane");

  const plain = "the hosts file";
  for (const [what, uri, schemes] of [
    ["a marked file link", "file:///etc/hosts", null],
    ["a marked link of another scheme in the list", "ssh://host/x", ["http", "https", "mailto", "file", "ssh"]],
    ["a javascript link", "javascript:alert(1)", ["http", "https", "mailto", "file", "javascript"]],
  ]) {
    if (schemes) await setSchemes(page, schemes);
    await show([], plain, [[4, 8, uri]]);
    await pressAgain(page, g, 5);
    assert.deepEqual(await selects(page), [{ pane: 2, kind: "begin", x: 5, y: 0, mode: "word" }], `${what}: the word selection`);
    await show([[0, 4, 8]], plain, [[4, 8, uri]]);
    assert.deepEqual(await menuAfter(page), ["Copy", "Paste"], `${what}: no Open`);
    await show([], plain, [[4, 8, uri]]);
    await page.locator(".touch-menu").waitFor({ state: "detached" });
  }
  assert.equal((await callsOf(page, "open_url")).length, 0);
}

/**
 * Open stays only while the selection covers exactly the link: (a) the program writes other text under the same span, (b) the end handle
 * moves the selection two cells past the link. In both the menu is Copy and Paste again and nothing is opened.
 */
async function leavesCase(page, url) {
  const { g, show } = await pressed(page, url, ROW0, 10);
  await show([[0, START, END]]);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"]);
  await show([[0, START, END]], "see https://example.org/b now");
  await frames(page);
  assert.deepEqual(await menuWords(page), ["Copy", "Paste"], "(a) other text under the selection: Open goes");
  await show([[0, START, END]]);
  await frames(page);
  assert.deepEqual(await menuWords(page), ["Copy", "Paste"], "(a) and it does not come back");

  await show();
  await page.locator(".touch-menu").waitFor({ state: "detached" });
  await pressAgain(page, g, 10);
  await show([[0, START, END]]);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"]);
  const end = await box(page, ".touch-handle[data-role=end]");
  const tip = [end.x + end.w / 2, end.y + 18];
  const to = [tip[0] + 2 * g.cw, tip[1]];
  await forget(page);
  await run(page, [down(1, tip, ".touch-handle[data-role=end]"), move(1, to, ".touch-handle[data-role=end]", 20), up(1, to, ".touch-handle[data-role=end]")]);
  await frames(page);
  assert.deepEqual(await selects(page), [{ pane: 2, kind: "extend", x: END + 2, y: 0 }], "the end handle extends two cells");
  await show([[0, START, END + 2]]);
  await frames(page);
  assert.deepEqual(await menuWords(page), ["Copy", "Paste"], "(b) the selection is longer than the link: Open goes");
  assert.equal((await callsOf(page, "open_url")).length, 0, "nothing was opened");
  await show();
}

/**
 * Under forced colours and under high contrast the three buttons stay 44 px with the hairline between them, the question's buttons stay
 * 44 px with a border, and nothing in the menu moves.
 */
async function cuesCase(page, url) {
  for (const [media, value] of [["forcedColors", "active"], ["contrast", "more"]]) {
    const where = `${media} ${value}`;
    await page.emulateMedia({ [media]: value });
    const docsText = "read the docs now";
    const links = [[9, 12, "https://example.org/docs"]];
    const { show } = await pressed(page, url, docsText, 10, links);
    if (media === "contrast") await page.waitForFunction(() => document.documentElement.classList.contains("contrast-high"));
    await show([[0, 9, 12]], docsText, links);
    assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"], where);
    const menu = await page.evaluate(() => {
      const m = document.querySelector(".touch-menu");
      const bs = [...m.querySelectorAll(".touch-button")];
      const st = (n) => getComputedStyle(n);
      return {
        buttons: bs.map((b) => ({ w: b.getBoundingClientRect().width, h: b.getBoundingClientRect().height, left: st(b).borderLeftWidth, style: st(b).borderLeftStyle })),
        moves: [m, ...bs].map((n) => [st(n).animationName, st(n).transitionDuration]),
      };
    });
    for (const [i, b] of menu.buttons.entries()) {
      assert.ok(b.w >= 43.99 && b.h >= 43.99, `${where}: button ${i + 1} is ${b.w} by ${b.h}`);
      if (i > 0) assert.ok(b.left === "1px" && b.style !== "none", `${where}: button ${i + 1} keeps the hairline before it (${b.left} ${b.style})`);
    }
    for (const [anim, trans] of menu.moves) {
      assert.equal(anim, "none", `${where}: no animation`);
      assert.match(trans, /^0s(, 0s)*$/, `${where}: no transition`);
    }
    await page.locator(".touch-menu .touch-button", { hasText: "Open" }).click();
    await page.locator(".dlg-link").waitFor({ state: "visible" });
    for (const b of (await question(page)).buttons) {
      assert.ok(b.w >= 43.99 && b.h >= 43.99, `${where}: the question's ${b.text} is ${b.w} by ${b.h}`);
      assert.ok(parseFloat(b.border) >= 1 && b.style !== "none", `${where}: the question's ${b.text} keeps its border`);
    }
    await page.locator(".dlg-link .dlg-cancel").click();
    await page.locator(".dlg-link").waitFor({ state: "detached" });
    await show();
  }
  await page.emulateMedia({ forcedColors: null, contrast: "no-preference" });
}

/**
 * The tablet: a long press on a printed address in the focused pane offers Open, a refusal is said in the status line and makes no phone
 * line, and the question's buttons are 44 px there too.
 */
async function tabletCase(page, url) {
  const TABLET = { width: 1280, height: 800, screen: [1280, 800], shown: 3 };
  const { g, show } = await pressed(page, url, ROW0, 10, undefined, TABLET);
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-tablet"));
  assert.deepEqual(await selects(page), [
    { pane: 2, kind: "begin", x: START, y: 0, mode: "cell" },
    { pane: 2, kind: "extend", x: END, y: 0 },
  ], "the address's cells");
  await page.evaluate((text) => window.__MOCK__.on("open_url", () => { throw text; }), REFUSAL);
  await show([[0, START, END]]);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"]);
  await page.locator(".touch-menu .touch-button", { hasText: "Open" }).click();
  await page.waitForFunction((w) => document.querySelector("#status .seg.notice")?.textContent === w, REFUSAL);
  assert.equal(await page.evaluate(() => document.querySelectorAll(".phone-notice-line").length), 0, "no phone line on a tablet");

  const docsText = "read the docs now";
  const links = [[9, 12, "https://example.org/docs"]];
  await show([], docsText, links);
  await pressAgain(page, g, 10);
  await show([[0, 9, 12]], docsText, links);
  assert.deepEqual(await menuAfter(page), ["Open", "Copy", "Paste"]);
  await page.locator(".touch-menu .touch-button", { hasText: "Open" }).click();
  await page.locator(".dlg-link").waitFor({ state: "visible" });
  for (const b of (await question(page)).buttons) assert.ok(b.w >= 43.99 && b.h >= 43.99, `tablet: the question's ${b.text} is ${b.w} by ${b.h}`);
  await page.locator(".dlg-link .dlg-cancel").click();
  await page.locator(".dlg-link").waitFor({ state: "detached" });
  await show();
}
