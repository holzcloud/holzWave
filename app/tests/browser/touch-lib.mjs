// Shared helpers of the touch browser tests (touch.test.mjs, touchscale.test.mjs): the page opened as an Android device would be, synthetic
// touch pointer events dispatched into the desktop engines, and the geometry of the cells. Not a test itself.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

export const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));

export const BG = 0x14091a;
export const MODES = { alt: false, bracketed_paste: false, mouse: false };

/** One workspace and one tab of three panes side by side, the second focused, as the phone test has it. */
export const VIEW = {
  rev: 1,
  workspaces: [{ name: "main", active: true, panes: 3, pane_ids: [1, 2, 3] }],
  tabs: [{ title: "shell", active: true, focus: 2, panes: 3, pane_ids: [1, 2, 3] }],
  focus: 2,
  panes: [
    [1, { x: 0, y: 0, w: 1 / 3, h: 1 }],
    [2, { x: 1 / 3, y: 0, w: 1 / 3, h: 1 }],
    [3, { x: 2 / 3, y: 0, w: 1 / 3, h: 1 }],
  ],
  dividers: [
    { split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 1 / 3 },
    { split: 11, axis: "Row", area: { x: 1 / 3, y: 0, w: 2 / 3, h: 1 }, ratio: 0.5 },
  ],
  pane_names: [],
  windows: 1,
};

/** The in-page core of the settings dialog: it keeps the view, clamps like the real one and announces the change. */
export function installCore(initial) {
  let view = structuredClone(initial);
  const RANGES = { font_size: [8, 40], ui_scale: [75, 200], scrollback_lines: [100, 100000] };
  window.__MOCK__.on("get_settings", () => structuredClone(view));
  window.__MOCK__.on("reload_settings", () => structuredClone(view));
  window.__MOCK__.on("set_setting", ({ key, value }) => {
    if (key in RANGES) {
      const [lo, hi] = RANGES[key];
      view.settings[key] = Math.min(hi, Math.max(lo, value));
    } else view.settings[key] = value;
    const out = structuredClone(view);
    window.__MOCK__.emit("settings", out);
    return out;
  });
}

/**
 * Open the page as a device would: the platform, the viewport and the size of the screen, and the commands answering like the core.
 * `shown` is the number of panes that must have been started (one on a phone, three on a tablet).
 */
export async function open(page, url, { os = "android", width = 390, height = 844, screen = [390, 844], view = VIEW, shown = 1, settings = false } = {}) {
  await page.setViewportSize({ width, height });
  await page.addInitScript(([w, h]) => {
    for (const [key, value] of [["width", w], ["height", h], ["availWidth", w], ["availHeight", h]]) {
      Object.defineProperty(window.screen, key, { get: () => value, configurable: true });
    }
  }, screen);
  await page.addInitScript(
    ([info, v]) => {
      const mock = window.__MOCK__;
      mock.on("app_info", () => info);
      mock.setView(v);
      mock.on("focus_pane", (a) => {
        const next = mock.view();
        next.focus = a.pane;
        next.tabs.find((t) => t.active).focus = a.pane;
        next.rev += 1;
        next.panes = next.panes.filter(([id]) => id !== a.pane).concat(next.panes.filter(([id]) => id === a.pane));
        mock.setView(next);
        return mock.view();
      });
    },
    [{ version: "0.0.1-alpha1", os, hostname: "pixel" }, view],
  );
  if (settings) await page.addInitScript(installCore, golden.settings_view);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("start_pane").length >= n, shown);
}

/** A frame of `pane` with `cols` by `rows` cells and these selection spans; always a full frame, so the sequence number does not matter. */
export let seq = 10;
export const frame = (pane, cols, rows, selection = [], over = {}) => ({
  pane, seq: ++seq, full: true, cols, rows, fg: 0xf6efe6, bg: BG, cursor: null, scrollbar: [0, rows, rows], selection, matches: [],
  search: null, copy_cursor: null, modes: MODES, changed: [], ...over,
});

/** Where the cells of a pane are on the page (client pixels) and how many there are, from the page's own metrics and rules. */
export const geometry = (page, pane) =>
  page.evaluate(async (id) => {
    const { state } = await import(new URL("state.js", location.href).href);
    const { gridRect } = await import(new URL("geometry.js", location.href).href);
    const canvas = document.getElementById("term").getBoundingClientRect();
    const unit = state.view.panes.find(([p]) => p === id)[1];
    const g = gridRect(state.metrics, unit, state.headerPx);
    const size = state.sizes.get(id);
    return { left: canvas.left + g.x, top: canvas.top + g.y, cw: state.metrics.cellW, ch: state.metrics.cellH, cols: size.cols, rows: size.rows };
  }, pane);

/** The geometry of a pane plus the helper that names the middle of one of its cells. */
export async function shown(page, pane) {
  const g = await geometry(page, pane);
  await emit(page, "frame", frame(pane, g.cols, g.rows));
  return { ...g, pt: (cx, cy) => [g.left + (cx + 0.5) * g.cw, g.top + (cy + 0.5) * g.ch] };
}

// ---- synthetic fingers --------------------------------------------------------------------------------

export const down = (id, [x, y], target) => ({ type: "pointerdown", id, x, y, target });
export const move = (id, [x, y], target, wait = 0) => ({ type: "pointermove", id, x, y, target, wait });
export const up = (id, [x, y], target) => ({ type: "pointerup", id, x, y, target });
export const wait = (ms) => ({ wait: ms });
export const mark = (name) => ({ mark: name });

/** Run the steps inside the page, so their timing is the page's own: each step may wait first, and a mark records the clock and the wheel log. */
export const run = (page, steps) =>
  page.evaluate(async (list) => {
    const out = {};
    for (const s of list) {
      if (s.wait) await new Promise((done) => setTimeout(done, s.wait));
      if (s.type) {
        const target = s.target ? document.querySelector(s.target) : document.getElementById("term");
        target.dispatchEvent(
          new PointerEvent(s.type, {
            pointerId: s.id, pointerType: "touch", isPrimary: s.id === 1, clientX: s.x, clientY: s.y, bubbles: true, cancelable: true,
            button: s.type === "pointermove" ? -1 : 0, buttons: s.type === "pointerup" ? 0 : 1,
          }),
        );
      }
      if (s.mark) out[s.mark] = { t: performance.now(), wheel: (window.__wheel ?? []).length };
    }
    return out;
  }, steps);

/** A drag of one finger from `from` to `to` in `n` equal moves `ms` apart, lifting right after the last one. */
export function swipe(from, to, n = 8, ms = 12, id = 1) {
  const steps = [down(id, from)];
  for (let i = 1; i <= n; i++) {
    steps.push(move(id, [from[0] + ((to[0] - from[0]) * i) / n, from[1] + ((to[1] - from[1]) * i) / n], undefined, ms));
  }
  return steps;
}

export const tap = (page, p) => run(page, [down(1, p), wait(30), up(1, p)]);

export const frames = (page, n = 3) =>
  page.evaluate(async (count) => {
    for (let i = 0; i < count; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  }, n);

export const selects = async (page) => (await callsOf(page, "select")).map((a) => ({ pane: a.pane, ...a.op }));
export const wheels = (page) => page.evaluate(() => window.__wheel);
export const forget = (page) =>
  page.evaluate(() => {
    window.__MOCK__.calls.length = 0;
    window.__wheel.length = 0;
  });
export const logWheel = (page) =>
  page.evaluate(() => {
    window.__wheel = [];
    window.__MOCK__.on("wheel", (a) => void window.__wheel.push({ t: performance.now(), rows: a.rows, pane: a.pane, x: a.x, y: a.y, mods: a.mods }));
  });
export const sum = (list) => list.reduce((n, w) => n + w.rows, 0);

export const box = (page, selector) =>
  page.evaluate((s) => {
    const n = document.querySelector(s);
    if (!n) return null;
    const r = n.getBoundingClientRect();
    return { x: r.x, y: r.y, w: r.width, h: r.height, right: r.right, bottom: r.bottom };
  }, selector);

export const visible = (page, selector) => page.locator(selector).filter({ visible: true }).count();


// ---- shared ---------------------------------------------------------------------------------------------

export const rootPx = (page) => page.evaluate(() => getComputedStyle(document.documentElement).fontSize);
export const fontPx = (page) =>
  page.evaluate(async () => (await import(new URL("frame.js", location.href).href)).getFontPx());
export const stored = (page) => page.evaluate(() => localStorage.getItem("hw.deviceScale"));
