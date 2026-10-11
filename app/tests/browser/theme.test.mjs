// Themes in the page (DESK-12, UI half): the core's theme becomes the chrome tokens and the cursor colour of both
// renderers, an action per listed theme asks the core to switch, a theme value that is not #rrggbb never reaches the
// page's styles, and a core that cannot answer leaves the built-in aubergine look. The browser stub plays the core.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  await bootCase(page, url);
  await switchCase(page, url, "?renderer=canvas");
  await switchCase(page, url, "");
  await listCase(page, url);
  await withoutSettingsCase(page, url);
  await hostileCase(page, url);
  await sourceCase();
  assert.deepEqual(errors, [], "the page reported errors");
}

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const HEADER = 20; // CSS px of a pane's header strip at the base font size

/** Papier as core/src/theme.rs defines it; the aubergine view is the golden sample. */
const PAPIER = {
  version: 1,
  name: "Papier",
  kind: "light",
  terminal: {
    foreground: "#14091A", background: "#FFF3F8", cursor: "#C23B72", selection: "#C23B72", selection_alpha: 0.25,
    ansi: [
      "#14091A", "#B3261E", "#2F7A1F", "#8A5A00", "#1F5FA8", "#B0306A", "#00746B", "#4F4455",
      "#7A6B80", "#C42F22", "#2A7A18", "#946000", "#2B66B5", "#C23B72", "#00796F", "#2E2433",
    ],
  },
  chrome: {
    ground: "#FFF3F8", ground_2: "#FFE6F0", pane: "#FFF9FB", edge: "#E5C6D5", ink: "#14091A", ink_2: "#5B4F5D",
    ink_3: "#6B5C70", rose: "#C23B72", rose_deep: "#C23B72", ok: "#2A7A18", warn: "#8A5A00", danger: "#B3261E",
    info: "#1F5FA8",
  },
};

/** The golden view with another active theme. */
function viewWith(theme) {
  const view = structuredClone(golden.settings_view);
  view.theme = structuredClone(theme);
  view.settings.theme = theme.name;
  return view;
}
const AUBERGINE = () => viewWith(golden.settings_view.theme);
const PAPIER_VIEW = () => viewWith(PAPIER);

/** Open the page against the stub; `settings` is what `get_settings` answers (a function throws, null is no answer). */
async function open(page, url, settings, query = "?renderer=canvas") {
  await page.addInitScript((s) => {
    window.__MOCK__.on("get_settings", () => {
      if (s === "throw") throw new Error("the core is not ready");
      return s;
    });
  }, settings);
  await page.goto(`${url}index.html${query}`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

/** What the page's styles say right now. */
const looks = (page) =>
  page.evaluate(() => {
    const root = document.documentElement;
    const css = getComputedStyle(root);
    return {
      ground: css.getPropertyValue("--hw-ground").trim(),
      rose: css.getPropertyValue("--hw-rose").trim(),
      selection: css.getPropertyValue("--hw-selection").trim(),
      scheme: css.colorScheme,
      body: getComputedStyle(document.body).backgroundColor,
      bar: getComputedStyle(document.getElementById("bar")).backgroundColor,
      display: getComputedStyle(document.body).display,
      styleAttr: root.getAttribute("style") ?? "",
    };
  });

/** The id and title of every registered action. */
const actionList = (page) =>
  page.evaluate(async () => {
    const { listActions } = await import(new URL("actions.js", location.href).href);
    return listActions().map((a) => [a.id, a.title]);
  });

/**
 * Deliver `events` to the page, wait for the draw they cause and read the colours (0xRRGGBB) at `points`, in one
 * page task: a WebGL canvas is only readable in the task of its draw (see frames.test.mjs).
 */
const paint = (page, events, points) =>
  page.evaluate(
    async ([evs, pts]) => {
      for (const [name, payload] of evs) window.__MOCK__.emit(name, payload);
      await new Promise((done) => requestAnimationFrame(() => done()));
      const c = document.getElementById("term");
      const k = c.width / c.clientWidth;
      const gl = c.getContext("webgl2");
      return pts.map(([x, y]) => {
        const px = Math.floor(x * k);
        const py = Math.floor(y * k);
        if (gl) {
          const out = new Uint8Array(4);
          gl.readPixels(px, gl.drawingBufferHeight - 1 - py, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out);
          return (out[0] << 16) | (out[1] << 8) | out[2];
        }
        const [r, g, b] = c.getContext("2d").getImageData(px, py, 1, 1).data;
        return (r << 16) | (g << 8) | b;
      });
    },
    [events, points],
  );

const hex = (n) => `#${n.toString(16).padStart(6, "0")}`;

/** A full frame of the focused pane (2) with a block cursor on a blank cell: that cell is the cursor colour. */
const cursorFrame = (seq, bg) => ({
  pane: 2, seq, full: true, cols: 20, rows: 4, fg: 0xffffff, bg,
  cursor: { x: 0, y: 0, style: "block", blink: false },
  scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null,
  modes: { alt: false, bracketed_paste: false, mouse: false }, changed: [],
});

/** The point inside the cursor cell (0, 0) of pane 2, in canvas CSS pixels. */
async function cursorPoint(page) {
  const start = (await callsOf(page, "start_pane")).find((a) => a.pane === 2);
  const width = await page.evaluate(() => document.getElementById("term").clientWidth);
  return [width / 2 + PAD + start.cell_w / 2, HEADER + start.cell_h / 2];
}

/** The page asks the core for its theme at start and applies it to the tokens and to the cursor. */
async function bootCase(page, url) {
  await open(page, url, PAPIER_VIEW());
  assert.equal((await callsOf(page, "get_settings")).length, 1, "get_settings is asked once at start-up");

  const l = await looks(page);
  assert.equal(l.ground, "#fff3f8", "--hw-ground follows the theme");
  assert.equal(l.rose, "#c23b72");
  assert.equal(l.selection, "rgb(194 59 114 / 0.25)");
  assert.equal(l.scheme, "light", "a light theme switches the form controls and scrollbars to light");
  assert.equal(l.body, "rgb(255, 243, 248)", "the page background is the theme's ground");
  assert.equal(l.bar, "rgb(255, 230, 240)", "the bar is the theme's ground-2");

  const [x, y] = await cursorPoint(page);
  const [cursor] = await paint(page, [["frame", cursorFrame(1, 0xfff3f8)]], [[x, y]]);
  assert.equal(hex(cursor), "#c23b72", "the cursor is drawn in the theme's cursor colour from the first frame");
}

/** An action per theme; running one asks the core, and the `settings` event it answers with recolours both. */
async function switchCase(page, url, query) {
  const label = query || "webgl";
  await open(page, url, AUBERGINE(), query);
  const [x, y] = await cursorPoint(page);

  let [cursor] = await paint(page, [["frame", cursorFrame(1, 0x14091a)]], [[x, y]]);
  assert.equal(hex(cursor), "#ff8fb8", `${label}: the aubergine cursor`);
  assert.equal((await looks(page)).ground, "#14091a");

  const list = await actionList(page);
  const themes = list.filter(([id]) => id.startsWith("theme.set."));
  assert.deepEqual(
    themes.map(([, title]) => title),
    ["Theme: aubergine", "Theme: Schiefer", "Theme: Papier"],
    "one action per listed theme, in the core's order",
  );

  const id = themes.find(([, title]) => title === "Theme: Papier")[0];
  await page.evaluate(async (actionId) => {
    const { runAction } = await import(new URL("actions.js", location.href).href);
    runAction(actionId);
  }, id);
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 1);
  assert.deepEqual((await callsOf(page, "set_setting")).at(-1), { key: "theme", value: "Papier" });
  // Nothing changed yet: the page shows what the core says, not what was asked for.
  assert.equal((await looks(page)).ground, "#14091a", `${label}: the tokens wait for the core`);

  [cursor] = await paint(page, [["settings", PAPIER_VIEW()]], [[x, y]]);
  const l = await looks(page);
  assert.equal(l.ground, "#fff3f8", `${label}: --hw-ground after the settings event`);
  assert.equal(l.body, "rgb(255, 243, 248)");
  assert.equal(hex(cursor), "#c23b72", `${label}: the cursor colour changed with the theme`);

  // And back: the same event path recolours again.
  [cursor] = await paint(page, [["settings", AUBERGINE()]], [[x, y]]);
  assert.equal((await looks(page)).ground, "#14091a");
  assert.equal(hex(cursor), "#ff8fb8", `${label}: back to the aubergine cursor`);
}

/** The theme actions follow the list the core sends: new themes appear, vanished ones go. */
async function listCase(page, url) {
  await open(page, url, AUBERGINE());
  const view = AUBERGINE();
  view.themes = [
    { name: "aubergine", kind: "dark", builtin: true },
    { name: "Papier", kind: "light", builtin: true },
    { name: "Mine", kind: "dark", builtin: false },
  ];
  await emit(page, "settings", view);
  const titles = (await actionList(page)).filter(([id]) => id.startsWith("theme.set.")).map(([, t]) => t);
  assert.deepEqual(titles, ["Theme: aubergine", "Theme: Papier", "Theme: Mine"]);
}

/** No answer, a failing core or an answer that is not a settings view: the page starts with the built-in look. */
async function withoutSettingsCase(page, url) {
  for (const answer of [null, "throw", { settings: 5 }, "text"]) {
    await open(page, url, answer);
    const l = await looks(page);
    assert.equal(l.ground, "#14091a", `with ${JSON.stringify(answer)}: the built-in ground`);
    assert.equal(l.bar, "rgb(29, 15, 38)", `with ${JSON.stringify(answer)}: the built-in bar`);
    assert.equal(l.scheme, "dark");
    const titles = (await actionList(page)).filter(([id]) => id.startsWith("theme.set."));
    assert.deepEqual(titles, [], "no theme to offer without a list");
    // The page works: panes were started and the divider is there.
    assert.ok((await callsOf(page, "start_pane")).length >= 2);
  }
}

/** A theme value that is not #rrggbb is dropped before it can reach a style, whatever it carries. */
async function hostileCase(page, url) {
  await open(page, url, AUBERGINE());
  const view = AUBERGINE();
  view.theme.chrome.ground = "red;}body{display:none}";
  view.theme.chrome.ground_2 = "url(http://example.test/x)";
  view.theme.chrome.ink = "#12345";
  view.theme.chrome.rose = "#aabbcc";
  view.theme.terminal.cursor = "javascript:alert(1)";
  view.theme.kind = "dark; display: none";
  await emit(page, "settings", view);
  const l = await looks(page);
  assert.equal(l.ground, "#14091a", "a hostile ground was not applied");
  assert.equal(l.bar, "rgb(29, 15, 38)", "a hostile ground-2 was not applied");
  assert.equal(l.rose, "#aabbcc", "the valid values of the same theme are applied");
  assert.notEqual(l.display, "none", "the page is still shown");
  assert.doesNotMatch(l.styleAttr, /url\(|display|javascript|example\.test/, `the root style carries: ${l.styleAttr}`);
  assert.equal(l.scheme, "dark", "an unknown kind leaves the colour scheme alone");

  // The cursor colour of an invalid value stays what it was.
  const [x, y] = await cursorPoint(page);
  const [cursor] = await paint(page, [["frame", cursorFrame(1, 0x14091a)]], [[x, y]]);
  assert.equal(hex(cursor), "#ff8fb8", "an invalid cursor colour was not used");
}

/** No source file writes a style or markup from a string (T-02-71). */
async function sourceCase() {
  const dist = join(APP_DIR, "dist");
  for (const name of ["settings.js", "settings-model.js"]) {
    const text = readFileSync(join(dist, name), "utf8");
    assert.doesNotMatch(text, /cssText|innerHTML|outerHTML|insertAdjacentHTML|setAttribute\(\s*["']style/, `${name}`);
  }
}
