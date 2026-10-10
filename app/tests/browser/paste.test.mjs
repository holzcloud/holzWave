// Paste in the real page against the Tauri stub: the core decides, the page asks. A rejected paste shows a modal
// confirmation with the line count and a bounded preview; nothing risky is sent without it.
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
  assert.ok([...d.lines[2]].length <= 81 && d.lines[2].endsWith("…"), "a long line is cut at 80 characters");
  assert.ok(d.lines[4].startsWith("␛[31m"), `control characters are shown as markers: ${JSON.stringify(d.lines[4])}`);
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
    assert.equal(await active(page), "ime", `${how}: focus returns to the terminal input`);
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
    assert.equal(await active(page), "ime", `${how}: focus returns to the terminal input`);
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
