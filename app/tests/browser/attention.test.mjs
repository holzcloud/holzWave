// The attention marks of the bar (docs/design.md section 6.23): a pane that is out of view needs the person, and the tab
// and the pill that hold it carry one glyph per kind, with the words in the title and the accessible name. This file
// holds the cases of the bell (the first source) and of the machinery every source shares: where the marks sit, what
// clears them, the side-by-side rule with all four kinds on one tab, and the second cue.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";

export default async function ({ page, url }) {
  await bellCase(page, url);
  await clearCase(page, url);
  await floodCase(page, url);
  await structureCase(page, url);
  await sideBySideCase(page, url);
  await accessibilityCase(page, url);
}

/** Two workspaces: alpha (shown) with the tabs one (pane 1), two (pane 2) and three (pane 3); beta holds pane 4. */
const VIEW = {
  workspaces: [
    { name: "alpha", active: true, panes: 3, pane_ids: [1, 2, 3] },
    { name: "beta", active: false, panes: 1, pane_ids: [4] },
  ],
  tabs: [
    { title: "one", active: true, focus: 1, panes: 1, pane_ids: [1] },
    { title: "two", active: false, focus: 2, panes: 1, pane_ids: [2] },
    { title: "three", active: false, focus: 3, panes: 1, pane_ids: [3] },
  ],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]],
  dividers: [],
};

/** `VIEW` with tab `index` of alpha shown (the shown tab's panes are in view). */
function shownTab(index) {
  const v = structuredClone(VIEW);
  v.tabs.forEach((t, i) => (t.active = i === index));
  v.focus = v.tabs[index].focus;
  v.panes = [[v.focus, { x: 0, y: 0, w: 1, h: 1 }]];
  return v;
}

const INFO = { version: "0.0.1-alpha1", os: "linux", hostname: "woodbox" };

async function open(page, url) {
  await page.addInitScript((i) => window.__MOCK__.on("app_info", () => i), INFO);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await showView(page, VIEW);
}

async function showView(page, view) {
  await page.evaluate((v) => window.__MOCK__.setView(v), view);
  await emit(page, "layout", view);
}

const texts = (page, selector) => page.locator(selector).allTextContents();

/** What the bar says about its marks, per tab and per pill. */
const marks = (page) =>
  page.evaluate(() => {
    const read = (item) => ({
      glyphs: item.querySelector(".attn")?.textContent ?? null,
      kinds: [...item.querySelectorAll(".attn .mk")].map((n) => n.getAttribute("data-kind")),
      attention: item.getAttribute("data-attention"),
      title: item.firstElementChild.getAttribute("title"),
      label: item.firstElementChild.getAttribute("aria-label"),
    });
    return {
      tabs: [...document.querySelectorAll("#bar .tab-item")].map(read),
      pills: [...document.querySelectorAll("#bar .ws-item")].map(read),
    };
  });

/** Ring the bell of a pane the way the core tells the page. */
const bell = (page, pane) => emit(page, "bell", { pane });

async function bellCase(page, url) {
  await open(page, url);
  let m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "", ""], "no mark before anything happened");
  assert.deepEqual(m.pills.map((t) => t.glyphs), ["", ""]);
  assert.equal(m.tabs[0].attention, null);

  // A bell in a pane of the second tab: a triangle on that tab, nothing on the first, nothing on the pill of the shown workspace.
  await bell(page, 2);
  m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "▲", ""]);
  assert.deepEqual(m.tabs[1].kinds, ["bell"]);
  assert.equal(m.tabs[1].attention, "bell");
  assert.deepEqual(m.pills.map((t) => t.glyphs), ["", ""], "the pill of the shown workspace carries no mark: its tabs do");
  // The second cue: the words, in the title and in the accessible name.
  assert.equal(m.tabs[1].title, "2 two · rang the bell");
  assert.equal(m.tabs[1].label, "2 two, rang the bell");
  assert.equal(m.tabs[0].title, "1 one", "a tab without a mark keeps its plain title");
  assert.equal(m.tabs[0].label, null, "and no accessible name of its own");
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 one", "2 two", "3 three"], "a mark does not change the text of the tab");

  // A bell in a pane of the other workspace: the triangle on its pill, and its words.
  await bell(page, 4);
  m = await marks(page);
  assert.deepEqual(m.pills.map((t) => t.glyphs), ["", "▲"]);
  assert.equal(m.pills[1].title, "beta · rang the bell");
  assert.equal(m.pills[1].label, "beta, rang the bell");
  assert.equal(m.pills[0].title, null, "a pill without a mark has no title of its own");
  assert.deepEqual(await texts(page, "#bar .ws-chip"), ["alpha", "beta"]);

  // A bell in a pane in view marks nothing.
  await bell(page, 1);
  m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "▲", ""], "pane 1 is in view");
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 one", "2 two", "3 three"]);
}

async function clearCase(page, url) {
  await open(page, url);
  await bell(page, 2);
  await bell(page, 3);
  await bell(page, 4);
  let m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "▲", "▲"]);

  // A click on the tab asks the core to show it; the layout that shows it clears its mark, and the others stay.
  await page.locator("#bar .tab").nth(1).click();
  assert.deepEqual((await callsOf(page, "select_tab")).at(-1), { index: 1 });
  await showView(page, shownTab(1));
  m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "", "▲"], "the shown tab lost its mark");
  assert.equal(m.tabs[1].attention, null);
  assert.equal(m.tabs[1].label, null);
  assert.deepEqual(m.pills.map((t) => t.glyphs), ["", "▲"], "the other workspace keeps its mark");

  // The bell of a pane that is in view marks nothing.
  await bell(page, 2);
  m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "", "▲"]);

  // Going back to the first tab marks nothing by itself: the person saw pane 2.
  await showView(page, VIEW);
  m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "", "▲"]);

  // A pane that ends takes its mark with it.
  await emit(page, "pane-exit", { pane: 3, code: 0 });
  m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "", ""], "the exit of the pane cleared its mark");
  // A pane that is gone latches nothing any more.
  await bell(page, 3);
  m = await marks(page);
  assert.deepEqual(m.tabs.map((t) => t.glyphs), ["", "", ""]);

  // A pane that no tab holds any more loses its mark with the next layout.
  const shrunk = structuredClone(VIEW);
  shrunk.workspaces[1].pane_ids = [];
  shrunk.workspaces[1].panes = 0;
  await showView(page, shrunk);
  m = await marks(page);
  assert.deepEqual(m.pills.map((t) => t.glyphs), ["", ""], "pane 4 is in no tab");

  // A click on the mark itself is a click on its tab.
  await bell(page, 2);
  const before = (await callsOf(page, "select_tab")).length;
  await page.locator("#bar .tab-item").nth(1).locator(".attn").click();
  const calls = await callsOf(page, "select_tab");
  assert.equal(calls.length, before + 1, "a press on the mark selects the tab");
  assert.deepEqual(calls.at(-1), { index: 1 });
}

async function floodCase(page, url) {
  await open(page, url);
  await page.evaluate(() => {
    for (let i = 0; i < 60; i++) window.__MOCK__.emit("bell", { pane: 2 });
  });
  const m = await marks(page);
  assert.deepEqual(m.tabs[1].kinds, ["bell"], "sixty bells leave one mark");
  assert.equal(m.tabs[1].glyphs, "▲");
  // A malformed event changes nothing and throws nothing.
  for (const bad of [null, {}, { pane: "2" }, { pane: 1.5 }, { pane: Number.NaN }, "2"]) await emit(page, "bell", bad);
  assert.deepEqual((await marks(page)).tabs.map((t) => t.glyphs), ["", "▲", ""]);
}

async function structureCase(page, url) {
  await open(page, url);
  await bell(page, 2);
  await bell(page, 4);
  const s = await page.evaluate(() => {
    const out = {};
    const tab = document.querySelectorAll("#bar .tab-item")[1];
    const pill = document.querySelectorAll("#bar .ws-item")[1];
    for (const [name, item] of [["tab", tab], ["pill", pill]]) {
      const kids = [...item.children];
      const attn = item.querySelector(".attn");
      const mk = attn.querySelector(".mk");
      const cs = getComputedStyle(mk);
      out[name] = {
        order: kids.map((n) => n.className.split(" ")[0] || n.tagName),
        hidden: attn.getAttribute("aria-hidden"),
        // Nothing sets a style attribute (the CSP forbids it).
        styled: [...item.querySelectorAll("[style]")].length,
        weight: cs.fontWeight,
        family: cs.fontFamily,
        transition: cs.transitionDuration,
        colour: cs.color,
        radius: getComputedStyle(item).borderRadius,
        rect: item.getBoundingClientRect().toJSON(),
        closeRect: item.querySelector(".close").getBoundingClientRect().toJSON(),
        attnRect: attn.getBoundingClientRect().toJSON(),
        scrollOverflow: item.scrollWidth - item.clientWidth,
      };
    }
    // The colour of a token, as the browser computes it.
    const probe = (token) => {
      const n = document.createElement("span");
      n.style.color = `var(${token})`;
      document.body.append(n);
      const c = getComputedStyle(n).color;
      n.remove();
      return c;
    };
    out.warn = probe("--hw-warn");
    out.rose = probe("--hw-rose");
    out.roseDeep = probe("--hw-rose-deep");
    out.diamond = getComputedStyle(pill.querySelector(".ws-chip"), "::before").content;
    out.number = document.querySelectorAll("#bar .tab")[1].textContent;
    out.tabRadius = getComputedStyle(tab).borderRadius;
    out.plainPill = document.querySelectorAll("#bar .ws-item")[0].getBoundingClientRect().height;
    out.styledAnywhere = document.querySelectorAll("#bar .attn[style], #bar .mk[style]").length;
    return out;
  });
  for (const name of ["tab", "pill"]) {
    assert.deepEqual(s[name].order, ["tab", "attn", "close"].map((c) => (name === "pill" ? c.replace("tab", "ws-chip") : c)), `${name}: the marks sit after the name and before the close mark`);
    assert.equal(s[name].hidden, "true", `${name}: the marks are hidden from assistive technology (the words are on the button)`);
    assert.equal(s[name].styled, 0, `${name}: no style attribute`);
    assert.ok(Number(s[name].weight) >= 700, `${name}: bold`);
    assert.match(s[name].family, /JetBrains Mono/, `${name}: the bundled mono face`);
    assert.equal(s[name].transition, "0s", `${name}: the mark never animates`);
    assert.equal(s[name].colour, s.warn, `${name}: the bell is --hw-warn`);
    assert.notEqual(s[name].colour, s.rose, `${name}: never rose`);
    assert.notEqual(s[name].colour, s.roseDeep, `${name}: never rose`);
    assert.ok(s[name].attnRect.right <= s[name].closeRect.left + 0.5, `${name}: the marks end where the close mark begins`);
    assert.ok(s[name].scrollOverflow <= 1, `${name}: nothing clipped (${s[name].scrollOverflow})`);
  }
  // The levels keep their shapes: the pill is round with its diamond, the tab square and numbered.
  assert.equal(s.pill.radius, "999px", "the pill is round");
  assert.notEqual(s.diamond, "none", "the pill keeps its diamond");
  assert.equal(s.tabRadius, "0px", "the tab is square");
  assert.equal(s.number, "2 two", "the tab keeps its number");
  assert.ok(Math.abs(s.pill.rect.height - s.plainPill) <= 0.5, `the pill keeps its height (${s.pill.rect.height} against ${s.plainPill})`);
  assert.equal(s.styledAnywhere, 0);
}

/** All four kinds on one tab: side by side, one symbol per kind, in the fixed order, nothing clipped, 28 px kept. */
async function sideBySideCase(page, url) {
  await open(page, url);
  // The sources of the other kinds come in later steps; the store takes them already, and the page's own module is
  // the one this import returns (the same URL).
  await page.evaluate(async () => {
    const { attention } = await import("./attentionstate.js");
    attention.provide("test", () => [2, 4]);
    attention.note(2, "done");
    attention.note(2, "failed");
    attention.note(2, "bell");
    attention.note(4, "failed");
    attention.note(4, "done");
  });
  const m = await marks(page);
  assert.equal(m.tabs[1].glyphs, "?▲!✓", "all four kinds side by side, in the fixed order");
  assert.deepEqual(m.tabs[1].kinds, ["ask", "bell", "failed", "done"]);
  assert.equal(m.tabs[1].attention, "ask bell failed done");
  assert.equal(m.tabs[1].title, "2 two · waits for you, rang the bell, command failed, command finished");
  assert.equal(m.tabs[1].label, "2 two, waits for you, rang the bell, command failed, command finished");
  assert.equal(m.pills[1].glyphs, "?!✓", "a pill shows the kinds of its workspace together, once each");
  assert.equal(m.pills[1].title, "beta · waits for you, command failed, command finished");

  const geometry = () =>
    page.evaluate(() => {
      const probe = (token) => {
        const n = document.createElement("span");
        n.style.color = `var(${token})`;
        document.body.append(n);
        const c = getComputedStyle(n).color;
        n.remove();
        return c;
      };
      const one = (item) => {
        const r = item.getBoundingClientRect();
        const attn = item.querySelector(".attn");
        const close = item.querySelector(".close").getBoundingClientRect();
        const main = item.firstElementChild.getBoundingClientRect();
        const mks = [...attn.querySelectorAll(".mk")].map((n) => ({
          rect: n.getBoundingClientRect().toJSON(),
          colour: getComputedStyle(n).color,
          kind: n.getAttribute("data-kind"),
        }));
        return {
          height: r.height, width: r.width, right: r.right, left: r.left,
          attn: attn.getBoundingClientRect().toJSON(),
          closeLeft: close.left, closeRight: close.right, mainWidth: main.width,
          overflow: item.scrollWidth - item.clientWidth,
          mks,
        };
      };
      return {
        tab: one(document.querySelectorAll("#bar .tab-item")[1]),
        pill: one(document.querySelectorAll("#bar .ws-item")[1]),
        colours: { ask: probe("--hw-info"), bell: probe("--hw-warn"), failed: probe("--hw-danger"), done: probe("--hw-ok") },
        rem: parseFloat(getComputedStyle(document.documentElement).fontSize),
        plainPill: document.querySelectorAll("#bar .ws-item")[0].getBoundingClientRect().height,
        plainTab: document.querySelectorAll("#bar .tab-item")[0].getBoundingClientRect().height,
      };
    });
  let g = await geometry();
  for (const name of ["tab", "pill"]) {
    const part = g[name];
    assert.deepEqual(part.mks.map((x) => x.kind), name === "tab" ? ["ask", "bell", "failed", "done"] : ["ask", "failed", "done"]);
    for (const x of part.mks) assert.equal(x.colour, g.colours[x.kind], `${name}: ${x.kind} has its own token`);
    // Side by side: each symbol starts where the previous one ended, none overlaps another or the close mark.
    for (let i = 1; i < part.mks.length; i++) {
      assert.ok(part.mks[i].rect.left >= part.mks[i - 1].rect.right - 0.5, `${name}: symbol ${i} overlaps the one before`);
    }
    assert.ok(part.attn.right <= part.closeLeft + 0.5, `${name}: the marks stay clear of the close mark`);
    assert.ok(part.attn.left >= part.left, `${name}: the marks stay inside the item`);
    assert.ok(part.overflow <= 1, `${name}: nothing is clipped (${part.overflow})`);
  }
  assert.ok(g.tab.height >= 28 * (g.rem / 12) - 1, `the tab keeps its 28 px target (${g.tab.height} at ${g.rem} px per rem)`);
  assert.ok(Math.abs(g.tab.height - g.plainTab) <= 0.5, "and its height");
  assert.ok(g.tab.attn.width <= 3.5 * g.rem, `four marks take about 3.4 rem (${g.tab.attn.width / g.rem} rem)`);
  assert.ok(Math.abs(g.pill.height - g.plainPill) <= 0.5, `the pill keeps its height (${g.pill.height} against ${g.plainPill})`);

  // A narrow window: the tab gives way by shortening its name, the marks and the close mark stay whole.
  await page.setViewportSize({ width: 380, height: 640 });
  g = await geometry();
  assert.ok(g.tab.overflow <= 1, `narrow: nothing is clipped (${g.tab.overflow})`);
  assert.ok(g.tab.attn.right <= g.tab.closeLeft + 0.5, "narrow: the marks stay clear of the close mark");
  assert.ok(g.tab.mainWidth >= 3 * g.rem - 1, `narrow: the name keeps its minimum (${g.tab.mainWidth})`);
  assert.equal(g.tab.mks.length, 4, "narrow: all four are still there");
  await page.setViewportSize({ width: 1000, height: 640 });
}

async function accessibilityCase(page, url) {
  await open(page, url);
  await bell(page, 2);
  // The words are the second cue and are there without any colour; the glyph is the first.
  const cue = await page.evaluate(() => {
    const item = document.querySelectorAll("#bar .tab-item")[1];
    return { text: item.querySelector(".attn").textContent, label: item.firstElementChild.getAttribute("aria-label") };
  });
  assert.equal(cue.text, "▲");
  assert.match(cue.label, /rang the bell/);

  // Forced colours: the glyph is still in the DOM, visible and not transparent; the browser gives it system colours.
  await page.emulateMedia({ forcedColors: "active" });
  const forced = await page.evaluate(() => {
    const mk = document.querySelector("#bar .tab-item:nth-of-type(2) .mk, #bar .attn .mk");
    const cs = getComputedStyle(mk);
    const r = mk.getBoundingClientRect();
    return { text: mk.textContent, display: cs.display, visibility: cs.visibility, width: r.width, colour: cs.color };
  });
  assert.equal(forced.text, "▲");
  assert.notEqual(forced.visibility, "hidden");
  assert.ok(forced.width > 0, "the glyph takes room under forced colours");
  assert.notEqual(forced.colour, "rgba(0, 0, 0, 0)");
  await page.emulateMedia({ forcedColors: null });

  // Reduced motion and higher contrast change nothing about the marks: they never move, and the status tokens hold.
  await page.emulateMedia({ reducedMotion: "reduce", contrast: "more" });
  const m = await marks(page);
  assert.equal(m.tabs[1].glyphs, "▲");
  const t = await page.evaluate(() => getComputedStyle(document.querySelector("#bar .attn .mk")).transitionDuration);
  assert.equal(t, "0s");
  await page.emulateMedia({ reducedMotion: null, contrast: "no-preference" });
}
