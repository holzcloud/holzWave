// The key binding editor and the core's table as the dispatch source (DESK-25). Shortcuts resolve through the table
// the core sent once settings arrived and through the static table before; the editor lists every action with its
// chord, records a chord with the same spelling the core stores, sends the whole overrides map through
// `set_setting`, shows the core's refusals beside the row and shows only what the core answered. A small in-page
// core resolves overrides by the rules of core/src/keybindings.rs (subset: what these cases need).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await dispatchCase(page, url);
  await listCase(page, url);
  await changeCase(page, url);
  await captureCase(page, url);
  await refusalCase(page, url);
  await conflictCase(page, url);
  await settingsButtonCase(page, url);
  await sourceCase();
  assert.deepEqual(errors, [], "the page reported errors");
}

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));
const AUBERGINE = () => structuredClone(golden.settings_view);

/**
 * The in-page core. `set_setting` for "keybindings" resolves the typed map into rows and warnings like the real
 * resolver (override beats default and unbinds the displaced action with a warning, a plain key is refused and
 * the default stays, the empty string unbinds, of two overrides on one chord the alphabetically earlier wins),
 * announces `settings` and answers with the whole view. `window.__core.down` makes `get_settings` fail.
 */
function installCore([initial, down]) {
  const defaults = initial.keybindings.map((r) => ({ ...r }));
  let view = structuredClone(initial);
  const MODS = { cmd: "Cmd", command: "Cmd", meta: "Cmd", super: "Cmd", ctrl: "Ctrl", control: "Ctrl", alt: "Alt", option: "Alt", shift: "Shift" };
  const parse = (text) => {
    const parts = text.split("+").map((p) => p.trim());
    if (parts.some((p) => p === "")) return { error: "has an empty part between its plus signs" };
    const key = parts.pop();
    const held = new Set();
    for (const m of parts) {
      const name = MODS[m.toLowerCase()];
      if (!name) return { error: `${m} is not a modifier (Cmd, Ctrl, Alt or Shift)` };
      held.add(name);
    }
    const mods = ["Cmd", "Ctrl", "Alt", "Shift"].filter((m) => held.has(m));
    const named = ["Enter", "Space", "Tab", "Escape", "Up", "Down", "Left", "Right"].find((n) => n.toLowerCase() === key.toLowerCase());
    const name = named ?? (key.length === 1 ? key.toUpperCase() : null);
    if (name === null) return { error: "is not a key" };
    const bindable = held.has("Cmd") || (mods.length >= 2 && (held.has("Ctrl") || held.has("Alt")));
    return { chord: [...mods, name].join("+"), bindable };
  };
  function resolve(overrides) {
    const rows = defaults.map((r) => ({ ...r }));
    const warnings = [];
    const claimed = new Map();
    const overridden = new Set();
    for (const [action, text] of Object.entries(overrides).sort(([a], [b]) => (a < b ? -1 : 1))) {
      const row = rows.find((r) => r.action === action);
      if (!row) {
        warnings.push(`Unknown action "${action}"; the binding is ignored.`);
        continue;
      }
      if (text === "") {
        row.chord = null;
        overridden.add(action);
        continue;
      }
      const parsed = parse(text);
      if (parsed.error) {
        warnings.push(`The key combination "${text}" for ${action} cannot be used (${parsed.error}); the default stays.`);
        continue;
      }
      if (!parsed.bindable) {
        warnings.push(
          `The key combination ${parsed.chord} for ${action} is not allowed because it would take plain typing away ` +
            "from the terminal; use Cmd, or two modifiers including Ctrl or Alt; the default stays.",
        );
        continue;
      }
      if (claimed.has(parsed.chord)) {
        warnings.push(`${action} cannot use ${parsed.chord} because ${claimed.get(parsed.chord)} already uses it; the binding is ignored.`);
        continue;
      }
      claimed.set(parsed.chord, action);
      row.chord = parsed.chord;
      overridden.add(action);
    }
    for (const row of rows) {
      if (overridden.has(row.action) || row.chord === null || !claimed.has(row.chord)) continue;
      warnings.push(`${row.action} lost its default ${row.chord} to ${claimed.get(row.chord)} and is unbound.`);
      row.chord = null;
    }
    for (const row of rows) row.is_default = row.chord === defaults.find((d) => d.action === row.action).chord;
    return { rows, warnings };
  }
  window.__core = {
    view: () => structuredClone(view),
    down,
  };
  window.__MOCK__.on("get_settings", () => {
    if (window.__core.down) throw new Error("the core is not ready");
    return structuredClone(view);
  });
  window.__MOCK__.on("reload_settings", () => structuredClone(view));
  window.__MOCK__.on("set_setting", ({ key, value }) => {
    if (key !== "keybindings") {
      if (!(key in view.settings)) throw `there is no setting called "${key}"`;
      view.settings[key] = value;
    } else {
      if (typeof value !== "object" || value === null || Array.isArray(value)) throw "invalid value for keybindings: expected a map";
      view.settings.keybindings = structuredClone(value);
      const { rows, warnings } = resolve(value);
      view.keybindings = rows;
      view.warnings = warnings;
    }
    const out = structuredClone(view);
    window.__MOCK__.emit("settings", out);
    return out;
  });
}

/** Open the page with the in-page core answering (or, with `down`, not answering at all). */
async function open(page, url, { down = false } = {}) {
  await page.addInitScript(installCore, [AUBERGINE(), down]);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

const dialog = (page) => page.getByRole("dialog", { name: "Key bindings" });
const rowOf = (page, action) => dialog(page).locator(`[data-action="${action}"]`);
const chordOf = (page, action) => rowOf(page, action).locator(".kb-chord").textContent();
const warningsOf = (page, action) => rowOf(page, action).locator(".kb-warn li").allTextContents();
const settle = (page) =>
  page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done()))));
const activeId = (page) => page.evaluate(() => document.activeElement && document.activeElement.id);
const runAction = (page, id) =>
  page.evaluate(async (actionId) => {
    const { runAction: run } = await import(new URL("actions.js", location.href).href);
    return run(actionId);
  }, id);
const bindingCalls = async (page) => (await callsOf(page, "set_setting")).filter((c) => c.key === "keybindings");

/** Shortcuts use the static table before settings arrive and the core's table after each view. */
async function dispatchCase(page, url) {
  await open(page, url, { down: true });
  const splits = async () => (await callsOf(page, "split")).length;

  await page.keyboard.press("Meta+d");
  await page.waitForFunction(() => window.__MOCK__.callsOf("split").length >= 1);
  assert.deepEqual((await callsOf(page, "split"))[0], { column: false }, "static table: Cmd+D splits right");

  // The core's table binds Split right to Cmd+Shift+Enter and leaves Cmd+D with nobody.
  const view = AUBERGINE();
  view.keybindings.find((r) => r.action === "pane.split_right").chord = "Cmd+Shift+Enter";
  view.keybindings.find((r) => r.action === "pane.split_right").is_default = false;
  await emit(page, "settings", view);
  await settle(page);

  const keys = (await callsOf(page, "key")).length;
  await page.keyboard.press("Meta+d");
  await settle(page);
  assert.equal(await splits(), 1, "the old binding no longer works");
  assert.equal((await callsOf(page, "key")).length, keys, "and the chord is not terminal input either");
  await page.keyboard.press("Meta+Shift+Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("split").length >= 2);
  assert.deepEqual((await callsOf(page, "split"))[1], { column: false });

  // The other rows keep working as the core said, and a fixed shortcut (Cmd+C is no rebindable action) stays.
  await page.keyboard.press("Meta+t");
  await page.waitForFunction(() => window.__MOCK__.callsOf("new_tab").length >= 1);
  assert.equal(await page.evaluate(async () => {
    const { listActions } = await import(new URL("actions.js", location.href).href);
    return listActions().some((a) => a.id === "edit.copy");
  }), true, "edit.copy is registered, so Cmd+C reaches it through the fixed chord");
}

/** `keybindings.open` lists every action with its title and chord, and gives the keyboard back. */
async function listCase(page, url) {
  await open(page, url);
  assert.equal(await dialog(page).count(), 0, "not open before it is asked for");
  assert.equal(await runAction(page, "keybindings.open"), true, "the action exists");
  await dialog(page).waitFor();
  assert.equal(await dialog(page).getAttribute("aria-modal"), "true");

  const rows = dialog(page).locator(".kb-row");
  assert.equal(await rows.count(), 47, "one row per action of the core");
  assert.equal(await rowOf(page, "pane.split_right").locator(".kb-title").textContent(), "Split pane right");
  assert.equal(await chordOf(page, "pane.split_right"), "Cmd+D");
  assert.equal(await chordOf(page, "workspace.select_3"), "Cmd+Alt+3");
  assert.equal(await chordOf(page, "settings.open"), "Cmd+,");

  // Every row has its own three buttons, named by the action.
  const split = rowOf(page, "pane.split_right");
  assert.equal(await split.getByRole("button", { name: "Change Split pane right" }).count(), 1);
  assert.equal(await split.getByRole("button", { name: "Unbind Split pane right" }).count(), 1);
  assert.equal(await split.getByRole("button", { name: "Reset Split pane right" }).isDisabled(), true, "no override to reset");

  // Keys stay in the dialog: a shortcut does not run behind it and nothing reaches the terminal.
  const keys = (await callsOf(page, "key")).length;
  await page.keyboard.press("Meta+d");
  await page.keyboard.type("xyz");
  await settle(page);
  assert.equal((await callsOf(page, "split")).length, 0);
  assert.equal((await callsOf(page, "key")).length, keys);

  await page.keyboard.press("Escape");
  await settle(page);
  assert.equal(await dialog(page).count(), 0, "Escape closes it");
  assert.equal(await activeId(page), "ime", "the keyboard is back in the terminal input");
}

/** Change, Unbind and Reset send the whole overrides map, the row follows the core's answer and dispatch follows the row. */
async function changeCase(page, url) {
  await open(page, url);
  await runAction(page, "keybindings.open");
  await dialog(page).waitFor();

  await rowOf(page, "pane.split_right").getByRole("button", { name: "Change Split pane right" }).click();
  await page.keyboard.press("Meta+Alt+y");
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 1);
  assert.deepEqual((await bindingCalls(page))[0], { key: "keybindings", value: { "pane.split_right": "Cmd+Alt+Y" } });
  await page.waitForFunction(() => document.querySelector('[data-action="pane.split_right"] .kb-chord')?.textContent === "Cmd+Alt+Y");
  assert.equal((await callsOf(page, "key")).length, 0, "the recorded press did not reach the terminal");
  assert.equal((await callsOf(page, "split")).length, 0, "nor did it run an action");
  assert.equal(await rowOf(page, "pane.split_right").getAttribute("data-state"), "custom");

  // A second override builds on the first: the whole map goes out.
  await rowOf(page, "pane.close").getByRole("button", { name: "Unbind Close pane" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 2);
  assert.deepEqual((await bindingCalls(page))[1].value, { "pane.split_right": "Cmd+Alt+Y", "pane.close": "" });
  await page.waitForFunction(() => document.querySelector('[data-action="pane.close"] .kb-chord')?.textContent === "Unbound");
  assert.equal(await rowOf(page, "pane.close").getAttribute("data-state"), "unbound");

  // Close the dialog: the new binding works, the old one and the unbound one do not.
  await page.keyboard.press("Escape");
  await settle(page);
  await page.keyboard.press("Meta+Alt+y");
  await page.waitForFunction(() => window.__MOCK__.callsOf("split").length >= 1);
  await page.keyboard.press("Meta+d");
  await page.keyboard.press("Meta+w");
  await settle(page);
  assert.equal((await callsOf(page, "split")).length, 1, "Cmd+D no longer splits");
  assert.equal((await callsOf(page, "close_pane")).length, 0, "an unbound action has no key");

  // Reset removes the override from the map and the default is back.
  await runAction(page, "keybindings.open");
  await dialog(page).waitFor();
  await rowOf(page, "pane.split_right").getByRole("button", { name: "Reset Split pane right" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 3);
  assert.deepEqual((await bindingCalls(page))[2].value, { "pane.close": "" }, "the override of that action is gone");
  await page.waitForFunction(() => document.querySelector('[data-action="pane.split_right"] .kb-chord')?.textContent === "Cmd+D");
  assert.equal(await rowOf(page, "pane.split_right").getAttribute("data-state"), "default");
  await rowOf(page, "pane.close").getByRole("button", { name: "Reset Close pane" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 4);
  assert.deepEqual((await bindingCalls(page))[3].value, {});
  await page.waitForFunction(() => document.querySelector('[data-action="pane.close"] .kb-chord')?.textContent === "Cmd+W");

  // Unbind sends an empty string for that action.
  await rowOf(page, "tab.new").getByRole("button", { name: "Unbind New tab" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 5);
  assert.deepEqual((await bindingCalls(page))[4].value, { "tab.new": "" });
}

/** While a chord is captured every key is the editor's: modifiers alone are ignored, Escape cancels. */
async function captureCase(page, url) {
  await open(page, url);
  await runAction(page, "keybindings.open");
  await dialog(page).waitFor();
  const change = rowOf(page, "pane.split_down").getByRole("button", { name: "Change Split pane down" });

  await change.click();
  assert.match(await chordOf(page, "pane.split_down"), /press/i, "the row asks for the key combination");
  const keys = (await callsOf(page, "key")).length;

  // A modifier on its own is no chord: still capturing, nothing sent.
  await page.keyboard.down("Meta");
  await page.keyboard.down("Shift");
  await page.keyboard.up("Shift");
  await page.keyboard.up("Meta");
  await settle(page);
  assert.equal((await bindingCalls(page)).length, 0, "a modifier alone sends nothing");
  assert.match(await chordOf(page, "pane.split_down"), /press/i, "still capturing");

  // Tab, Enter and Space are chords' keys here, not the dialog's: nothing activates behind the capture.
  await page.keyboard.press("Space");
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 1);
  assert.deepEqual((await bindingCalls(page))[0].value, { "pane.split_down": "Space" }, "a key is recorded as the core spells it");

  // Escape cancels a capture and leaves the dialog open; a second Escape closes it.
  await page.waitForFunction(() => !/press/i.test(document.querySelector('[data-action="pane.split_down"] .kb-chord')?.textContent ?? ""));
  await change.click();
  await page.keyboard.press("Escape");
  await settle(page);
  assert.equal(await dialog(page).count(), 1, "Escape ended the capture only");
  assert.doesNotMatch(await chordOf(page, "pane.split_down"), /press/i);
  assert.equal((await bindingCalls(page)).length, 1, "a cancelled capture sends nothing");
  assert.equal((await callsOf(page, "key")).length, keys, "no key reached the terminal while capturing");
  assert.equal((await callsOf(page, "split")).length, 0, "no shortcut ran while capturing");
  await page.keyboard.press("Escape");
  await settle(page);
  assert.equal(await dialog(page).count(), 0);
}

/** The core refuses a chord: the row stays as it was and the core's sentence is shown beside it. */
async function refusalCase(page, url) {
  await open(page, url);
  await runAction(page, "keybindings.open");
  await dialog(page).waitFor();

  await rowOf(page, "pane.split_right").getByRole("button", { name: "Change Split pane right" }).click();
  await page.keyboard.press("x");
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 1);
  assert.deepEqual((await bindingCalls(page))[0].value, { "pane.split_right": "X" });
  await rowOf(page, "pane.split_right").locator(".kb-warn li").first().waitFor();

  assert.equal(await chordOf(page, "pane.split_right"), "Cmd+D", "the row is unchanged");
  const warnings = await warningsOf(page, "pane.split_right");
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /not allowed because it would take plain typing/);
  assert.deepEqual(await warningsOf(page, "pane.split_down"), [], "another row shows nothing");

  // The sentence is text, never markup.
  await page.evaluate(() => {
    const v = window.__core.view();
    v.warnings = ["<img src=x onerror=alert(1)> for pane.close is refused"];
    window.__MOCK__.emit("settings", v);
  });
  await rowOf(page, "pane.close").locator(".kb-warn li").first().waitFor();
  assert.equal(await dialog(page).locator("img").count(), 0);

  // Reset of the refused entry removes it from the map; the sentence goes with it.
  await page.evaluate(() => {
    const v = window.__core.view();
    window.__MOCK__.emit("settings", v);
  });
  await rowOf(page, "pane.split_right").getByRole("button", { name: "Reset Split pane right" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 2);
  assert.deepEqual((await bindingCalls(page))[1].value, {}, "the refused entry is removed");
  await page.waitForFunction(() => document.querySelectorAll('[data-action="pane.split_right"] .kb-warn li').length === 0);
}

/** An override beats a default: the displaced action is unbound and the core's warning shows beside both rows. */
async function conflictCase(page, url) {
  await open(page, url);
  await runAction(page, "keybindings.open");
  await dialog(page).waitFor();

  await rowOf(page, "pane.split_down").getByRole("button", { name: "Change Split pane down" }).click();
  await page.keyboard.press("Meta+d");
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 1);
  assert.deepEqual((await bindingCalls(page))[0].value, { "pane.split_down": "Cmd+D" });
  await page.waitForFunction(() => document.querySelector('[data-action="pane.split_right"] .kb-chord')?.textContent === "Unbound");
  assert.equal(await chordOf(page, "pane.split_down"), "Cmd+D");

  for (const action of ["pane.split_right", "pane.split_down"]) {
    const w = await warningsOf(page, action);
    assert.equal(w.length, 1, `${action} shows the sentence`);
    assert.match(w[0], /pane\.split_right lost its default Cmd\+D to pane\.split_down and is unbound/);
  }
  assert.deepEqual(await warningsOf(page, "tab.new"), []);
}

/** The settings dialog has a way to the editor, and the keyboard goes to the editor and back to the terminal. */
async function settingsButtonCase(page, url) {
  await open(page, url);
  await page.keyboard.press("Meta+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings.waitFor();
  await settings.getByRole("button", { name: "Key bindings" }).click();
  await dialog(page).waitFor();
  await settle(page);
  assert.equal(await settings.count(), 0, "the settings dialog closed");
  assert.equal(await page.getByRole("dialog").count(), 1, "one dialog at a time");
  assert.ok(await page.evaluate(() => !!document.activeElement?.closest('[role="dialog"]')), "the keyboard is in the editor");

  await dialog(page).getByRole("button", { name: "Close", exact: true }).click();
  await settle(page);
  assert.equal(await dialog(page).count(), 0);
  assert.equal(await activeId(page), "ime");
}

/** No source writes a style or markup from a string. */
async function sourceCase() {
  const text = readFileSync(join(APP_DIR, "dist", "keybindings-ui.js"), "utf8");
  assert.doesNotMatch(text, /cssText|innerHTML|outerHTML|insertAdjacentHTML|setAttribute\(\s*["']style/);
  const css = readFileSync(join(APP_DIR, "dist", "settings.css"), "utf8");
  const colours = css.match(/#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla)\(/g) ?? [];
  assert.deepEqual(colours, [], `settings.css hard-codes colours: ${colours.join(", ")}`);
}
