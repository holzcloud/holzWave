// A notice on a phone (docs/design.md 6.6, the paragraph Notices): the phone draws no status line, so a notice that the status line
// would hold is a line over the top of the stage. A terminal link that the host refuses shows its sentence there with the `!` mark, an
// underline and the warn colour, at least 44 px high, while the status line stays hidden. The platform is mocked (`app_info`
// answers `android`) and the window is sized like a phone in the desktop engines; a phone with a hardware keyboard is played by the
// keyboard route of the page (hint mode with Shift). Nothing here was run on a real phone or with a real host.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";
import { boot as bootChat, sendText } from "./ai-lib.mjs";
import { frame, open, shown } from "./touch-lib.mjs";

// The phone layout is the Android shell's; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the phone layout belongs to the Android shell, which the web backend never is";

const REFUSAL = "No app on this device can open this link.";
const URL_TEXT = "https://example.com/a";

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(6000);
  for (const [name, run] of [
    ["a refused terminal link", terminalLinkCase],
    ["a refused list widget link", widgetLinkCase],
    ["a refused Take over", takeoverCase],
    ["the life of the line", lifeCase],
    ["the cues", cuesCase],
    ["elsewhere", elsewhereCase],
    ["the stack with the hand-over line", stackCase],
    ["no double sentence", noDoubleCase],
  ]) {
    try {
      await run(page, url, engine);
    } catch (e) {
      // The case is named, and the line of the test that failed is kept, because the runner prints one line.
      const where = String(e?.stack ?? "").split("\n").find((l) => l.includes("phone-notice.test.mjs"))?.trim() ?? "";
      throw new Error(`${name}: ${e?.message ?? e} ${where}`);
    }
  }
  assert.deepEqual(errors, [], `the page reported errors (${engine})`);
}

/** The line of the phone, as the page draws it, or null while there is none. */
const lineOf = (page) =>
  page.evaluate(() => {
    const n = document.querySelector("#phone-notice .phone-notice-line");
    if (!n) return null;
    const r = n.getBoundingClientRect();
    const st = getComputedStyle(n);
    const mark = n.querySelector(".phone-notice-mark");
    return {
      text: n.querySelector(".phone-notice-text")?.textContent ?? null,
      tone: n.dataset.tone,
      mark: mark ? { text: mark.textContent, hidden: mark.getAttribute("aria-hidden") } : null,
      decoration: st.textDecorationLine,
      height: r.height,
      top: r.top,
      bottom: r.bottom,
      border: st.borderTopWidth,
      animation: st.animationName,
      transition: st.transitionDuration,
      children: n.children.length,
    };
  });

const statusShown = (page) => page.evaluate(() => getComputedStyle(document.getElementById("status")).display !== "none");

/** One row of 50 cells with `text` in it. */
const rowWith = (y, text, cols = 50) => ({ y, runs: [[text.padEnd(cols), cols, 0xf6efe6, 0x14091a, 0]] });

/**
 * A printed address in the one pane of a phone, and `open_url` answering with `sentence` as a refusal. The address is opened by hint
 * mode and Shift plus its label (a phone with a hardware keyboard); a touch gesture for a link is not built.
 */
async function terminalLinkCase(page, url) {
  await open(page, url);
  const g = await shown(page, 2);
  await emit(page, "frame", frame(2, g.cols, g.rows, [], { changed: [rowWith(0, `see ${URL_TEXT} now`, g.cols)] }));
  await page.evaluate((text) => window.__MOCK__.on("open_url", () => { throw text; }), REFUSAL);
  await page.evaluate(() => document.getElementById("term").focus());
  assert.equal(await statusShown(page), false, "the status line is not drawn on a phone");

  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("hints.open"));
  await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  const label = await page.evaluate(() => document.querySelector("#overlay [data-hint]").textContent);
  await page.keyboard.press(`Shift+${label}`);
  await page.waitForFunction(() => document.querySelector("#phone-notice .phone-notice-line") !== null);

  assert.deepEqual((await callsOf(page, "open_url")).map((a) => a.url), [URL_TEXT], "the host was asked once");
  const line = await lineOf(page);
  assert.equal(line.text, REFUSAL, "the host's sentence is the line");
  assert.equal(line.tone, "warn", "a refused link is the warn tone");
  assert.deepEqual(line.mark, { text: "!", hidden: "true" }, "the mark is `!` and is hidden from assistive technology");
  assert.match(line.decoration, /underline/, "an underline besides the colour");
  assert.ok(line.height >= 44, `the line is ${line.height} px high, under 44 px`);
  assert.equal(await statusShown(page), false, "the status line stays hidden on a phone");
}

/** Say `text` in the notice, as a part of the page does. */
const say = (page, text, tone) =>
  page.evaluate(async ([t, k]) => (await import(new URL("status.js", location.href).href)).showNotice(t, k), [text, tone]);
const noticeText = (page) => page.evaluate(() => document.querySelector("#status .seg.notice")?.textContent ?? "");
const lineCount = (page) => page.evaluate(() => document.querySelectorAll(".phone-notice-line").length);

/**
 * A refused link of a list widget of the sidebar. The phone draws no sidebar column and a person cannot reach the widget there, so the
 * widget is made with the action that toggles the sidebar and its button is clicked from the page: the handler under test is the
 * widget's own, the one that sends `open_url` and says a refusal.
 */
async function widgetLinkCase(page, url) {
  const widgets = { widgets: [{ id: "deploy", title: "Deploy", kind: "list", items: [{ label: "Docs", url: "https://example.org/docs" }] }], notes: {}, warnings: [] };
  await page.addInitScript((w) => window.__MOCK__.on("widgets_list", () => structuredClone(w)), widgets);
  await open(page, url);
  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("sidebar.toggle"));
  await page.waitForFunction(() => document.querySelector(".sb-link") !== null);
  const click = () => page.evaluate(() => document.querySelector(".sb-link").click());
  for (const [rejection, want] of [
    ["Links cannot be opened on this device.", "Links cannot be opened on this device."],
    [{ code: 7 }, "The link could not be opened."],
  ]) {
    await say(page, "");
    await page.evaluate((r) => window.__MOCK__.on("open_url", () => { throw r; }), rejection);
    await click();
    await page.waitForFunction((w) => document.querySelector("#phone-notice .phone-notice-text")?.textContent === w, want);
    const line = await lineOf(page);
    assert.equal(line.tone, "warn");
    assert.deepEqual(line.mark, { text: "!", hidden: "true" });
    assert.match(line.decoration, /underline/);
    assert.ok(line.height >= 44, `the line is ${line.height} px high`);
  }
}

/** A refused `Take over` in the top strip of a phone, reached by a tap: the host's sentence, plain. */
async function takeoverCase(page, url) {
  await open(page, url);
  await page.evaluate(() => window.__MOCK__.remoteSessions.setPane(2, { remote: { writer: "free" } }));
  await page.waitForSelector("#phone-chip .ph-take");
  await page.evaluate(() => window.__MOCK__.remoteSessions.fails("pane_takeover", "The server cannot be reached."));
  await page.locator("#phone-chip .ph-take").click();
  await page.waitForFunction(() => document.querySelector("#phone-notice .phone-notice-text")?.textContent === "The server cannot be reached.");
  const line = await lineOf(page);
  assert.equal(line.tone, "plain", "a refused Take over is a plain notice");
  assert.equal(line.mark, null, "no mark");
  assert.ok(!/underline/.test(line.decoration), "no underline");
  assert.ok(line.height >= 44);
}

/** The life of the line: replaced, tapped away, emptied, gone after its time; no focus moved; text only. */
async function lifeCase(page, url) {
  await open(page, url);
  await page.evaluate(() => document.getElementById("ime")?.focus());
  const focused = () => page.evaluate(() => document.activeElement?.id ?? null);
  const before = await focused();
  assert.equal(before, "ime", "the terminal input has the focus");

  await say(page, "first");
  await say(page, "second", "warn");
  await page.waitForFunction(() => document.querySelector(".phone-notice-text")?.textContent === "second");
  assert.equal(await lineCount(page), 1, "a newer notice replaces the line");
  assert.equal(await focused(), before, "showing a notice moves no focus");
  await say(page, "third");
  await page.waitForFunction(() => document.querySelector(".phone-notice-text")?.textContent === "third");
  assert.equal((await lineOf(page)).mark, null, "the mark goes with the warn tone");

  await page.locator(".phone-notice-line").click();
  assert.equal(await lineCount(page), 0, "a tap takes the line away");
  assert.equal(await noticeText(page), "", "and the notice with it");

  await say(page, "to be emptied");
  assert.equal(await lineCount(page), 1);
  await say(page, "");
  assert.equal(await lineCount(page), 0, "an empty notice removes the line");

  await say(page, "<img src=x onerror=alert(1)>", "warn");
  await page.waitForFunction(() => document.querySelector(".phone-notice-text") !== null);
  const made = await page.evaluate(() => ({ text: document.querySelector(".phone-notice-text").textContent, elements: document.querySelector(".phone-notice-line").querySelectorAll("img, script").length }));
  assert.equal(made.text, "<img src=x onerror=alert(1)>", "a sentence is text");
  assert.equal(made.elements, 0, "and makes no element");

  // Left alone, the line goes after the usual time of a notice.
  await say(page, "for a while");
  await page.waitForFunction(() => document.querySelector(".phone-notice-line") === null, null, { timeout: 8000 });
  assert.equal(await noticeText(page), "");
}

/** The cues of the warn line stay in forced colours and high contrast; nothing in the line moves. */
async function cuesCase(page, url) {
  await open(page, url);
  for (const [media, value] of [["forcedColors", "active"], ["contrast", "more"]]) {
    await page.emulateMedia({ [media]: value });
    if (media === "contrast") await page.waitForFunction(() => document.documentElement.classList.contains("contrast-high"));
    await say(page, REFUSAL, "warn");
    await page.waitForSelector(".phone-notice-line");
    const line = await lineOf(page);
    const where = `${media} ${value}`;
    assert.ok(parseFloat(line.border) >= 1, `${where}: the line keeps its border (${line.border})`);
    assert.deepEqual(line.mark, { text: "!", hidden: "true" }, `${where}: the mark stays`);
    assert.match(line.decoration, /underline/, `${where}: the underline stays`);
    assert.ok(line.height >= 44, `${where}: still 44 px`);
    assert.equal(line.animation, "none", `${where}: no animation`);
    assert.match(line.transition, /^0s(, 0s)*$/, `${where}: no transition`);
    if (media === "forcedColors") assert.equal(line.border, "2px", "a refusal has the 2 px border of the system's highlight");
    await say(page, "");
  }
  await page.emulateMedia({ forcedColors: null, contrast: "no-preference" });
}

/** The desktop, the browser build's page on a desktop and the tablet keep the notice in the status line and make no line. */
async function elsewhereCase(page, url) {
  const wide = { width: 1000, height: 700, screen: [1000, 700], shown: 3 };
  for (const [what, options] of [["a macOS desktop", { os: "macos", ...wide }], ["an Android tablet", wide]]) {
    await open(page, url, options);
    await say(page, REFUSAL, "warn");
    await page.waitForFunction((w) => document.querySelector("#status .seg.notice")?.textContent === w, REFUSAL);
    assert.equal(await page.evaluate(() => document.getElementById("phone-notice")), null, `${what}: no place for a phone line`);
    assert.equal(await lineCount(page), 0, `${what}: no line`);
    assert.equal(await page.evaluate(() => document.querySelector("#status .seg.notice").children.length), 0, `${what}: the notice is text`);
  }

  // A phone whose class turns to tablet while a line shows: the line goes and the notice is in the status line.
  await open(page, url);
  await say(page, REFUSAL, "warn");
  await page.waitForSelector(".phone-notice-line");
  assert.equal(await noticeText(page), REFUSAL, "the notice is in the hidden status line too");
  await page.evaluate(() => {
    for (const [key, value] of [["width", 1000], ["height", 700], ["availWidth", 1000], ["availHeight", 700]]) {
      Object.defineProperty(window.screen, key, { get: () => value, configurable: true });
    }
  });
  await page.setViewportSize({ width: 1000, height: 700 });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-tablet"));
  await page.waitForFunction(() => document.getElementById("phone-notice") === null);
  assert.equal(await lineCount(page), 0, "the line went with the class");
  assert.equal(await noticeText(page), REFUSAL, "the notice shows in the status line");
}

/** The line after a file hand-over and the notice show at once: one column, the hand-over line first, neither covering the other. */
async function stackCase(page, url) {
  const done = { kind: "done", name: "report.pdf", size: 1288490, shown: "~/handover/report.pdf", renamed: false };
  await page.addInitScript((answer) => {
    window.__MOCK__.on("handover_in", () => structuredClone(answer));
    window.__MOCK__.on("vault_status", () => ({ state: "none", protection: "plain", has_screen_lock: false, strongbox: false, can_protect: false, notice: "none", reason: null }));
    window.__MOCK__.on("vault_hidden", () => null);
  }, done);
  await open(page, url);
  await page.waitForFunction(async () => (await import(new URL("actions.js", location.href).href)).listActions().some((a) => a.id === "files.copy_in"));
  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("files.copy_in"));
  await page.waitForSelector("#handover-top .handover-line");
  await say(page, REFUSAL, "warn");
  await page.waitForSelector("#phone-notice .phone-notice-line");
  const boxes = await page.evaluate(() => {
    const r = (n) => n.getBoundingClientRect().toJSON();
    const top = document.getElementById("stage-top");
    return {
      stage: r(document.getElementById("stage")),
      hand: r(document.querySelector("#handover-top .handover-line")),
      note: r(document.querySelector("#phone-notice .phone-notice-line")),
      inside: top.contains(document.querySelector(".handover-line")) && top.contains(document.querySelector(".phone-notice-line")),
      zones: [document.querySelector(".handover-line"), document.querySelector(".phone-notice-line")].map((n) => {
        const b = n.getBoundingClientRect();
        return document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2) === n || n.contains(document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2));
      }),
    };
  });
  assert.ok(boxes.inside, "both lines are in the column at the top of the stage");
  assert.ok(boxes.hand.bottom <= boxes.note.top, `the hand-over line (${boxes.hand.bottom}) ends at or above the notice (${boxes.note.top})`);
  for (const b of [boxes.hand, boxes.note]) assert.ok(b.top >= boxes.stage.top && b.bottom <= boxes.stage.bottom && b.left >= boxes.stage.left && b.right <= boxes.stage.right, "inside the stage");
  assert.deepEqual(boxes.zones, [true, true], "each line is what a finger on it reaches");

  // A tap on one leaves the other in place.
  await page.locator(".phone-notice-line").click();
  assert.equal(await lineCount(page), 0);
  assert.equal(await page.locator("#handover-top .handover-line").count(), 1, "the hand-over line stays");
  await say(page, "again");
  await page.locator("#handover-top .handover-line").click();
  assert.equal(await page.locator("#handover-top .handover-line").count(), 0);
  assert.equal(await lineCount(page), 1, "the notice stays");
}

/** A sentence that the chat says in its own line is not said again in the notice line. */
async function noDoubleCase(page, url) {
  const one = {
    workspaces: [{ name: "main", active: true, panes: 1 }],
    tabs: [{ title: "shell", active: true, focus: 1, panes: 1 }],
    focus: 1,
    panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]],
    dividers: [],
    pane_names: [],
    windows: 1,
  };
  await page.setViewportSize({ width: 390, height: 844 });
  // The helper's page script clears the mock's answers first, so the platform is put in again after that clear.
  await page.addInitScript(() => {
    for (const [key, value] of [["width", 390], ["height", 844], ["availWidth", 390], ["availHeight", 844]]) {
      Object.defineProperty(window.screen, key, { get: () => value, configurable: true });
    }
    const mock = window.__MOCK__;
    const clear = mock.reset.bind(mock);
    mock.reset = () => {
      clear();
      mock.on("app_info", () => ({ version: "0.0.1-alpha1", os: "android", hostname: "pixel" }));
    };
  });
  await bootChat(page, url, { view: one, starts: 1 });
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains("layout-phone")), true, "the phone layout");
  await page.evaluate(() => window.__registry.runAction("ai.open"));
  await page.waitForSelector(".block-chat:not(.side-chat) textarea.chat-input");
  await page.evaluate(() => window.__MOCK__.on("markdown_render", () => '<p><a href="https://example.org/docs">docs</a></p>'));
  await sendText(page, "links");
  await page.waitForSelector(".chat-md a");
  await page.evaluate((r) => window.__MOCK__.on("open_url", () => { throw r; }), REFUSAL);
  await page.locator(".chat-md a").click();
  await page.waitForFunction((want) => document.querySelector(".chat-status")?.textContent === want, REFUSAL);
  assert.equal(await page.locator(".chat-status").getAttribute("data-kind"), "error");
  await page.waitForTimeout(300);
  assert.equal(await lineCount(page), 0, "the sentence is said once, in the chat's line, and not in the notice line");
  assert.equal(await noticeText(page), "", "nor in the notice");
}
