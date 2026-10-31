// Links in the browser build: a link in the visible output opens only by a click with Ctrl or Cmd held, only for http, https
// and mailto, in a new tab that cannot reach the page (`noopener,noreferrer`), never in the page's own tab and never through
// the shell's `open_url`; a file path is not offered because a folder cannot be shown from a browser. The chat's links and the
// sidebar's items open the same way. The desktop's own click is covered by links.test.mjs, which runs unchanged.
import assert from "node:assert/strict";
import { boot, openAi, sendText } from "./ai-lib.mjs";
import { callsOf, emit } from "./lib.mjs";

export const backends = ["ws"];
export const backendNote = "the web paths of links exist only on the web backend; the desktop's are in links.test.mjs";

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const HEADER = 24; // CSS px of a pane's header strip at the base font size
const BG = 0x14091a;
const MODES = { alt: false, bracketed_paste: false, mouse: false };
const COLS = 60;

const URL_TEXT = "https://example.com/a?b=1";
const MAIL_TEXT = "mailto:me@example.org";
const FEATURES = "noopener,noreferrer";
const ROWS = [
  `see ${URL_TEXT} now`,
  "edit ./src/main.rs:12:3 ok",
  `write ${MAIL_TEXT} now`,
  "ftp://example.com/x javascript:alert(1) file:///etc/passwd",
  "commit 3f2a9c1 done",
  "nothing to click here",
];

const rowMsg = (y, text) => ({ y, runs: [[text.padEnd(COLS), COLS, 0xf6efe6, BG, 0]] });
const frame = (pane, seq, rows) => ({
  pane, seq, full: true, cols: COLS, rows: rows.length, fg: 0xf6efe6, bg: BG, cursor: null, scrollbar: [0, rows.length, rows.length],
  selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed: rows.map((t, y) => rowMsg(y, t)),
});

/** Record what the page opens: `window.open` is replaced, and every click on an anchor is recorded and stopped. */
const spyOpen = (page) =>
  page.addInitScript(() => {
    if (window.__linkSpy) return;
    window.__linkSpy = true;
    window.__opened = [];
    window.__anchors = [];
    window.open = (...args) => {
      window.__opened.push(args);
      return null;
    };
    // A plain click on the web build starts a copy-on-select write whose item fails when nothing is selected; the real
    // clipboard of Chromium leaves that rejection unhandled, so the page gets a clipboard that waits for the item as a browser does.
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async () => {},
        write: async (items) => {
          for (const item of items) for (const type of item.types) await item.getType(type);
        },
      },
    });
    document.addEventListener(
      "click",
      (e) => {
        const a = e.target instanceof Element ? e.target.closest("a") : null;
        if (!a) return;
        window.__anchors.push({ href: a.href, target: a.getAttribute("target"), rel: a.getAttribute("rel") });
        e.preventDefault();
      },
      true,
    );
  });

const opened = (page) => page.evaluate(() => window.__opened.map((a) => [...a]));
const anchors = (page) => page.evaluate(() => structuredClone(window.__anchors));
const clearOpened = (page) =>
  page.evaluate(() => {
    window.__opened.length = 0;
    window.__anchors.length = 0;
  });
const shellCalls = async (page) => (await callsOf(page, "open_url")).length + (await callsOf(page, "reveal_path")).length;

/** Open the page as the web build with `rows` in both panes. */
async function open(page, url, { rows = ROWS } = {}) {
  await spyOpen(page);
  await page.addInitScript(() => window.__MOCK__.on("app_info", () => ({ os: "web", version: "t", hostname: "h" })));
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
  const start = (await callsOf(page, "start_pane"))[0];
  const c = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });
  const origin = (pane) => c.left + (pane === 1 ? 0 : c.width / 2) + PAD;
  await emit(page, "frame", frame(1, 1, rows));
  await emit(page, "frame", frame(2, 1, rows));
  await page.evaluate(() => window.__MOCK__.reset());
  await clearOpened(page);
  return {
    pt: (pane, cx, cy) => [origin(pane) + (cx + 0.5) * start.cell_w, c.top + HEADER + (cy + 0.5) * start.cell_h],
  };
}

/** Hold `mod`, click at (x, y), release. */
async function modClick(page, mod, [x, y]) {
  await page.keyboard.down(mod);
  await page.mouse.click(x, y);
  await page.keyboard.up(mod);
}

const frames = (page, n = 2) =>
  page.evaluate(async (count) => {
    for (let i = 0; i < count; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  }, n);

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await clickCase(page, url);
  await refusedCase(page, url);
  await mailCase(page, url);
  await pathCase(page, url);
  await hoverCase(page, url);
  await hintCase(page, url);
  await chatCase(page, url);
  await sidebarCase(page, url);
  await unavailableCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Ctrl+click and Cmd+click open a new tab with the three-part feature string; a plain click and a drag open nothing. */
async function clickCase(page, url) {
  const g = await open(page, url);
  const here = page.url();

  await modClick(page, "Control", g.pt(2, 10, 0));
  assert.deepEqual(await opened(page), [[URL_TEXT, "_blank", FEATURES]], "Ctrl+click opens the URL once, in a new tab, cut off from the page");
  await clearOpened(page);
  await page.waitForTimeout(450);
  await modClick(page, "Meta", g.pt(2, 10, 0));
  assert.deepEqual(await opened(page), [[URL_TEXT, "_blank", FEATURES]], "Cmd+click the same");
  assert.equal(page.url(), here, "the page's own tab did not move");
  assert.deepEqual(await anchors(page), [], "no anchor was clicked for a web address");
  assert.equal(await shellCalls(page), 0, "the shell's open_url and reveal_path are never called on the web");

  // A plain click selects as it always did and opens nothing; so does a click with Shift or Alt.
  await clearOpened(page);
  await page.waitForTimeout(450);
  await page.mouse.click(...g.pt(2, 10, 0));
  await frames(page);
  await page.keyboard.down("Shift");
  await page.mouse.click(...g.pt(2, 10, 0));
  await page.keyboard.up("Shift");
  await frames(page);
  assert.deepEqual(await opened(page), [], "no modifier, no tab");
  assert.deepEqual(await anchors(page), []);

  // The press of the link is the link's: no selection and no focus change.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.waitForTimeout(450);
  await modClick(page, "Control", g.pt(2, 10, 0));
  await frames(page);
  assert.deepEqual((await callsOf(page, "select")).length, 0, "a link click starts no selection");
  assert.equal((await callsOf(page, "mouse")).length, 0, "and no report for the program");

  // Released somewhere else than it was pressed: not a click on the link.
  await clearOpened(page);
  await page.keyboard.down("Control");
  await page.mouse.move(...g.pt(2, 10, 0));
  await page.mouse.down();
  await page.mouse.move(...g.pt(2, 10, 5));
  await page.mouse.up();
  await page.keyboard.up("Control");
  await frames(page);
  assert.deepEqual(await opened(page), [], "a press on the link released elsewhere opens nothing");
}

/** Text that only looks like a link, and text that is none, opens nothing. */
async function refusedCase(page, url) {
  const g = await open(page, url);
  for (const col of [3, 12, 27, 40, 50]) {
    await page.waitForTimeout(450);
    await modClick(page, "Control", g.pt(2, col, 3)); // the row with ftp:, javascript: and file:
  }
  await modClick(page, "Control", g.pt(2, 5, 5)); // plain text
  await modClick(page, "Control", g.pt(2, 8, 4)); // a hash
  await frames(page);
  assert.deepEqual(await opened(page), [], "ftp:, javascript:, file: text, a hash and plain text are no targets");
  assert.deepEqual(await anchors(page), []);
  assert.equal(await shellCalls(page), 0);
}

/** mailto opens through an anchor without a target, so the page keeps its tab and the mail program answers. */
async function mailCase(page, url) {
  const g = await open(page, url);
  await modClick(page, "Control", g.pt(2, 10, 2));
  await frames(page);
  assert.deepEqual(await opened(page), [], "a mail address opens no tab");
  assert.deepEqual(await anchors(page), [{ href: MAIL_TEXT, target: null, rel: "noopener noreferrer" }], "one anchor click, no target");
  assert.equal(await shellCalls(page), 0);
}

/** A printed absolute or relative path is not offered: no underline, no click, no reveal_path. */
async function pathCase(page, url) {
  const g = await open(page, url);
  await emit(page, "cwd", { pane: 2, cwd: "/home/u/proj" });
  await page.keyboard.down("Control");
  await page.mouse.move(...g.pt(2, 8, 1));
  await frames(page, 3);
  assert.equal(await page.evaluate(() => document.querySelector("#overlay .linkhover")), null, "a path is not underlined");
  assert.notEqual(await page.evaluate(() => getComputedStyle(document.getElementById("term")).cursor), "pointer");
  await page.keyboard.up("Control");
  await modClick(page, "Control", g.pt(2, 8, 1));
  await frames(page);
  assert.equal((await callsOf(page, "reveal_path")).length, 0, "Ctrl+click on a path reveals nothing");
  assert.deepEqual(await opened(page), []);
  assert.deepEqual(await anchors(page), []);
}

/** The underline and the pointer cursor follow the modifier, Ctrl or Cmd, with or without a pointer move. */
async function hoverCase(page, url) {
  const g = await open(page, url);
  const hovered = () =>
    page.evaluate(() => {
      const n = document.querySelector("#overlay .linkhover");
      if (!n) return null;
      const s = getComputedStyle(n);
      return { underline: parseFloat(s.borderBottomWidth) };
    });
  const cursor = () => page.evaluate(() => getComputedStyle(document.getElementById("term")).cursor);

  await page.mouse.move(...g.pt(2, 10, 0));
  assert.equal(await hovered(), null, "no modifier, no underline");
  assert.notEqual(await cursor(), "pointer");
  for (const key of ["Control", "Meta"]) {
    await page.keyboard.down(key);
    await page.waitForFunction(() => document.querySelector("#overlay .linkhover") !== null);
    assert.ok((await hovered()).underline >= 1, `${key}: the cue is a line, not a colour`);
    assert.equal(await cursor(), "pointer", `${key}: the pointer cursor`);
    await page.mouse.move(...g.pt(2, 0, 5));
    assert.equal(await hovered(), null, `${key}: plain text is not underlined`);
    await page.mouse.move(...g.pt(2, 10, 0));
    await page.waitForFunction(() => document.querySelector("#overlay .linkhover") !== null);
    await page.keyboard.up(key);
    await page.waitForFunction(() => document.querySelector("#overlay .linkhover") === null);
    assert.notEqual(await cursor(), "pointer", `${key}: released`);
  }
}

/** Hint mode: Shift plus a label opens a URL in a new tab, never reveals a path, and calls nothing of the shell. */
async function hintCase(page, url) {
  await open(page, url);
  const runHints = async () => {
    await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("hints.open"));
    await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  };
  const labels = () => page.evaluate(() => [...document.querySelectorAll("#overlay [data-hint]")].map((n) => n.dataset.hint));
  await runHints();
  assert.deepEqual(await labels(), ["a", "b", "c", "d"], "the URL, the path, the address and the hash");
  await page.keyboard.press("Shift+a");
  assert.deepEqual(await opened(page), [[URL_TEXT, "_blank", FEATURES]], "Shift+a opens the URL in a new tab");
  await clearOpened(page);
  await runHints();
  await page.keyboard.press("Shift+b");
  assert.deepEqual(await labels(), [], "the mode ended");
  await runHints();
  await page.keyboard.press("Shift+c");
  assert.deepEqual(await anchors(page), [{ href: MAIL_TEXT, target: null, rel: "noopener noreferrer" }]);
  assert.deepEqual(await opened(page), [], "a path and an address opened no tab");
  assert.equal(await shellCalls(page), 0, "no hint reaches the shell's open_url or reveal_path");
}

/** The link of a finished AI reply opens the same way, by a plain click. */
async function chatCase(page, url) {
  await spyOpen(page);
  await boot(page, url);
  await page.evaluate(async () => {
    (await import(new URL("state.js", location.href).href)).state.os = "web";
  });
  await openAi(page);
  await page.evaluate(() => window.__MOCK__.on("markdown_render", () => '<p><a href="https://example.org/docs">docs</a></p>'));
  await sendText(page, "links");
  await page.waitForSelector(".chat-md");
  const here = page.url();
  await clearOpened(page);
  await page.locator('.chat-msg[data-role="assistant"] .chat-text a').click();
  assert.deepEqual(await opened(page), [["https://example.org/docs", "_blank", FEATURES]], "the reply's link opens in a new tab");
  assert.equal(page.url(), here, "the page's own tab did not move");
  assert.equal(await shellCalls(page), 0, "the shell's open_url is not called");
  await page.locator('.chat-msg[data-role="assistant"] .chat-text a').click({ button: "middle" });
  await page.waitForTimeout(100);
  assert.equal((await opened(page)).length, 1, "a middle click opens nothing");
}

/** A widget list item with an address opens the same way. */
async function sidebarCase(page, url) {
  await spyOpen(page);
  const widgets = {
    widgets: [{ id: "deploy", title: "Deploy", kind: "list", items: [{ label: "Docs", url: "https://example.org/docs" }] }],
    notes: { notes: "" },
    warnings: [],
  };
  await page.addInitScript((w) => {
    window.__MOCK__.on("app_info", () => ({ os: "web", version: "t", hostname: "h" }));
    window.__MOCK__.on("widgets_list", () => structuredClone(w));
    window.__MOCK__.on("open_url", () => null);
    window.__MOCK__.on("connections_list", () => []);
    window.__MOCK__.on("pane_connections", () => []);
    window.__MOCK__.on("history_search", () => []);
  }, widgets);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".sidebar-btn");
  await page.locator(".sidebar-btn").click();
  await page.waitForSelector("#sidebar:not([hidden])");
  await clearOpened(page);
  await page.getByRole("button", { name: /Docs/ }).click();
  assert.deepEqual(await opened(page), [["https://example.org/docs", "_blank", FEATURES]], "the item opens in a new tab");
  assert.equal(await shellCalls(page), 0, "the shell's open_url is not called");
}

const UNAVAILABLE = "Not available in the browser.";
const TEAR_OFF = ["Move pane to new window", "Move tab to new window"];
const notice = (page) => page.evaluate(() => document.querySelector("#status .notice")?.textContent ?? "");
const titlesOfRegistry = (page) =>
  page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).listActions().map((a) => a.title));
const run = (page, id) =>
  page.evaluate(async (i) => (await import(new URL("actions.js", location.href).href)).runAction(i), id);
const tearOffCalls = async (page) =>
  (await callsOf(page, "tear_off_pane")).length + (await callsOf(page, "tear_off_tab")).length +
  (await callsOf(page, "release_outside")).length + (await callsOf(page, "drag_over")).length + (await callsOf(page, "drag_end")).length;

/** The palette does not list the two tear-off entries, lists every other, and the registry still answers in words. */
async function unavailableCase(page, url) {
  await open(page, url);
  const all = await titlesOfRegistry(page);
  for (const t of TEAR_OFF) assert.ok(all.includes(t), `the registry holds "${t}"`);

  assert.equal(await run(page, "palette.open"), true);
  await page.waitForSelector(".palette-row");
  const listed = await page.evaluate(() => [...document.querySelectorAll(".palette-row .palette-title")].map((n) => n.textContent));
  for (const t of TEAR_OFF) assert.ok(!listed.includes(t), `the palette does not list "${t}"`);
  assert.deepEqual(
    [...listed].sort(),
    all.filter((t) => !TEAR_OFF.includes(t)).sort(),
    "every other action is listed",
  );
  await page.locator(".palette-input").fill("new window");
  assert.equal(await page.locator(".palette-row").count(), 0, "a search for the words finds neither entry");
  await page.keyboard.press("Escape");

  // Run by a stored shortcut or a script: the sentence, and no call of the shell.
  await page.evaluate(() => window.__MOCK__.reset());
  for (const id of ["pane.move_to_new_window", "tab.move_to_new_window"]) {
    assert.equal(await run(page, id), true, `${id} is still a registered action`);
    await page.waitForFunction((t) => document.querySelector("#status .notice")?.textContent === t, UNAVAILABLE);
    assert.equal(await notice(page), UNAVAILABLE);
  }
  assert.equal(await tearOffCalls(page), 0, "no tear-off call reached the shell");
  const panes = async () => page.evaluate(() => document.querySelectorAll(".panehead").length);
  const before = await panes();
  assert.ok(before >= 2);

  // A pane dragged out of the window and released: the same sentence, and it stays.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.waitForFunction(() => !document.querySelector("#status .notice")?.textContent, null, { timeout: 8000 }); // the earlier notice ran out
  const view = await page.evaluate(() => window.__MOCK__.view());
  const c = await page.locator("canvas").first().boundingBox();
  const unit = view.panes[0][1];
  const grip = { x: c.x + (unit.x + unit.w) * c.width - 60, y: c.y + unit.y * c.height + 8 };
  const width = await page.evaluate(() => window.innerWidth);
  await page.mouse.move(grip.x, grip.y);
  await page.mouse.down();
  await page.mouse.move(width + 40, grip.y + 10, { steps: 6 });
  await page.waitForTimeout(300);
  await page.mouse.up();
  await page.waitForFunction((t) => document.querySelector("#status .notice")?.textContent === t, UNAVAILABLE);
  assert.equal(await tearOffCalls(page), 0, "a drag released outside the page asks the shell for nothing, not even the outline");
  assert.equal((await callsOf(page, "move_pane")).length, 0, "and moves nothing");
  assert.equal(await panes(), before, "the pane stays where it was");
  assert.equal((await page.evaluate(() => window.__MOCK__.view())).panes.length, view.panes.length);
}
