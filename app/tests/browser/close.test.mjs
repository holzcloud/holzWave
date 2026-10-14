// Close buttons (phase 2.1, plan 06): every tab, workspace chip and pane header has a × that is always visible and always
// asks first. The question is a page dialog with the name and the number of shells that end; Enter or Close sends one
// command, Escape or Cancel sends nothing, and no key of it reaches the terminal. A press on a × is neither a drag, nor a
// rename, nor a selection of the tab. Cmd+W and the palette actions ask nothing. The browser stub plays the core.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";

let rev = 100;

/** Two workspaces (alpha has four panes, beta one), three tabs of the shown workspace, and the two panes of the shown tab. */
function viewOf(over = {}) {
  return {
    rev: ++rev,
    workspaces: [{ name: "alpha", active: true, panes: 4 }, { name: "beta", active: false, panes: 1 }],
    tabs: [
      { title: "one", active: true, focus: 1, panes: 2 },
      { title: "two", active: false, focus: 3, panes: 1 },
      { title: "three", active: false, focus: 4, panes: 1 },
    ],
    focus: 1,
    panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
    dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
    pane_names: [],
    windows: 1,
    ...over,
  };
}

async function show(page, view) {
  await page.evaluate((v) => window.__MOCK__.setView(v), view);
  await emit(page, "layout", view);
}

async function open(page, url, view = viewOf()) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  await show(page, view);
  await page.waitForSelector("#bar .tab-item:nth-child(3)");
}

const dialog = (page) => page.locator(".dlg-close");
const tabClose = (page, i) => page.locator("#bar .tab-item").nth(i).locator("button.close");
const chipClose = (page, i) => page.locator("#bar .ws-item").nth(i).locator("button.close");
const headClose = (page, pane) => page.locator(`.panehead[data-pane="${pane}"] button.close`);

/** What would reach the shell or change the layout other than by a close command. */
const stray = (page) =>
  page.evaluate(() =>
    window.__MOCK__.calls
      .filter(([c]) => ["key", "text", "paste", "select_tab", "select_workspace", "focus_pane", "move_tab", "move_pane", "rename_tab", "rename_workspace", "rename_pane", "move_tab_to_workspace"].includes(c))
      .map(([c]) => c),
  );

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await visibleCase(page, url);
  await askCase(page, url);
  await wordsCase(page, url);
  await keysCase(page, url);
  await staleCase(page, url);
  await pressCase(page, url);
  await shortcutCase(page, url);
  await hostileCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A × on every tab, chip and header, always visible, with its accessible name, and the strip itself takes no pointer. */
async function visibleCase(page, url) {
  await open(page, url);
  const found = await page.evaluate(() => {
    const info = (n) => {
      const r = n.getBoundingClientRect();
      const s = getComputedStyle(n);
      return {
        name: n.getAttribute("aria-label"),
        title: n.title,
        text: n.textContent,
        w: r.width, h: r.height,
        visible: s.visibility === "visible" && s.display !== "none" && Number(s.opacity) === 1,
        events: s.pointerEvents,
        glyph: getComputedStyle(n, "::before").content,
        tag: n.tagName,
      };
    };
    const inside = (n, within) => {
      const a = n.getBoundingClientRect();
      const b = within.getBoundingClientRect();
      return a.left >= b.left - 0.5 && a.right <= b.right + 0.5 && a.top >= b.top - 0.5 && a.bottom <= b.bottom + 0.5;
    };
    return {
      tabs: [...document.querySelectorAll("#bar .tab-item")].map((i) => ({ ...info(i.querySelector("button.close")), fits: inside(i.querySelector("button.close"), document.getElementById("bar")) })),
      chips: [...document.querySelectorAll("#bar .ws-item")].map((i) => info(i.querySelector("button.close"))),
      heads: [...document.querySelectorAll(".panehead")].map((h) => ({ ...info(h.querySelector("button.close")), head: getComputedStyle(h).pointerEvents, fits: inside(h.querySelector("button.close"), h) })),
      nested: document.querySelectorAll("button button").length,
    };
  });
  assert.equal(found.tabs.length, 3, "a × on every tab");
  assert.equal(found.chips.length, 2, "a × on every workspace chip");
  assert.equal(found.heads.length, 2, "a × on every pane header");
  for (const [list, name] of [[found.tabs, "Close tab"], [found.chips, "Close workspace"], [found.heads, "Close pane"]]) {
    for (const x of list) {
      assert.equal(x.tag, "BUTTON");
      assert.equal(x.name, name, "the accessible name");
      assert.equal(x.title, name, "and the tooltip");
      assert.equal(x.text, "", "the glyph is drawn by the stylesheet: no stray character in any text");
      assert.match(x.glyph, /×/, "the glyph is a ×");
      assert.ok(x.visible, "always visible, without a hover");
      assert.ok(x.w >= 12 && x.h >= 12, `a target that can be hit: ${x.w} x ${x.h}`);
      assert.equal(x.events, "auto");
    }
  }
  assert.ok(found.tabs.every((x) => x.fits), "every tab × is inside the bar");
  assert.ok(found.heads.every((x) => x.fits && x.head === "none"), "the × is inside its header, which takes no pointer events itself");
  assert.equal(found.nested, 0, "no button holds another button");
  // The names and numbers of the tabs are what they were: the × adds no text to them.
  assert.deepEqual(await page.locator("#bar .tab").allTextContents(), ["1 one", "2 two", "3 three"]);
  assert.deepEqual(await page.locator("#bar .ws-chip").allTextContents(), ["alpha", "beta"]);
  assert.equal(await callsOf(page, "close_tab").then((c) => c.length), 0, "nothing closes by itself");

  // Tab and chip order for the keyboard: the × follows its tab.
  const order = await page.evaluate(() => [...document.querySelectorAll("#bar .bar-tabs button")].map((b) => b.className.split(" ")[0]));
  assert.deepEqual(order, ["tab", "close", "tab", "close", "tab", "close", "plus"]);
}

/** A click on a × asks; Escape, Cancel and a click outside keep everything; Enter and Close send one command. */
async function askCase(page, url) {
  await open(page, url);
  const answered = (cmd) => callsOf(page, cmd);

  // Tab: Escape.
  await tabClose(page, 1).click();
  await dialog(page).waitFor();
  assert.equal(await dialog(page).getAttribute("role"), "alertdialog");
  assert.equal(await page.locator(".dlg-close .dlg-title").textContent(), "Close tab “two”?");
  assert.deepEqual(await answered("close_tab"), [], "asked, not closed");
  assert.deepEqual(await stray(page), [], "the click did not select the tab");
  assert.equal(await page.evaluate(() => document.activeElement?.className), "dlg-ok", "the Close button has the focus");
  await page.keyboard.press("Escape");
  assert.equal(await dialog(page).count(), 0, "Escape closes the dialog");
  assert.deepEqual(await answered("close_tab"), [], "and keeps everything");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the terminal has the keyboard back");

  // Tab: Cancel, then a click outside the box.
  await tabClose(page, 1).click();
  await page.getByRole("button", { name: "Cancel" }).click();
  assert.equal(await dialog(page).count(), 0);
  await tabClose(page, 1).click();
  await dialog(page).waitFor();
  await page.mouse.click(5, 300);
  assert.equal(await dialog(page).count(), 1, "a press right after it opened is the second click of a double click: it stays");
  await page.waitForTimeout(600);
  await page.mouse.click(5, 300);
  assert.equal(await dialog(page).count(), 0, "a click outside is Cancel");
  assert.deepEqual(await answered("close_tab"), []);

  // Tab: Enter closes the tab at that index, once.
  await tabClose(page, 1).click();
  await dialog(page).waitFor();
  await page.keyboard.press("Enter");
  assert.equal(await dialog(page).count(), 0);
  assert.deepEqual(await answered("close_tab"), [{ index: 1 }], "one close_tab with the index of the tab");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");

  // Tab: the Close button; the last tab is another index.
  await tabClose(page, 2).click();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  assert.deepEqual(await answered("close_tab"), [{ index: 1 }, { index: 2 }]);

  // Workspace chip.
  await chipClose(page, 1).click();
  await dialog(page).waitFor();
  assert.equal(await page.locator(".dlg-close .dlg-title").textContent(), "Close workspace “beta”?");
  await page.keyboard.press("Escape");
  assert.deepEqual(await answered("close_workspace"), []);
  await chipClose(page, 1).click();
  await dialog(page).waitFor();
  await page.keyboard.press("Enter");
  assert.deepEqual(await answered("close_workspace"), [{ index: 1 }]);

  // Pane header: close_pane with the pane id, and no focus change on the way.
  await headClose(page, 2).click();
  await dialog(page).waitFor();
  assert.equal(await page.locator(".dlg-close .dlg-title").textContent(), "Close pane “one”?", "a pane with no title of its own shows its tab's");
  await page.keyboard.press("Escape");
  assert.deepEqual(await answered("close_pane"), []);
  await headClose(page, 2).click();
  await dialog(page).waitFor();
  await page.keyboard.press("Enter");
  assert.deepEqual(await answered("close_pane"), [{ pane: 2 }]);
  assert.deepEqual(await stray(page), [], "no select, no focus change, no drag: only the three close commands");
}

/** The words: the name, the shells that end, and what else ends when it is everything. */
async function wordsCase(page, url) {
  await open(page, url);
  const text = async () => ({
    title: await page.locator(".dlg-close .dlg-title").textContent(),
    body: await page.locator(".dlg-close .dlg-why").textContent(),
  });
  const cancel = async () => {
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
  };

  await tabClose(page, 0).click();
  await dialog(page).waitFor();
  assert.deepEqual(await text(), { title: "Close tab “one”?", body: "The 2 shells in it end." });
  await cancel();
  await tabClose(page, 1).click();
  await dialog(page).waitFor();
  assert.deepEqual(await text(), { title: "Close tab “two”?", body: "The shell in it ends." });
  await cancel();
  await chipClose(page, 0).click();
  await dialog(page).waitFor();
  assert.deepEqual(await text(), { title: "Close workspace “alpha”?", body: "The 4 shells in it end." });
  await cancel();

  // A name the user gave to a pane, and a program title, are what the header shows.
  await show(page, viewOf({ pane_names: [[2, "api"]] }));
  await emit(page, "title", { pane: 1, title: "vim README.md" });
  await headClose(page, 2).click();
  await dialog(page).waitFor();
  assert.equal((await text()).title, "Close pane “api”?");
  await cancel();
  await headClose(page, 1).click();
  await dialog(page).waitFor();
  assert.equal((await text()).title, "Close pane “vim README.md”?");
  await cancel();

  // Everything in the window: the app ends when it is the only window, the window closes when there are others.
  const lone = (windows) =>
    viewOf({
      workspaces: [{ name: "alpha", active: true, panes: 1 }],
      tabs: [{ title: "one", active: true, focus: 1, panes: 1 }],
      panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]],
      dividers: [],
      windows,
    });
  await show(page, lone(1));
  await headClose(page, 1).click();
  await dialog(page).waitFor();
  assert.equal((await text()).body, "The shell in it ends. It is everything holzWave has open, so holzWave quits.");
  await cancel();
  await tabClose(page, 0).click();
  await dialog(page).waitFor();
  assert.match((await text()).body, /holzWave quits\.$/);
  await cancel();
  await chipClose(page, 0).click();
  await dialog(page).waitFor();
  assert.match((await text()).body, /holzWave quits\.$/);
  await cancel();
  await show(page, lone(2));
  await headClose(page, 1).click();
  await dialog(page).waitFor();
  assert.equal((await text()).body, "The shell in it ends. It is everything in this window, so the window closes.");
  await cancel();
  assert.deepEqual(await callsOf(page, "close_pane"), [], "only questions so far");
}

/** While the question is open no key reaches the terminal and no shortcut runs; a held Enter does not answer it. */
async function keysCase(page, url) {
  await open(page, url);
  await tabClose(page, 1).click();
  await dialog(page).waitFor();
  await page.keyboard.type("hello");
  await page.keyboard.press("Meta+w");
  await page.keyboard.press("Meta+t");
  await page.keyboard.press("Meta+k");
  await page.keyboard.press("ArrowLeft");
  assert.equal(await dialog(page).count(), 1, "the question is still open");
  assert.deepEqual(await stray(page), [], "no key reached the terminal");
  assert.equal((await callsOf(page, "close_pane")).length + (await callsOf(page, "new_tab")).length, 0, "no shortcut ran");
  assert.equal(await page.locator(".palette-backdrop:not([hidden])").count(), 0, "the palette did not open on top of it");
  // Tab moves between the two buttons, Enter on Cancel keeps everything.
  await page.keyboard.press("Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.className), "dlg-cancel");
  await page.keyboard.press("Enter");
  assert.equal(await dialog(page).count(), 0);
  assert.deepEqual(await callsOf(page, "close_tab"), []);
  // Space on Close answers yes.
  await tabClose(page, 0).click();
  await dialog(page).waitFor();
  await page.keyboard.press("Space");
  assert.deepEqual(await callsOf(page, "close_tab"), [{ index: 0 }]);

}

/** What was asked about is checked again after the answer: a layout that changed meanwhile closes nothing else. */
async function staleCase(page, url) {
  await open(page, url);
  await tabClose(page, 1).click();
  await dialog(page).waitFor();
  // The tab at index 1 is another one now (the first tab closed on its own, say).
  await show(page, viewOf({ tabs: [{ title: "two", active: false, focus: 3, panes: 1 }, { title: "three", active: true, focus: 4, panes: 1 }] }));
  await page.keyboard.press("Enter");
  assert.equal(await dialog(page).count(), 0);
  assert.deepEqual(await callsOf(page, "close_tab"), [], "the tab at that index is not the one that was asked about");

  await chipClose(page, 1).click();
  await dialog(page).waitFor();
  await show(page, viewOf({ workspaces: [{ name: "alpha", active: true, panes: 4 }] }));
  await page.keyboard.press("Enter");
  assert.deepEqual(await callsOf(page, "close_workspace"), []);

  // A pane that is gone is not closed (the core would ignore the id, but nothing is sent).
  await show(page, viewOf());
  await headClose(page, 2).click();
  await dialog(page).waitFor();
  await show(page, viewOf({ panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]], dividers: [] }));
  await page.keyboard.press("Enter");
  assert.deepEqual(await callsOf(page, "close_pane"), []);
}

/** A press on a × is no press on the tab or the header: no drag, no rename, no selection, no focus change. */
async function pressCase(page, url) {
  await open(page, url);
  const centre = (b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });
  const outlines = () => page.locator(".drop-outline").count();

  // Pressing a tab ×, moving over the other tabs and the chips and letting go starts no drag.
  const from = centre(await tabClose(page, 0).boundingBox());
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + 40, from.y + 2, { steps: 4 });
  assert.equal(await outlines(), 0, "no drop outline over the other tabs");
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains("dragging")), false);
  await page.mouse.move(centre(await page.locator("#bar .ws-chip").nth(1).boundingBox()).x, from.y, { steps: 4 });
  await page.mouse.up();
  assert.deepEqual(await callsOf(page, "move_tab"), []);
  assert.deepEqual(await callsOf(page, "move_tab_to_workspace"), []);
  assert.equal(await dialog(page).count(), 0, "released elsewhere: no click on the ×, so no question");

  // Same for a header ×: no pane drag, no focus change.
  const head = centre(await headClose(page, 1).boundingBox());
  const target = centre(await headClose(page, 2).boundingBox());
  await page.mouse.move(head.x, head.y);
  await page.mouse.down();
  await page.mouse.move(target.x - 80, target.y + 150, { steps: 6 });
  assert.equal(await outlines(), 0, "no outline over the other pane");
  await page.mouse.up();
  assert.deepEqual(await callsOf(page, "move_pane"), []);
  assert.deepEqual(await callsOf(page, "release_outside"), []);
  assert.deepEqual(await stray(page), []);

  // A press on the tab's own label still drags (the × did not take that away).
  const label = centre(await page.locator("#bar .tab").nth(0).boundingBox());
  const over = centre(await page.locator("#bar .tab").nth(2).boundingBox());
  await page.mouse.move(label.x, label.y);
  await page.mouse.down();
  await page.mouse.move(over.x, over.y, { steps: 6 });
  assert.equal(await outlines(), 1, "the tab label is still a handle");
  await page.mouse.up();
  assert.deepEqual(await callsOf(page, "move_tab"), [{ from: 0, to: 2 }]);

  // A tab dropped on the × of another tab is a drop on that tab.
  await page.evaluate(() => window.__MOCK__.reset());
  await show(page, viewOf());
  const x2 = centre(await tabClose(page, 2).boundingBox());
  await page.mouse.move(label.x, label.y);
  await page.mouse.down();
  await page.mouse.move(x2.x, x2.y, { steps: 6 });
  assert.equal(await outlines(), 1, "an outline over the tab whose × the pointer is over");
  await page.mouse.up();
  assert.deepEqual(await callsOf(page, "move_tab"), [{ from: 0, to: 2 }]);

  // A double click on a × renames nothing, and asks once.
  await page.evaluate(() => window.__MOCK__.reset());
  await show(page, viewOf());
  await tabClose(page, 1).dblclick();
  assert.equal(await page.locator("input.rename-input").count(), 0, "no name editor");
  await dialog(page).waitFor();
  assert.equal(await dialog(page).count(), 1, "one question");
  await page.keyboard.press("Escape");
  await chipClose(page, 1).dblclick();
  assert.equal(await page.locator("input.rename-input").count(), 0);
  await dialog(page).waitFor();
  await page.keyboard.press("Escape");
  await headClose(page, 2).dblclick();
  assert.equal(await page.locator("input.rename-input").count(), 0);
  await dialog(page).waitFor();
  await page.keyboard.press("Escape");
  assert.deepEqual(await stray(page), [], "and selected nothing");
  assert.deepEqual(await callsOf(page, "close_tab"), []);
}

/** Cmd+W is as it was and asks nothing; the two new actions are in the palette and close at once, as a chord would. */
async function shortcutCase(page, url) {
  await open(page, url);
  await page.keyboard.press("Meta+w");
  assert.deepEqual(await callsOf(page, "close_pane"), [{ pane: 1 }], "Cmd+W closes the focused pane");
  assert.equal(await dialog(page).count(), 0, "and asks nothing");

  const run = async (query) => {
    await page.keyboard.press("Meta+k");
    await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
    await page.keyboard.type(query);
    await page.keyboard.press("Enter");
  };
  await show(page, viewOf({ tabs: [
    { title: "one", active: false, focus: 1, panes: 2 },
    { title: "two", active: true, focus: 3, panes: 1 },
    { title: "three", active: false, focus: 4, panes: 1 },
  ] }));
  await run("Close tab");
  assert.deepEqual(await callsOf(page, "close_tab"), [{ index: 1 }], "the shown tab");
  await run("Close workspace");
  assert.deepEqual(await callsOf(page, "close_workspace"), [{ index: 0 }], "the shown workspace");
  assert.equal(await dialog(page).count(), 0, "a chord or a palette entry is deliberate: no question");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");
}

/** A name from the core is text in the question, never markup. */
async function hostileCase(page, url) {
  const bad = '<img src=x onerror="window.__pwned=1"><b>bold</b>';
  await open(page, url);
  await show(page, viewOf({
    workspaces: [{ name: bad, active: true, panes: 4 }, { name: "beta", active: false, panes: 1 }],
    tabs: [{ title: bad, active: true, focus: 1, panes: 2 }, { title: "two", active: false, focus: 3, panes: 1 }, { title: "three", active: false, focus: 4, panes: 1 }],
  }));
  await tabClose(page, 0).click();
  await dialog(page).waitFor();
  assert.equal(await page.locator(".dlg-close img, .dlg-close b").count(), 0, "a name was parsed as markup");
  assert.equal(await page.locator(".dlg-close .dlg-title").textContent(), `Close tab “${Array.from(bad).slice(0, 40).join("")}…”?`); // the first 40 characters, as text
  await page.keyboard.press("Escape");
  await chipClose(page, 0).click();
  await dialog(page).waitFor();
  assert.equal(await page.locator(".dlg-close img, .dlg-close b").count(), 0);
  await page.keyboard.press("Escape");
  assert.equal(await page.evaluate(() => window.__pwned), undefined);
}
