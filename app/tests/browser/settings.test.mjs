// The settings dialog and what a font size or UI scale change does to the grid (DESK-25, GUI half): every control
// sends one `set_setting` with its key and typed value, the dialog shows only what the core answered, font and
// scale changes re-fit every visible pane, and a window focus picks up hand edits. A small in-page core plays
// the answers: it validates and clamps like the real one and returns the whole view.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000); // a dialog that never appears fails here, not at the runner's 30 s limit
  await openCase(page, url);
  await controlsCase(page, url);
  await errorCase(page, url);
  await keyboardCase(page, url);
  await fontActionCase(page, url);
  await fitCase(page, url);
  await scaleCase(page, url);
  await pasteCase(page, url);
  await focusCase(page, url);
  await sourceCase();
  assert.deepEqual(errors, [], "the page reported errors");
}

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));
const AUBERGINE = () => structuredClone(golden.settings_view);

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const HEADER = 20; // CSS px of a pane's header strip at the base root font size of 12 px

/** Papier, enough of it for the dialog's theme change to have a palette to show. */
const PAPIER = {
  version: 1, name: "Papier", kind: "light",
  terminal: { foreground: "#14091A", background: "#FFF3F8", cursor: "#C23B72", selection: "#C23B72", selection_alpha: 0.25, ansi: [] },
  chrome: {
    ground: "#FFF3F8", ground_2: "#FFE6F0", pane: "#FFF9FB", edge: "#E5C6D5", ink: "#14091A", ink_2: "#5B4F5D",
    ink_3: "#6B5C70", rose: "#C23B72", rose_deep: "#C23B72", ok: "#2A7A18", warn: "#8A5A00", danger: "#B3261E",
    info: "#1F5FA8",
  },
};

/**
 * The in-page core. `set_setting` changes its copy of the view, clamps numbers into their ranges like the real
 * core, announces `settings` and answers with the view; `window.__core.fail = { key, message }` makes one key
 * fail with a string as Tauri does. `get_settings` and `reload_settings` answer with the copy.
 */
function installCore([initial, papier]) {
  let view = structuredClone(initial);
  const RANGES = { font_size: [8, 40], ui_scale: [75, 200], scrollback_lines: [100, 100000] };
  window.__core = {
    fail: null,
    view: () => structuredClone(view),
    set(v) {
      view = structuredClone(v);
    },
  };
  window.__MOCK__.on("get_settings", () => structuredClone(view));
  window.__MOCK__.on("reload_settings", () => structuredClone(view));
  window.__MOCK__.on("set_setting", ({ key, value }) => {
    const fail = window.__core.fail;
    if (fail && fail.key === key) throw fail.message;
    if (!(key in view.settings)) throw `there is no setting called "${key}"`;
    if (key in RANGES) {
      if (typeof value !== "number" || !Number.isInteger(value)) throw `invalid value for ${key}: expected a whole number`;
      const [lo, hi] = RANGES[key];
      view.settings[key] = Math.min(hi, Math.max(lo, value));
    } else {
      view.settings[key] = value;
      if (key === "theme") {
        const known = view.themes.some((t) => t.name === value);
        if (!known) throw `there is no theme called "${value}"`;
        if (value === "Papier") view.theme = structuredClone(papier);
      }
    }
    const out = structuredClone(view);
    window.__MOCK__.emit("settings", out);
    return out;
  });
}

/** Open the page against the stub with the in-page core answering. */
async function open(page, url, view = AUBERGINE(), query = "?renderer=canvas") {
  await page.addInitScript(installCore, [view, PAPIER]);
  await page.goto(`${url}index.html${query}`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

const dialog = (page) => page.getByRole("dialog", { name: "Settings" });
const field = (page, label) => dialog(page).getByLabel(label, { exact: true });
/** A choice is a group of radio buttons named by its legend (WebKit reports a CSP violation for a `<select>`). */
const choice = (page, name) => dialog(page).getByRole("group", { name, exact: true });
const options = (page, name) => choice(page, name).locator("label").allTextContents();
const chosen = (page, name) => choice(page, name).locator("input:checked").inputValue();
const pick = (page, name, label) => choice(page, name).getByRole("radio", { name: label, exact: true }).click();
const status = (page) => dialog(page).getByRole("status");
const activeId = (page) => page.evaluate(() => document.activeElement && document.activeElement.id);
const settle = (page) =>
  page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done()))));

/** A value of the state the page keeps, by path (`settings.settings.font_size`), through the page's own module. */
const pageState = (page, path) =>
  page.evaluate(async (p) => {
    const { state } = await import(new URL("state.js", location.href).href);
    return p.split(".").reduce((o, k) => (o === null || o === undefined ? o : o[k]), state);
  }, path);

/** Run an action by id, as the command palette will. */
const runAction = (page, id) =>
  page.evaluate(async (actionId) => {
    const { runAction: run } = await import(new URL("actions.js", location.href).href);
    return run(actionId);
  }, id);

const canvasSize = (page) =>
  page.evaluate(() => {
    const c = document.getElementById("term");
    return { w: c.clientWidth, h: c.clientHeight };
  });

const settingCalls = (page) => callsOf(page, "set_setting");

/** The settings dialog opens with Cmd+, shows every setting with a label, and Escape gives the keyboard back. */
async function openCase(page, url) {
  await open(page, url);
  assert.equal(await dialog(page).count(), 0, "no dialog before it is asked for");
  await page.keyboard.press("Meta+,");
  await dialog(page).waitFor();

  const inside = await page.evaluate(() => !!document.activeElement?.closest('[role="dialog"]'));
  assert.ok(inside, "the focus moves into the dialog");
  assert.equal(await dialog(page).getAttribute("aria-modal"), "true");

  // Every control has a label of its own and shows what the core said.
  assert.equal(await chosen(page, "Theme"), "aubergine");
  assert.deepEqual(await options(page, "Theme"), ["aubergine", "Schiefer", "Papier"], "the theme list is the core's, in its order");
  assert.equal(await field(page, "Font size").inputValue(), "14");
  assert.equal(await field(page, "UI scale").inputValue(), "100");
  assert.equal(await field(page, "Option as Alt").isChecked(), false);
  assert.equal(await chosen(page, "Cursor blink"), "system");
  assert.deepEqual(await options(page, "Cursor blink"), ["System", "On", "Off"]);
  assert.equal(await chosen(page, "Motion"), "system");
  assert.deepEqual(await options(page, "Motion"), ["System", "Reduce", "Full"]);
  assert.equal(await chosen(page, "Contrast"), "system");
  assert.deepEqual(await options(page, "Contrast"), ["System", "Normal", "High"]);
  assert.equal(await dialog(page).locator("select").count(), 0, "no <select>: WebKit reports an inline style for it");
  assert.equal(await field(page, "Confirm multi-line paste").isChecked(), true);
  assert.equal(await field(page, "Scrollback lines").inputValue(), "10000");
  assert.equal(await dialog(page).getByRole("button", { name: "Reload from file" }).count(), 1);
  assert.ok((await dialog(page).locator("fieldset").count()) >= 3, "the controls are grouped in fieldsets");

  // The scrollback control says what the engine keeps, not what the number would promise.
  const hint = await dialog(page).locator("[data-hint=scrollback]").textContent();
  assert.match(hint, /fewer/i, `the scrollback hint must name the engine's limit: ${hint}`);

  // The core's warnings are shown as text.
  assert.match(await dialog(page).textContent(), /font_size 9999 is outside 8\.\.=40/);

  await page.keyboard.press("Escape");
  await settle(page);
  assert.equal(await dialog(page).count(), 0, "Escape closes the dialog");
  assert.equal(await activeId(page), "ime", "the keyboard is back in the terminal input");

  // The action opens it too, and a second ask does not stack a second dialog.
  await runAction(page, "settings.open");
  await runAction(page, "settings.open");
  await dialog(page).waitFor();
  assert.equal(await page.getByRole("dialog").count(), 1);
}

/** Every control sends exactly one set_setting with its key and the typed value; the view answers show. */
async function controlsCase(page, url) {
  await open(page, url);
  await page.keyboard.press("Meta+,");
  await dialog(page).waitFor();

  const last = async (n) => {
    await page.waitForFunction((count) => window.__MOCK__.callsOf("set_setting").length >= count, n);
    const calls = await settingCalls(page);
    assert.equal(calls.length, n, `exactly ${n} set_setting calls so far, got ${JSON.stringify(calls)}`);
    return calls[n - 1];
  };

  await pick(page, "Theme", "Papier");
  assert.deepEqual(await last(1), { key: "theme", value: "Papier" });
  await page.waitForFunction(() => getComputedStyle(document.documentElement).getPropertyValue("--hw-ground").trim() === "#fff3f8");

  await field(page, "Font size").fill("18");
  await field(page, "Font size").press("Enter");
  assert.deepEqual(await last(2), { key: "font_size", value: 18 });
  assert.equal(typeof (await settingCalls(page))[1].value, "number", "a number goes out as a number");

  await field(page, "UI scale").fill("150");
  await field(page, "UI scale").press("Enter");
  assert.deepEqual(await last(3), { key: "ui_scale", value: 150 });

  await field(page, "Option as Alt").check();
  assert.deepEqual(await last(4), { key: "option_as_alt", value: true });

  await pick(page, "Cursor blink", "Off");
  assert.deepEqual(await last(5), { key: "cursor_blink", value: "off" });

  await pick(page, "Motion", "Reduce");
  assert.deepEqual(await last(6), { key: "motion", value: "reduce" });

  await pick(page, "Contrast", "High");
  assert.deepEqual(await last(7), { key: "contrast", value: "high" });

  await field(page, "Confirm multi-line paste").uncheck();
  assert.deepEqual(await last(8), { key: "confirm_multiline_paste", value: false });

  await field(page, "Scrollback lines").fill("5000");
  await field(page, "Scrollback lines").press("Enter");
  assert.deepEqual(await last(9), { key: "scrollback_lines", value: 5000 });

  // Leaving a field after Enter does not send it a second time.
  await field(page, "Scrollback lines").blur();
  await settle(page);
  assert.equal((await settingCalls(page)).length, 9, "blur after Enter must not send again");

  // The dialog shows what the core answered.
  assert.equal(await field(page, "Font size").inputValue(), "18");
  assert.equal(await field(page, "UI scale").inputValue(), "150");
  assert.equal(await chosen(page, "Theme"), "Papier");
  assert.equal(await field(page, "Option as Alt").isChecked(), true);
  assert.equal(await field(page, "Confirm multi-line paste").isChecked(), false);
  assert.equal(await pageState(page, "settings.settings.font_size"), 18);

  // A number beyond the range is clamped by the core, and the control shows the clamped value.
  await field(page, "Font size").fill("99");
  await field(page, "Font size").press("Enter");
  assert.deepEqual(await last(10), { key: "font_size", value: 99 });
  await page.waitForFunction(() => document.getElementById("set-font-size")?.value === "40");
  assert.equal(await field(page, "Font size").inputValue(), "40", "the control shows the core's value, not the typed one");

  // A field with no number in it (a number input holds nothing else) sends nothing and returns to the last good value.
  await field(page, "Font size").fill("");
  await field(page, "Font size").press("Enter");
  await settle(page);
  assert.equal((await settingCalls(page)).length, 10, "an empty number field must not be sent");
  assert.equal(await field(page, "Font size").inputValue(), "40");
}

/** A refused value shows the core's reason in the dialog and the control goes back to the last good value. */
async function errorCase(page, url) {
  await open(page, url);
  await page.keyboard.press("Meta+,");
  await dialog(page).waitFor();
  await page.evaluate(() => {
    window.__core.fail = { key: "font_size", message: "invalid value for font_size: expected a whole number" };
  });

  await field(page, "Font size").fill("18");
  await field(page, "Font size").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 1);
  await status(page).filter({ hasText: "invalid value for font_size" }).waitFor();
  assert.equal(await field(page, "Font size").inputValue(), "14", "a rejected value must not stay in the control");
  assert.equal(await pageState(page, "settings.settings.font_size"), 14, "the page's state is the core's");

  // A choice returns to the last good value as well.
  await page.evaluate(() => {
    window.__core.fail = { key: "theme", message: 'there is no theme called "Papier"' };
  });
  await pick(page, "Theme", "Papier");
  await status(page).filter({ hasText: "there is no theme called" }).waitFor();
  assert.equal(await chosen(page, "Theme"), "aubergine", "a refused choice goes back to the last good one");

  // The reason is text: markup in it is shown as it is.
  await page.evaluate(() => {
    window.__core.fail = { key: "contrast", message: "<img src=x onerror=alert(1)>" };
  });
  await pick(page, "Contrast", "High");
  await status(page).filter({ hasText: "<img src=x" }).waitFor();
  assert.equal(await dialog(page).locator("img").count(), 0, "an error message is never markup");
}

/** The dialog is keyboard-operable and keeps its keys to itself. */
async function keyboardCase(page, url) {
  await open(page, url);
  await page.keyboard.press("Meta+,");
  await dialog(page).waitFor();
  const keysBefore = (await callsOf(page, "key")).length;

  // Tab walks the controls and wraps inside the dialog; it never reaches the page behind it.
  const seen = new Set();
  for (let i = 0; i < 30; i++) {
    await page.keyboard.press("Tab");
    const where = await page.evaluate(() => {
      const a = document.activeElement;
      return a && a.closest('[role="dialog"]') ? a.id || a.textContent : null;
    });
    assert.notEqual(where, null, `Tab ${i + 1} left the dialog`);
    seen.add(where);
  }
  assert.ok(seen.size >= 10, `Tab visited only ${seen.size} controls: ${[...seen].join(", ")}`);
  assert.ok(![...seen].some((id) => /^set-theme-[12]$/.test(id)), "a radio group is one tab stop");

  // The arrow keys move inside a group and choose, one request each.
  await choice(page, "Motion").getByRole("radio", { name: "System", exact: true }).focus();
  await page.keyboard.press("ArrowRight");
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 1);
  assert.deepEqual((await settingCalls(page)).at(-1), { key: "motion", value: "reduce" });

  // Shift+Tab wraps backwards.
  await page.keyboard.press("Shift+Tab");
  assert.ok(await page.evaluate(() => !!document.activeElement?.closest('[role="dialog"]')));

  // A checkbox works from the keyboard: Space toggles and sends once.
  await field(page, "Option as Alt").focus();
  await page.keyboard.press("Space");
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 2);
  assert.deepEqual((await settingCalls(page)).at(-1), { key: "option_as_alt", value: true });

  // Typed characters belong to the field: nothing reaches the terminal.
  await field(page, "Scrollback lines").focus();
  await page.keyboard.type("x7");
  await page.keyboard.press("Meta+d");
  await settle(page);
  assert.equal((await callsOf(page, "key")).length, keysBefore, "keys must not reach the core while the dialog is open");
  assert.equal((await callsOf(page, "split")).length, 0, "a shortcut must not run behind the dialog");

  // A click outside closes it and the keyboard is back.
  await page.mouse.click(2, 2);
  await settle(page);
  assert.equal(await dialog(page).count(), 0, "a click on the backdrop closes the dialog");
  assert.equal(await activeId(page), "ime");
  await page.keyboard.press("x");
  assert.ok((await callsOf(page, "key")).length > keysBefore, "keys reach the terminal again");
}

/** Cmd+=, Cmd+- and Cmd+0 ask for size plus one, minus one and the default; fast repeats do not lose steps. */
async function fontActionCase(page, url) {
  await open(page, url);
  const sent = async (n) => {
    await page.waitForFunction((count) => window.__MOCK__.callsOf("set_setting").length >= count, n);
    return (await settingCalls(page)).map((c) => [c.key, c.value]);
  };
  const shown = (size) =>
    page.waitForFunction(async (v) => {
      const { state } = await import(new URL("state.js", location.href).href);
      return state.settings.settings.font_size === v;
    }, size);

  await page.keyboard.press("Meta+=");
  assert.deepEqual(await sent(1), [["font_size", 15]]);
  await shown(15);
  await page.keyboard.press("Meta+-");
  await page.keyboard.press("Meta+-");
  assert.deepEqual((await sent(3)).slice(1), [["font_size", 14], ["font_size", 13]], "a second step builds on the first");
  await shown(13);
  await page.keyboard.press("Meta+0");
  assert.deepEqual((await sent(4)).at(-1), ["font_size", 14]);
  await shown(14);

  // Three increases in the same task: 15, 16, 17 (each one reads the answer of the one before).
  await page.evaluate(async () => {
    const { runAction: run } = await import(new URL("actions.js", location.href).href);
    run("font.increase");
    run("font.increase");
    run("font.increase");
  });
  assert.deepEqual((await sent(7)).slice(4), [["font_size", 15], ["font_size", 16], ["font_size", 17]]);
}

/** A new font size is a new cell size: the renderer is configured and every visible pane gets `resize`. */
async function fitCase(page, url) {
  await open(page, url);
  const first = (await callsOf(page, "start_pane")).find((a) => a.pane === 1);
  assert.equal(first.cell_h, 18, "ceil(14 px * 1.25) at the default size");
  const resizes = (await callsOf(page, "resize")).length;

  const view = AUBERGINE();
  view.settings.font_size = 20;
  await emit(page, "settings", view);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("resize").length >= n + 2, resizes);
  const calls = (await callsOf(page, "resize")).slice(resizes);
  assert.deepEqual(calls.map((c) => c.pane).sort(), [1, 2], "resize goes to every visible pane, once");

  const size = await canvasSize(page);
  for (const c of calls) {
    assert.equal(c.cell_h, 25, "ceil(20 px * 1.25)");
    assert.ok(c.cell_w > first.cell_w, `the cell is wider: ${c.cell_w} vs ${first.cell_w}`);
    assert.equal(c.rows, Math.floor((size.h - HEADER - PAD) / 25), `pane ${c.pane}: rows from the new cell height`);
    const gridW = size.w / 2 - 2 * PAD;
    assert.equal(c.cols, Math.floor(gridW / c.cell_w), `pane ${c.pane}: columns from the new cell width`);
    assert.ok(c.cols < first.cols, `pane ${c.pane}: fewer columns at a larger font (${c.cols} vs ${first.cols})`);
  }
  assert.equal((await callsOf(page, "start_pane")).length, 2, "no pane is started again");

  // The same view again changes nothing.
  await emit(page, "settings", view);
  await settle(page);
  assert.equal((await callsOf(page, "resize")).length, resizes + 2, "an unchanged view sends no resize");

  // A font size the page does not accept leaves the size alone.
  const bad = AUBERGINE();
  bad.settings.font_size = 900;
  await emit(page, "settings", bad);
  await settle(page);
  assert.equal((await callsOf(page, "resize")).length, resizes + 2, "an out-of-range size must not change the cells");
}

/** UI scale 150 % is an 18 px root font size and a 30 px header; the grid below it is fitted again. */
async function scaleCase(page, url) {
  await open(page, url);
  const resizes = (await callsOf(page, "resize")).length;
  assert.equal(await pageState(page, "headerPx"), HEADER);

  const view = AUBERGINE();
  view.settings.ui_scale = 150;
  await emit(page, "settings", view);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("resize").length >= n + 2, resizes);

  const css = await page.evaluate(() => ({
    root: getComputedStyle(document.documentElement).fontSize,
    header: document.documentElement.style.getPropertyValue("--hw-header"),
  }));
  assert.equal(css.root, "18px", "12 px base times 150 %");
  assert.equal(css.header, "30px");
  assert.equal(await pageState(page, "headerPx"), 30);

  const size = await canvasSize(page);
  const calls = (await callsOf(page, "resize")).slice(resizes);
  for (const c of calls) {
    assert.equal(c.rows, Math.floor((size.h - 30 - PAD) / 18), `pane ${c.pane}: rows below a 30 px header`);
  }
  // The header strips are as high as the state says.
  const heads = await page.evaluate(() => [...document.querySelectorAll("#overlay .panehead")].map((n) => n.getBoundingClientRect().height));
  assert.deepEqual(heads, [30, 30]);

  // Back to 100 %: the stylesheet's own size is back and the header too.
  const normal = AUBERGINE();
  await emit(page, "settings", normal);
  await page.waitForFunction(() => getComputedStyle(document.documentElement).fontSize === "12px");
  assert.equal(await pageState(page, "headerPx"), HEADER);
}

/** "Confirm multi-line paste" drives the paste decision: off sends a rejected paste again without asking. */
async function pasteCase(page, url) {
  await open(page, url);
  await page.evaluate(() => window.__MOCK__.on("paste", (a) => (a.allow_unsafe ? "written" : "rejected")));
  const paste = (text) =>
    page.evaluate((t) => {
      const data = new DataTransfer();
      data.setData("text/plain", t);
      document.getElementById("ime").dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
    }, text);

  const off = AUBERGINE();
  off.settings.confirm_multiline_paste = false;
  await emit(page, "settings", off);
  await paste("one\ntwo");
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length >= 2);
  assert.deepEqual((await callsOf(page, "paste")).map((a) => a.allow_unsafe), [false, true]);
  await settle(page);
  assert.equal(await page.getByRole("dialog").count(), 0, "with the preference off nothing is asked");

  await emit(page, "settings", AUBERGINE()); // confirm_multiline_paste is true again
  await paste("three\nfour");
  await page.getByRole("dialog").waitFor();
  assert.equal((await callsOf(page, "paste")).length, 3, "with the preference on the second send waits for the answer");
  await page.keyboard.press("Escape");
}

/** A window focus asks the core to look at the files, at most once a second; the answer is applied. */
async function focusCase(page, url) {
  await open(page, url);
  const base = (await callsOf(page, "reload_settings")).length;
  const focus = () => page.evaluate(() => window.dispatchEvent(new Event("focus")));

  await focus();
  await focus();
  await focus();
  await settle(page);
  assert.equal((await callsOf(page, "reload_settings")).length, base + 1, "three focus events in a row make one reload");

  // A hand edit: the core's view has another font size when it is asked again.
  const edited = AUBERGINE();
  edited.settings.font_size = 16;
  await page.evaluate((v) => window.__core.set(v), edited);
  await page.waitForTimeout(1100);
  const resizes = (await callsOf(page, "resize")).length;
  await focus();
  await page.waitForFunction((n) => window.__MOCK__.callsOf("reload_settings").length >= n + 2, base);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("resize").length >= n + 2, resizes);
  assert.equal(await pageState(page, "settings.settings.font_size"), 16, "the hand edit is picked up");

  // The button asks at once, whatever the focus did a moment ago.
  await page.keyboard.press("Meta+,");
  await dialog(page).waitFor();
  const before = (await callsOf(page, "reload_settings")).length;
  await dialog(page).getByRole("button", { name: "Reload from file" }).click();
  await page.waitForFunction((n) => window.__MOCK__.callsOf("reload_settings").length >= n + 1, before);
}

/** No source file writes a style or markup from a string (T-02-71). */
async function sourceCase() {
  const dist = join(APP_DIR, "dist");
  for (const name of ["settings.js", "settings-ui.js", "settings-model.js"]) {
    const text = readFileSync(join(dist, name), "utf8");
    assert.doesNotMatch(text, /cssText|innerHTML|outerHTML|insertAdjacentHTML|setAttribute\(\s*["']style/, `${name}`);
  }
  const css = readFileSync(join(dist, "settings.css"), "utf8");
  const colours = css.match(/#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla)\(/g) ?? [];
  assert.deepEqual(colours, [], `settings.css hard-codes colours: ${colours.join(", ")}`);
}
