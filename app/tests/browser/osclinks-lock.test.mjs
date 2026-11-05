// The question before a link opens cannot lock itself (docs/design.md 6.31): when building the dialog fails, or its style sheet
// does not load, nothing half-built stays in the page, no page error is reported (the runner fails a test that has one), the
// keyboard is the terminal's again, and the next click with the gesture asks as usual.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";
import { frames, gestureClick, open, step, waitDialog } from "./osclinks.test.mjs";

/** What of the question is in the page: the backdrop and the dialog. */
const left = (page) =>
  page.evaluate(() => ({ backdrops: document.querySelectorAll(".dlg-backdrop").length, dialogs: document.querySelectorAll(".dlg-link").length }));

/**
 * Make the next call that `which` names throw once, and then work again: `attr` is `setAttribute("aria-modal")` on the dialog, the
 * first step of its build; `focus` is `focus()` of its Cancel button, the last one (the key handler is in place by then).
 */
const failOnce = (page, which) =>
  page.evaluate((w) => {
    const owner = (w === "attr" ? Element : HTMLElement).prototype;
    const name = w === "attr" ? "setAttribute" : "focus";
    const original = owner[name];
    let spent = false;
    owner[name] = function (...args) {
      const mine = w === "attr" ? args[0] === "aria-modal" && this.classList.contains("dlg-link") : this.classList.contains("dlg-cancel");
      if (!spent && mine) {
        spent = true;
        owner[name] = original;
        throw new Error("forced failure while the dialog is built");
      }
      return original.apply(this, args);
    };
  }, which);

/** A second question is asked and answered with Escape: the lock is gone. */
async function asksAgain(page, g) {
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.keyboard.press("Escape");
  await waitDialog(page, false);
  assert.deepEqual(await left(page), { backdrops: 0, dialogs: 0 });
}

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await step("early failure", () => earlyCase(page, url));
  await step("late failure", () => lateCase(page, url));
  await step("style sheet", () => styleCase(page, url));
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A DOM call fails at the start of the build: nothing stays and the next click asks. */
async function earlyCase(page, url) {
  const g = await open(page, url);
  await failOnce(page, "attr");
  await gestureClick(page, g.pt(1, 5, 1));
  await frames(page, 3);
  await page.waitForTimeout(300);
  assert.deepEqual(await left(page), { backdrops: 0, dialogs: 0 }, "no half-built dialog or backdrop stays");
  assert.equal((await callsOf(page, "open_url")).length, 0, "a question that failed opens nothing");
  await asksAgain(page, g);
}

/** The build fails at its last step, when the key handler is already in place: the keys are the terminal's again as well. */
async function lateCase(page, url) {
  const g = await open(page, url);
  await failOnce(page, "focus");
  await gestureClick(page, g.pt(1, 5, 1));
  await frames(page, 3);
  await page.waitForTimeout(300);
  assert.deepEqual(await left(page), { backdrops: 0, dialogs: 0 }, "no half-built dialog or backdrop stays");
  await page.evaluate(() => document.getElementById("term").focus());
  await page.keyboard.type("x");
  await page.waitForFunction(() => window.__MOCK__.calls.some(([c]) => c === "input" || c === "key" || c === "text" || c === "write" || c === "send_input"));
  await asksAgain(page, g);
}

/** The style sheet of the dialogs does not load: the question still appears, and goes on asking. */
async function styleCase(page, url) {
  const g = await open(page, url);
  await page.route("**/dialogs.css", (route) => route.abort());
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.keyboard.press("Escape");
  await waitDialog(page, false);
  await page.unroute("**/dialogs.css");
  await asksAgain(page, g);
}
