// The header of a pane that shows a session of a server (docs/design.md 6.34): the writer label as a word and a shape, `Take over` when
// the pane does not write, the × that detaches without asking, the strip's pointer rules, and two panes of one session after a
// takeover. The stub plays the host: its `remoteSessions` hook puts the remote member into the answer of `pane_connections`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

// Both builds: the browser's worker shows a session in a pane of its window with the same header.
export const backends = ["tauri", "ws"];

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await labelCase(page, url);
  await takeoverCase(page, url);
  await detachCase(page, url);
  await stripCase(page, url);
  await twoPanesCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const head = (page, pane) => page.locator(`.panehead[data-pane="${pane}"]`);

/** Load the page; `panes` are the panes of a session as `setPane` takes them. */
async function open(page, url, panes = { 2: {} }) {
  await page.addInitScript((p) => {
    const sessions = window.__MOCK__.remoteSessions;
    sessions.reset();
    sessions.set([{ id: 1, title: "", cwd: "", cols: 80, rows: 24, started: 1_700_000_000, attached: 1, held: true }]);
    for (const [pane, partial] of Object.entries(p)) sessions.setPane(Number(pane), partial);
  }, panes);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

/** Change the remote member of `pane` in the stub and wait until the header shows `word` (or no label for null). */
async function setWriter(page, pane, remote, word) {
  await page.evaluate(([p, r]) => window.__MOCK__.remoteSessions.setPane(p, { remote: r }), [pane, remote]);
  await waitWord(page, pane, word);
}

async function waitWord(page, pane, word) {
  await page.waitForFunction(
    ([p, w]) => {
      const chip = document.querySelector(`.panehead[data-pane="${p}"] .ph-writer`);
      return w === null ? chip === null : chip?.textContent === w;
    },
    [pane, word],
    { timeout: 4000 },
  );
}

/** What the chip of `pane` looks like: its word, its shape, the square's fill and border, the chip's border and radius. */
const chipLook = (page, pane) =>
  head(page, pane).locator(".ph-writer").evaluate((chip) => {
    const sq = chip.querySelector(".ph-sq");
    const c = getComputedStyle(chip);
    const s = getComputedStyle(sq);
    return {
      word: chip.textContent,
      role: chip.getAttribute("role"),
      shape: sq.dataset.shape,
      filled: s.backgroundColor !== "rgba(0, 0, 0, 0)" && s.backgroundColor !== "transparent",
      squareBorder: s.borderTopStyle,
      squareWidth: parseFloat(s.borderTopWidth),
      chipBorder: c.borderTopStyle,
      radius: parseFloat(c.borderTopLeftRadius),
      height: chip.getBoundingClientRect().height,
      label: chip.classList.contains("label"),
    };
  });

async function labelCase(page, url) {
  await open(page, url, { 2: { remote: { writer: "writing" } } });
  await waitWord(page, 2, "WRITING");
  // An ordinary pane shows no chip, no button and keeps its × as it was.
  assert.equal(await head(page, 1).locator(".ph-writer, .ph-take").count(), 0);
  assert.equal(await head(page, 1).evaluate((h) => h.classList.contains("remote") || h.style.clipPath !== ""), false, "an ordinary header keeps its classes");
  assert.equal(await head(page, 1).locator(".ph-close").getAttribute("aria-label"), "Close pane");
  // The chip sits between the title and the context.
  const order = await head(page, 2).evaluate((h) => Array.from(h.children).map((n) => n.className));
  assert.deepEqual(order.slice(0, 3), ["ph-dot", "label ph-title", "label ph-sub"]);
  assert.ok(order.indexOf("label ph-writer") < order.indexOf("ph-ctx"), "the label comes before the context");

  const looks = {};
  for (const [writer, word] of [["writing", "WRITING"], ["watching", "WATCHING"], ["free", "FREE"]]) {
    await setWriter(page, 2, { writer }, word);
    looks[writer] = await chipLook(page, 2);
  }
  assert.deepEqual([looks.writing.shape, looks.writing.filled], ["filled", true], "WRITING: a filled square");
  assert.deepEqual([looks.watching.shape, looks.watching.filled, looks.watching.squareBorder], ["hollow", false, "solid"], "WATCHING: a hollow square");
  assert.deepEqual([looks.free.shape, looks.free.squareBorder, looks.free.chipBorder], ["dashed", "dashed", "dashed"], "FREE: a dashed square in a dashed border");
  for (const look of Object.values(looks)) {
    assert.ok(look.label && look.role === "status", "a .label chip that is a status region");
    assert.ok(look.radius < look.height / 2, "a square chip, never a round pill");
  }

  // Forced colours and higher contrast keep the word and the shape.
  for (const media of [{ forcedColors: "active" }, { contrast: "more" }]) {
    await page.emulateMedia(media);
    if (media.contrast) await page.waitForFunction(() => document.documentElement.classList.contains("contrast-high"));
    for (const [writer, word] of [["writing", "WRITING"], ["watching", "WATCHING"], ["free", "FREE"]]) {
      await setWriter(page, 2, { writer }, word);
      const look = await chipLook(page, 2);
      assert.equal(look.word, word);
      assert.ok(look.squareWidth >= 1 && look.squareBorder !== "none", `${word} keeps the square's border under ${JSON.stringify(media)}`);
      if (writer === "writing") assert.ok(look.filled, `WRITING stays filled under ${JSON.stringify(media)}`);
      if (writer === "free") assert.equal(look.squareBorder, "dashed", `FREE keeps its dashes under ${JSON.stringify(media)}`);
    }
    await page.emulateMedia({ forcedColors: "none", contrast: "no-preference" });
  }
}

async function takeoverCase(page, url) {
  await open(page, url, { 2: { remote: { writer: "watching" } } });
  await waitWord(page, 2, "WATCHING");
  const take = head(page, 2).locator(".ph-take");
  assert.equal(await take.textContent(), "Take over");
  const order = await head(page, 2).evaluate((h) => Array.from(h.children).map((n) => n.className));
  assert.equal(order.indexOf("label ph-take"), order.indexOf("label ph-writer") + 1, "the button follows the label");
  // The hit box: at least 28 px with a fine pointer, centred on the drawn button and inside the strip's reach.
  const reach = async () =>
    take.evaluate((b) => {
      const r = b.getBoundingClientRect();
      const x = r.left + r.width / 2;
      const mid = r.top + r.height / 2;
      const hits = (dy) => document.elementFromPoint(x, mid + dy) === b;
      let up = 0;
      while (up < 40 && hits(-(up + 1))) up += 1;
      let down = 0;
      while (down < 40 && hits(down + 1)) down += 1;
      return { hit: up + down + 1, drawn: r.height, strip: b.parentElement.getBoundingClientRect().height };
    });
  const fine = await reach();
  assert.ok(fine.hit >= 28, `the hit box is at least 28 px (${fine.hit})`);
  assert.ok(fine.drawn <= fine.strip, "the drawn button fits the strip");
  // A coarse pointer asks for 44 px (the sheet's rule, played here on the strip).
  const css = readFileSync(join(APP_DIR, "dist", "chrome.css"), "utf8");
  assert.match(css.slice(css.indexOf("@media (pointer: coarse)", css.indexOf(".ph-take"))), /^@media \(pointer: coarse\) \{\s*\.panehead\.remote \{ --ph-hit: max\(44px, calc\(44rem \/ 12\)\); \}/);
  await head(page, 2).evaluate((h) => h.style.setProperty("--ph-hit", "44px"));
  const coarse = await reach();
  assert.ok(coarse.hit >= 44, `the hit box is at least 44 px with a coarse pointer (${coarse.hit})`);
  await head(page, 2).evaluate((h) => h.style.removeProperty("--ph-hit"));
  // The keyboard reaches it (a real button in the tab order), with the 2 px ring of the sheet.
  await take.focus();
  const ring = await take.evaluate((b) => ({ focused: document.activeElement === b, tab: b.tabIndex }));
  assert.ok(ring.focused && ring.tab >= 0, "the button takes the keyboard focus");
  assert.match(css, /\.ph-take:focus-visible \{ outline: 2px solid var\(--hw-ink\); outline-offset: -2px; \}/, "the focus ring is 2 px");
  const visual = await head(page, 2).evaluate((h) => Array.from(h.querySelectorAll("button")).map((b) => b.className));
  assert.deepEqual(visual, ["label ph-take", "close ph-close"], "the focus order of the strip is its visual order");

  // A press calls the command once, also when the pointer moves a few pixels while it is down (a finger on a tablet): the button is
  // not a handle of the pane, so no drag starts and swallows the click. After the next poll the label changes and the button goes.
  const box = await take.boundingBox();
  await page.mouse.move(box.x + box.width / 2 + 3, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 - 3, box.y + box.height / 2, { steps: 4 });
  await page.mouse.up();
  assert.equal((await callsOf(page, "move_pane")).length, 0, "no pane moved");
  await waitWord(page, 2, "WRITING");
  assert.deepEqual((await callsOf(page, "pane_takeover")), [{ pane: 2 }]);
  assert.equal(await head(page, 2).locator(".ph-take").count(), 0, "no Take over while the pane writes");

  // A refusal is the host's fixed sentence in the status line.
  await setWriter(page, 2, { writer: "free" }, "FREE");
  await page.evaluate(() => window.__MOCK__.remoteSessions.fails("pane_takeover", "The server cannot be reached."));
  await head(page, 2).locator(".ph-take").click();
  await page.waitForFunction(() => document.querySelector("#status")?.textContent.includes("The server cannot be reached."));
  assert.equal(await head(page, 2).locator(".ph-writer").textContent(), "FREE", "a refusal changes nothing");
}

async function detachCase(page, url) {
  await open(page, url, { 2: { remote: { writer: "writing" } } });
  await waitWord(page, 2, "WRITING");
  const x = head(page, 2).locator(".ph-close");
  assert.equal(await x.getAttribute("title"), "Detach session");
  assert.equal(await x.getAttribute("aria-label"), "Detach session");
  await x.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("close_pane").length === 1);
  assert.deepEqual(await callsOf(page, "close_pane"), [{ pane: 2 }], "the × detaches at once through the close path of every pane");
  assert.equal(await page.locator(".dlg-close").count(), 0, "nothing is asked");
  assert.equal((await callsOf(page, "server_session_end")).length, 0, "the session is never ended by the ×");

  // The × of an ordinary pane still asks.
  await open(page, url, { 2: { remote: { writer: "writing" } } });
  await waitWord(page, 2, "WRITING");
  await head(page, 1).locator(".ph-close").click();
  await page.locator(".dlg-close").waitFor();
  await page.keyboard.press("Escape");
  await page.locator(".dlg-close").waitFor({ state: "detached" });
  assert.equal((await callsOf(page, "close_pane")).length, 0);

  // The key that closes a pane closes a remote pane the same way.
  await page.evaluate(async () => {
    const registry = await import(new URL("actions.js", location.href).href);
    await registry.runAction("pane.focus_next");
  });
  await page.waitForFunction(() => window.__MOCK__.view().focus === 2);
  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("pane.close"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("close_pane").length === 1);
  assert.deepEqual(await callsOf(page, "close_pane"), [{ pane: 2 }]);
  assert.equal(await page.locator(".dlg-close").count(), 0);
}

async function stripCase(page, url) {
  await open(page, url, { 2: { remote: { writer: "watching" } } });
  await waitWord(page, 2, "WATCHING");
  // A title that imitates the label stays a title: the label is its own element with its own class and shape.
  await emit(page, "title", { pane: 2, title: "WRITING " + "x".repeat(200) });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-title')?.textContent.startsWith("WRITING"));
  assert.equal(await head(page, 2).locator(".ph-writer").count(), 1);
  assert.equal(await head(page, 2).locator(".ph-writer").textContent(), "WATCHING");
  const box = await head(page, 2).evaluate((h) => {
    const r = h.getBoundingClientRect();
    const whole = (sel) => {
      const n = h.querySelector(sel);
      const b = n.getBoundingClientRect();
      return n.scrollWidth <= n.clientWidth + 1 && b.left >= r.left - 0.5 && b.right <= r.right + 0.5;
    };
    const ctx = h.querySelector(".ph-ctx");
    const centre = (sel) => {
      const b = h.querySelector(sel).getBoundingClientRect();
      return document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2);
    };
    return {
      height: r.height,
      header: parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--hw-header")),
      chip: whole(".ph-writer"),
      take: whole(".ph-take"),
      ctxCut: ctx.scrollWidth >= ctx.clientWidth,
      onChip: centre(".ph-writer")?.id,
      onCtx: centre(".ph-ctx")?.id,
      onTitle: centre(".ph-title")?.classList.contains("ph-title"),
      onTake: centre(".ph-take")?.classList.contains("ph-take"),
    };
  });
  assert.equal(box.height, box.header, "the strip never grows over the pane's content");
  assert.ok(box.chip && box.take, "the label and the button stay whole beside a long title");
  assert.equal(box.onChip, "term", "a click on the label reaches the terminal below");
  assert.equal(box.onCtx, "term", "a click on the context reaches the terminal below");
  assert.ok(box.onTitle && box.onTake, "the title and the button take the pointer");
  // A narrow tile: the context is cut first, the label and the button stay whole.
  await page.setViewportSize({ width: 560, height: 640 });
  await page.waitForTimeout(100);
  const narrow = await head(page, 2).evaluate((h) => {
    const r = h.getBoundingClientRect();
    const whole = (sel) => {
      const n = h.querySelector(sel);
      const b = n.getBoundingClientRect();
      return n.scrollWidth <= n.clientWidth + 1 && b.right <= r.right + 0.5;
    };
    return { chip: whole(".ph-writer"), take: whole(".ph-take") };
  });
  assert.deepEqual(narrow, { chip: true, take: true });
  await page.setViewportSize({ width: 1000, height: 640 });
}

async function twoPanesCase(page, url) {
  // Two panes of one session, standing in for two windows of two devices: a takeover moves the label of both within one poll.
  await open(page, url, { 1: { remote: { writer: "free" } }, 2: { remote: { writer: "free" } } });
  await waitWord(page, 1, "FREE");
  await waitWord(page, 2, "FREE");
  await head(page, 1).locator(".ph-take").click();
  await waitWord(page, 1, "WRITING");
  await waitWord(page, 2, "WATCHING");
  assert.equal(await head(page, 2).locator(".ph-take").textContent(), "Take over");
}
