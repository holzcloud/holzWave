// Paste in the real page against the Tauri stub: the core decides, the page asks. A rejected paste shows a modal
// confirmation with the line count and a bounded preview; nothing risky is sent without it. It also covers the
// clipboard and the chord routes of Windows and Linux (Phase 2.2): a paste chord keeps its default and the page reads
// the clipboard itself only when no `paste` event comes, copy and paste leave a text field alone, plain Ctrl+C is
// the program's, and a binding row of the core runs its action for that platform.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";

/** Open the app against the stub and wait until the layout is on screen and every pane was started. */
async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

/** Paste `text` the way the system does: a paste event on the focused terminal input. Returns defaultPrevented. */
const paste = (page, text) =>
  page.evaluate((t) => {
    const data = new DataTransfer();
    if (t !== null) data.setData("text/plain", t);
    const e = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
    document.getElementById("ime").dispatchEvent(e);
    return e.defaultPrevented;
  }, text);

/** The stub answers the first paste with `rejected` and a paste that allows unsafe text with `written`. */
const rejectUnsafe = (page) =>
  page.evaluate(() => window.__MOCK__.on("paste", (a) => (a.allow_unsafe ? "written" : "rejected")));

const dialog = (page) => page.locator('[role="dialog"]');
const pastes = (page) => callsOf(page, "paste");
const active = (page) => page.evaluate(() => document.activeElement && document.activeElement.id);
/** The keyboard is back in the terminal input (a click may move it away first; the page takes it back). */
const focusBack = (page) =>
  page.waitForFunction(() => document.activeElement && document.activeElement.id === "ime", undefined, { timeout: 2000 });
const settle = (page) =>
  page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done()))));

const SEVEN = [
  "first", "second", "x".repeat(120), "fourth", "\u001b[31mfifth", "sixth", "seventh",
].join("\n");

export default async function ({ page, url }) {
  page.setDefaultTimeout(5000); // a dialog that never appears fails here, not at the runner's 30 s limit
  await writtenCase(page, url);
  await overlayCase(page, url);
  await confirmCase(page, url);
  await cancelCase(page, url);
  await preferenceCase(page, url);
  await markupCase(page, url);
  await windowsPasteCase(page, url);
  await fallbackCase(page, url);
  await macosPasteCase(page, url);
  await clipboardChordsCase(page, url);
  await fieldCase(page, url);
  await chordRowsCase(page, url);
}

/** Written, empty and an unknown answer do nothing more; an empty clipboard sends nothing. */
async function writtenCase(page, url) {
  await open(page, url);
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  assert.equal(await paste(page, "ls"), true, "the page handles the paste event itself");
  assert.deepEqual(await pastes(page), [{ pane: 2, text: "ls", allow_unsafe: false }]);
  await settle(page);
  assert.equal(await dialog(page).count(), 0, "a written paste shows nothing");

  await paste(page, "");
  await paste(page, null);
  assert.equal((await pastes(page)).length, 1, "an empty clipboard sends nothing");

  await page.evaluate(() => window.__MOCK__.on("paste", () => "empty"));
  await paste(page, "\n");
  assert.equal((await pastes(page)).length, 2);
  await settle(page);
  assert.equal(await dialog(page).count(), 0, "an empty answer shows nothing");

  await page.evaluate(() => window.__MOCK__.on("paste", null)); // the stub answers null: not an outcome
  await paste(page, "a\nb");
  await settle(page);
  assert.equal((await pastes(page)).length, 3, "an unknown answer is not followed by a second paste");
  assert.equal(await dialog(page).count(), 0);
}

/** A rejected paste opens a modal dialog with the line count, a bounded preview and two buttons. */
async function overlayCase(page, url) {
  await open(page, url);
  await rejectUnsafe(page);
  await paste(page, SEVEN);
  await dialog(page).waitFor({ state: "visible" });

  const d = await page.evaluate(() => {
    const root = document.querySelector('[role="dialog"]');
    const labelId = root.getAttribute("aria-labelledby");
    return {
      modal: root.getAttribute("aria-modal"),
      title: document.getElementById(labelId)?.textContent ?? null,
      lines: [...root.querySelectorAll(".dlg-line")].map((n) => n.textContent),
      more: root.querySelector(".dlg-more")?.textContent ?? null,
      note: root.querySelector(".dlg-note")?.textContent ?? null,
      buttons: [...root.querySelectorAll("button")].map((b) => b.textContent),
      focusInside: root.contains(document.activeElement),
      radius: getComputedStyle(root).borderRadius,
    };
  });
  assert.equal(d.modal, "true");
  assert.match(d.title, /7 lines/, "the title names the number of lines");
  assert.equal(d.lines.length, 5, "at most 5 preview lines");
  assert.deepEqual(d.lines.slice(0, 2), ["first", "second"]);
  assert.ok([...d.lines[2]].length <= 81 && d.lines[2].endsWith("\u2026"), "a long line is cut at 80 characters");
  assert.ok(d.lines[4].startsWith("\u241b[31m"), `control characters are shown as markers: ${JSON.stringify(d.lines[4])}`);
  assert.ok(!d.lines.join("").includes("\u001b"), "no raw escape in the preview");
  assert.match(d.more, /2 more lines/);
  assert.match(d.note, /control sequences/, "the overlay says the text holds terminal control sequences");
  assert.deepEqual(d.buttons, ["Cancel", "Paste"]);
  assert.equal(d.focusInside, true, "the dialog has the focus");
  assert.equal(d.radius, "8px", "a floating layer has the 8 px radius of the design");

  // The keyboard belongs to the dialog: no key reaches the pane and no second paste is made.
  await page.keyboard.press("a");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Meta+d");
  assert.deepEqual(await callsOf(page, "key"), [], "keys do not reach the terminal while the dialog is open");
  assert.deepEqual(await callsOf(page, "split"), [], "nor do shortcuts");
  await paste(page, "another\ntext");
  assert.equal((await pastes(page)).length, 1, "a paste while the dialog is open is ignored");

  // The terminal input must not hold the keys either: leaving by Escape gives them back.
  await page.keyboard.press("Escape");
  await dialog(page).waitFor({ state: "detached" });
  await focusBack(page);
  assert.equal(await active(page), "ime", "focus returns to the terminal input");
  const before = (await callsOf(page, "key")).length;
  await page.keyboard.press("a");
  assert.equal((await callsOf(page, "key")).length, before + 2, "typing works again");
}

/** Enter and the Paste button send the same text again with allow_unsafe true. */
async function confirmCase(page, url) {
  for (const how of ["Enter", "button"]) {
    await open(page, url);
    await rejectUnsafe(page);
    await paste(page, SEVEN);
    await dialog(page).waitFor({ state: "visible" });
    if (how === "Enter") await page.keyboard.press("Enter");
    else await dialog(page).getByRole("button", { name: "Paste" }).click();
    await dialog(page).waitFor({ state: "detached" });
    assert.deepEqual(
      await pastes(page),
      [{ pane: 2, text: SEVEN, allow_unsafe: false }, { pane: 2, text: SEVEN, allow_unsafe: true }],
      `${how}: the same text is sent again, allowed`,
    );
    await focusBack(page);
    assert.deepEqual(await callsOf(page, "key"), [], `${how}: the key that confirmed did not reach the pane`);
  }
}

/** Escape, the Cancel button and Enter on Cancel send nothing; Tab stays inside the dialog. */
async function cancelCase(page, url) {
  for (const how of ["Escape", "button", "tab-enter", "backdrop"]) {
    await open(page, url);
    await rejectUnsafe(page);
    await paste(page, "one\ntwo");
    await dialog(page).waitFor({ state: "visible" });
    if (how === "Escape") await page.keyboard.press("Escape");
    else if (how === "button") await dialog(page).getByRole("button", { name: "Cancel" }).click();
    else if (how === "tab-enter") {
      await page.keyboard.press("Tab");
      const inside = await page.evaluate(() => document.querySelector('[role="dialog"]').contains(document.activeElement));
      assert.equal(inside, true, "Tab stays inside the dialog");
      assert.equal(await page.evaluate(() => document.activeElement.textContent), "Cancel");
      await page.keyboard.press("Tab");
      assert.equal(await page.evaluate(() => document.activeElement.textContent), "Paste", "Tab wraps around");
      await page.keyboard.press("Shift+Tab");
      assert.equal(await page.evaluate(() => document.activeElement.textContent), "Cancel", "Shift+Tab goes back");
      await page.keyboard.press("Enter");
    } else {
      await page.mouse.click(3, 3); // outside the box
    }
    await dialog(page).waitFor({ state: "detached" });
    assert.equal((await pastes(page)).length, 1, `${how}: nothing is sent after the first, rejected paste`);
    await focusBack(page);
    assert.deepEqual(await callsOf(page, "key"), [], `${how}: no key reached the pane`);
  }
}

/** The preference decides whether to ask; the bracket-end marker always asks. */
async function preferenceCase(page, url) {
  await open(page, url);
  await rejectUnsafe(page);
  await page.evaluate(async () => {
    const mod = await import(new URL("keyboard.js", location.href).href);
    mod.setPasteConfirm(false);
  });
  await paste(page, "x\ny");
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length >= 2);
  assert.deepEqual(await pastes(page), [
    { pane: 2, text: "x\ny", allow_unsafe: false },
    { pane: 2, text: "x\ny", allow_unsafe: true },
  ], "with the preference off a rejected paste is re-sent without asking");
  await settle(page);
  assert.equal(await dialog(page).count(), 0);

  await paste(page, "x\u001b[201~\nrm -rf ~");
  await dialog(page).waitFor({ state: "visible" });
  assert.equal((await pastes(page)).length, 3, "the marker case asked first: only the unsafe=false paste was made");
  await page.keyboard.press("Escape");
  await dialog(page).waitFor({ state: "detached" });
  assert.equal((await pastes(page)).length, 3, "and cancelled sends nothing");

  await page.evaluate(async () => {
    const mod = await import(new URL("keyboard.js", location.href).href);
    mod.setPasteConfirm(true);
  });
  await paste(page, "p\nq");
  await dialog(page).waitFor({ state: "visible" });
  await page.keyboard.press("Escape");
  await dialog(page).waitFor({ state: "detached" });
}

/** The preview is text: markup in the clipboard is never parsed. */
async function markupCase(page, url) {
  await open(page, url);
  await rejectUnsafe(page);
  const hostile = '<img src=x onerror="window.__pwned=1"><b>bold</b>\n<script>window.__pwned=1</script>';
  await paste(page, hostile);
  await dialog(page).waitFor({ state: "visible" });
  const found = await page.evaluate(() => ({
    elements: document.querySelectorAll('[role="dialog"] img, [role="dialog"] b, [role="dialog"] script').length,
    first: document.querySelector(".dlg-line").textContent,
    pwned: window.__pwned,
  }));
  assert.equal(found.elements, 0, "the clipboard text was parsed as markup");
  assert.equal(found.first, '<img src=x onerror="window.__pwned=1"><b>bold</b>');
  assert.equal(found.pwned, undefined);
  await page.keyboard.press("Escape");
  await dialog(page).waitFor({ state: "detached" });
}

// ---- Windows and Linux: no native Edit menu ------------------------------------------------------------

/** Open the page as the core's `app_info` names the platform. Every case registers one: an earlier init script stays. */
async function openOn(page, url, os) {
  await page.addInitScript((i) => window.__MOCK__.on("app_info", () => i), { version: "0.0.1-alpha1", os, hostname: "woodbox" });
  await open(page, url);
}

/** A `keydown` on the terminal input. Untrusted, so the browser's own default never runs; returns defaultPrevented. */
const keydown = (page, init) =>
  page.evaluate((i) => {
    const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...i });
    document.getElementById("ime").dispatchEvent(e);
    return e.defaultPrevented;
  }, init);

const CTRL_V = { code: "KeyV", key: "v", ctrlKey: true };
const CTRL_SHIFT_V = { code: "KeyV", key: "V", ctrlKey: true, shiftKey: true };
const SHIFT_INSERT = { code: "Insert", key: "Insert", shiftKey: true };
const CTRL_C = { code: "KeyC", key: "c", ctrlKey: true };
const CTRL_SHIFT_C = { code: "KeyC", key: "C", ctrlKey: true, shiftKey: true };
const CTRL_INSERT = { code: "Insert", key: "Insert", ctrlKey: true };

/** Replace `navigator.clipboard.readText` with one that answers `text` after `delay` ms and counts its calls. */
const stubClipboard = (page, text, delay = 0) =>
  page.evaluate(
    ([t, d]) => {
      window.__reads = 0;
      navigator.clipboard.readText = () => {
        window.__reads++;
        return new Promise((done) => setTimeout(() => done(t), d));
      };
    },
    [text, delay],
  );
const reads = (page) => page.evaluate(() => window.__reads);
const wait = (page, ms) => page.waitForTimeout(ms);
/** Longer than the page's wait for a `paste` event (100 ms) and its late-event window start. */
const SETTLE_MS = 350;

/** The primary route: the chord keeps its default, the browser's `paste` event pastes once, readText is never asked. */
async function windowsPasteCase(page, url) {
  for (const os of ["windows", "linux"]) {
    await openOn(page, url, os);
    await stubClipboard(page, "from the clipboard");
    await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
    const chords = os === "windows" ? [CTRL_SHIFT_V, SHIFT_INSERT, CTRL_V] : [CTRL_SHIFT_V, SHIFT_INSERT];
    for (const [n, chord] of chords.entries()) {
      assert.equal(await keydown(page, chord), false, `${os}: ${JSON.stringify(chord)} keeps its default so the browser pastes`);
      assert.equal((await callsOf(page, "key")).length, 0, `${os}: the chord is not terminal input`);
      assert.equal(await paste(page, `text ${n}`), true, "the page takes the paste event itself");
      await wait(page, SETTLE_MS);
      assert.equal((await pastes(page)).length, n + 1, `${os}: exactly one paste for chord ${n}`);
      assert.deepEqual((await pastes(page))[n], { pane: 2, text: `text ${n}`, allow_unsafe: false });
    }
    assert.equal(await reads(page), 0, `${os}: readText is not asked when the event came`);
  }
}

/** The fallback route: no `paste` event, so the page reads the clipboard once; a late event does not paste again. */
async function fallbackCase(page, url) {
  await openOn(page, url, "windows");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await stubClipboard(page, "from readText");
  assert.equal(await keydown(page, CTRL_V), false);
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length >= 1);
  assert.deepEqual(await pastes(page), [{ pane: 2, text: "from readText", allow_unsafe: false }]);
  assert.equal(await reads(page), 1, "the clipboard was read once");
  await paste(page, "late event");
  await wait(page, SETTLE_MS);
  assert.equal((await pastes(page)).length, 1, "a late paste event after the fallback does not paste a second time");

  // The same while readText is still waiting: the late event arrives first and is ignored.
  await openOn(page, url, "windows");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await stubClipboard(page, "slow", 300);
  await keydown(page, CTRL_SHIFT_V);
  await page.waitForFunction(() => window.__reads === 1);
  await paste(page, "late");
  await wait(page, 600);
  assert.deepEqual(await pastes(page), [{ pane: 2, text: "slow", allow_unsafe: false }], "one paste, from the fallback");

  // An empty clipboard sends nothing; a clipboard that refuses sends nothing.
  await openOn(page, url, "linux");
  await stubClipboard(page, "");
  await keydown(page, SHIFT_INSERT);
  await wait(page, SETTLE_MS);
  assert.equal(await reads(page), 1);
  assert.equal((await pastes(page)).length, 0, "an empty clipboard sends nothing");
  await page.evaluate(() => {
    navigator.clipboard.readText = () => Promise.reject(new Error("denied"));
  });
  await keydown(page, SHIFT_INSERT);
  await wait(page, SETTLE_MS);
  assert.equal((await pastes(page)).length, 0, "a refused read sends nothing");

  // Both routes end in the one paste path: several lines ask first.
  await openOn(page, url, "windows");
  await rejectUnsafe(page);
  await stubClipboard(page, SEVEN);
  await keydown(page, CTRL_V);
  await dialog(page).waitFor({ state: "visible" });
  assert.deepEqual(await pastes(page), [{ pane: 2, text: SEVEN, allow_unsafe: false }], "asked before anything unsafe was sent");
  await page.keyboard.press("Escape");
  await dialog(page).waitFor({ state: "detached" });
  assert.equal((await pastes(page)).length, 1, "cancelled sends nothing more");

  // A held chord pastes once: the first press keeps its default, every repeat is consumed.
  await openOn(page, url, "windows");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await stubClipboard(page, "once");
  assert.equal(await keydown(page, CTRL_SHIFT_V), false);
  for (let i = 0; i < 5; i++) assert.equal(await keydown(page, { ...CTRL_SHIFT_V, repeat: true }), true, "a repeat is consumed");
  await wait(page, SETTLE_MS);
  assert.equal((await pastes(page)).length, 1, "a held paste chord pastes once");
  assert.equal(await reads(page), 1);
  assert.equal((await callsOf(page, "key")).length, 0, "and none of it was terminal input");
}

/** Copy chords run the copy; plain Ctrl+C, and Ctrl+V on Linux, are the program's. */
async function clipboardChordsCase(page, url) {
  await openOn(page, url, "windows");
  for (const chord of [CTRL_SHIFT_C, CTRL_INSERT]) {
    const before = (await callsOf(page, "selection_text")).length;
    assert.equal(await keydown(page, chord), true, `${JSON.stringify(chord)} is consumed`);
    assert.equal((await callsOf(page, "selection_text")).length, before + 1, "the copy of the selection ran");
  }
  assert.equal((await callsOf(page, "key")).length, 0, "the copy chords are not terminal input");

  const copies = (await callsOf(page, "selection_text")).length;
  assert.equal(await keydown(page, CTRL_C), true);
  assert.equal((await callsOf(page, "selection_text")).length, copies, "plain Ctrl+C is never a copy");
  const keys = await callsOf(page, "key");
  assert.equal(keys.length, 1, "it reaches the program as a key");
  assert.equal(keys[0].mods, 4, "with the Ctrl bit");
  assert.deepEqual([keys[0].code, keys[0].key], ["KeyC", "c"]);

  // A key with the Super flag that no chord owns is neither a command nor terminal input.
  assert.equal(await keydown(page, { code: "KeyX", key: "x", metaKey: true }), false);
  assert.equal((await callsOf(page, "key")).length, 1);

  await openOn(page, url, "linux");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await stubClipboard(page, "no");
  assert.equal(await keydown(page, CTRL_V), true, "on Linux Ctrl+V is forwarded to the program");
  await wait(page, SETTLE_MS);
  const linuxKeys = await callsOf(page, "key");
  assert.deepEqual(linuxKeys.map((k) => [k.code, k.mods]), [["KeyV", 4]]);
  assert.equal((await pastes(page)).length, 0);
  assert.equal(await reads(page), 0);
}

/** A text field of the page keeps copy and paste: no action runs, the default stays, nothing is pasted or read. */
async function fieldCase(page, url) {
  await openOn(page, url, "windows");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await stubClipboard(page, "must not be read");
  await page.evaluate(() => {
    const input = document.createElement("input");
    input.id = "probe";
    document.body.append(input);
    input.focus();
  });
  const onProbe = (init) =>
    page.evaluate((i) => {
      const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...i });
      document.getElementById("probe").dispatchEvent(e);
      return e.defaultPrevented;
    }, init);
  for (const chord of [CTRL_V, CTRL_SHIFT_V, SHIFT_INSERT, CTRL_SHIFT_C, CTRL_INSERT]) {
    assert.equal(await onProbe(chord), false, `${JSON.stringify(chord)} in a field keeps the browser's default`);
  }
  await wait(page, SETTLE_MS);
  assert.equal(await reads(page), 0, "the clipboard is not read for a field");
  assert.equal((await callsOf(page, "selection_text")).length, 0, "nor is the terminal's selection copied");
  assert.equal((await callsOf(page, "key")).length, 0, "nor does a key go to the terminal");
  const prevented = await page.evaluate(() => {
    const data = new DataTransfer();
    data.setData("text/plain", "into the field");
    const e = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
    document.getElementById("probe").dispatchEvent(e);
    return e.defaultPrevented;
  });
  assert.equal(prevented, false, "a paste event in the field is the field's");
  assert.equal((await pastes(page)).length, 0);
}

/** A binding row of the core runs its action on Windows and is consumed; on macOS the same event is no chord. */
async function chordRowsCase(page, url) {
  const rows = (chord) => [
    { action: "pane.split_right", chord },
    { action: "pane.close", chord: null },
  ];
  const setRows = (r) =>
    page.evaluate(async (list) => {
      const mod = await import(new URL("chords.js", location.href).href);
      mod.setBindingTable(list);
    }, r);
  const CTRL_SHIFT_D = { code: "KeyD", key: "D", ctrlKey: true, shiftKey: true };

  await openOn(page, url, "windows");
  await setRows(rows("Ctrl+Shift+D"));
  assert.equal(await keydown(page, CTRL_SHIFT_D), true, "the chord is consumed");
  await page.waitForFunction(() => window.__MOCK__.callsOf("split").length >= 1);
  assert.deepEqual(await callsOf(page, "split"), [{ column: false }]);
  assert.equal((await callsOf(page, "key")).length, 0, "and is not terminal input");

  await openOn(page, url, "macos");
  await setRows(rows("Cmd+D"));
  assert.equal(await keydown(page, CTRL_SHIFT_D), true, "forwarded to the program, so the page took it");
  await wait(page, 100);
  assert.equal((await callsOf(page, "split")).length, 0, "on macOS Ctrl+Shift+D is not a chord");
  assert.deepEqual((await callsOf(page, "key")).map((k) => [k.code, k.mods]), [["KeyD", 5]], "it is a key for the program");
}

// ---- macOS: the page owns Cmd+V too ----------------------------------------------------------------------

const CMD_V = { code: "KeyV", key: "v", metaKey: true };

/** Script the native pasteboard read: `answer` is returned (or thrown when it is a function that throws). */
const nativeText = (page, answer) =>
  page.evaluate((a) => {
    window.__native = 0;
    window.__MOCK__.on("clipboard_text", () => {
      window.__native++;
      if (a && a.reject) throw a.reject;
      return a;
    });
  }, answer);
const natives = (page) => page.evaluate(() => window.__native);

/**
 * macOS has the two routes of Windows and Linux: the chord keeps its default (the menu and the web view still paste), the
 * web view's `paste` event is the primary route, and only when it does not come within 100 ms the page reads the system
 * pasteboard through `clipboard_text` (the web view's own clipboard is no secure context under the app's scheme). Every
 * text still ends in the one paste path.
 */
async function macosPasteCase(page, url) {
  // The primary route: the event pastes once and the native read is never asked.
  await openOn(page, url, "macos");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await nativeText(page, "native text");
  assert.equal(await keydown(page, CMD_V), false, "Cmd+V keeps its default so the menu and the web view paste");
  assert.equal((await callsOf(page, "key")).length, 0, "Cmd+V is not terminal input");
  assert.equal(await paste(page, "from the event"), true, "the page takes the paste event itself");
  await wait(page, SETTLE_MS);
  assert.deepEqual(await pastes(page), [{ pane: 2, text: "from the event", allow_unsafe: false }], "exactly one paste");
  assert.equal(await natives(page), 0, "the pasteboard is not read when the event came");

  // The fallback: no event, so the pasteboard is read once through the shell and pasted once; a late event adds nothing.
  await openOn(page, url, "macos");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await nativeText(page, "native text");
  await stubClipboard(page, "must not be read");
  assert.equal(await keydown(page, CMD_V), false);
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length >= 1);
  assert.deepEqual(await pastes(page), [{ pane: 2, text: "native text", allow_unsafe: false }]);
  assert.equal(await natives(page), 1, "the native read ran once");
  assert.equal(await reads(page), 0, "the web view's clipboard was not needed");
  await paste(page, "late event");
  await wait(page, SETTLE_MS);
  assert.equal((await pastes(page)).length, 1, "a late paste event after the fallback does not paste again");

  // The native read fails or answers something that is not text: the web view's clipboard is the second try.
  for (const answer of [{ reject: "this platform has no native clipboard access" }, null, 5, { text: "x" }]) {
    await openOn(page, url, "macos");
    await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
    await nativeText(page, answer);
    await stubClipboard(page, "from readText");
    await keydown(page, CMD_V);
    await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length >= 1);
    assert.deepEqual(await pastes(page), [{ pane: 2, text: "from readText", allow_unsafe: false }], `${JSON.stringify(answer)}: readText answered`);
    assert.equal(await natives(page), 1);
    assert.equal(await reads(page), 1);
  }

  // An empty pasteboard text is no text, and a text field that both routes leave empty sends nothing.
  await openOn(page, url, "macos");
  await nativeText(page, "");
  await stubClipboard(page, "");
  await keydown(page, CMD_V);
  await wait(page, SETTLE_MS);
  assert.equal((await pastes(page)).length, 0, "nothing to paste: nothing is sent");
  await nativeText(page, { reject: "denied" });
  await page.evaluate(() => {
    navigator.clipboard.readText = () => Promise.reject(new Error("denied"));
  });
  await keydown(page, CMD_V);
  await wait(page, SETTLE_MS);
  assert.equal((await pastes(page)).length, 0, "both reads refused: nothing is sent");

  // Several lines ask first, whichever route brought them (D-15).
  await openOn(page, url, "macos");
  await rejectUnsafe(page);
  await nativeText(page, SEVEN);
  await keydown(page, CMD_V);
  await dialog(page).waitFor({ state: "visible" });
  assert.deepEqual(await pastes(page), [{ pane: 2, text: SEVEN, allow_unsafe: false }], "asked before anything unsafe was sent");
  await page.keyboard.press("Escape");
  await dialog(page).waitFor({ state: "detached" });
  assert.equal((await pastes(page)).length, 1, "cancelled sends nothing more");
  await nativeText(page, { reject: "denied" });
  await stubClipboard(page, SEVEN);
  await keydown(page, CMD_V);
  await dialog(page).waitFor({ state: "visible" });
  await page.keyboard.press("Escape");
  await dialog(page).waitFor({ state: "detached" });

  // A held chord pastes once: the first press keeps its default and every repeat is consumed.
  await openOn(page, url, "macos");
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await nativeText(page, "once");
  assert.equal(await keydown(page, CMD_V), false);
  for (let i = 0; i < 5; i++) assert.equal(await keydown(page, { ...CMD_V, repeat: true }), true, "a repeat is consumed");
  await wait(page, SETTLE_MS);
  assert.equal((await pastes(page)).length, 1, "a held Cmd+V pastes once");
  assert.equal(await natives(page), 1);
  assert.equal((await callsOf(page, "key")).length, 0, "and none of it was terminal input");

  // In a text field of the page Cmd+V is the field's: the default stays and nothing runs.
  await openOn(page, url, "macos");
  await nativeText(page, "must not be read");
  await page.evaluate(() => {
    const input = document.createElement("input");
    input.id = "probe";
    document.body.append(input);
    input.focus();
  });
  const prevented = await page.evaluate((i) => {
    const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...i });
    document.getElementById("probe").dispatchEvent(e);
    return e.defaultPrevented;
  }, CMD_V);
  assert.equal(prevented, false, "Cmd+V in a field keeps the browser's default");
  await wait(page, SETTLE_MS);
  assert.equal(await natives(page), 0, "the pasteboard is not read for a field");
  assert.equal((await pastes(page)).length, 0);
  assert.equal((await callsOf(page, "key")).length, 0);

  // Plain Ctrl+V is the program's: a key with the Ctrl bit, no paste, no read.
  await openOn(page, url, "macos");
  await nativeText(page, "must not be read");
  assert.equal(await keydown(page, CTRL_V), true, "Ctrl+V is forwarded to the program");
  await wait(page, SETTLE_MS);
  assert.deepEqual((await callsOf(page, "key")).map((k) => [k.code, k.mods]), [["KeyV", 4]]);
  assert.equal((await pastes(page)).length, 0);
  assert.equal(await natives(page), 0);
}
