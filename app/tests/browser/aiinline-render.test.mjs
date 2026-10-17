// The look of the actions of the chat on a narrow screen and in the accessibility modes (docs/design.md section 6.19): the
// prepared message at 16 px, the button under a code block a 44 px touch target, and contrast, motion and forced colours reaching
// the button and its warn line.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { APP_DIR } from "./lib.mjs";
import { VIEW_P1, boot, openSide, runAction, scriptReply, sendSide, setPrivacy, settle } from "./ai-lib.mjs";

const REPLY = '<p>Use this:</p><pre><code>find . -type f -size +1G</code></pre><pre><code>echo ‮evil</code></pre><p>Done.</p>';

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  await narrowCase(page, url);
  await modesCase(page, url, engine);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The column as the chat, with the prepared message and a finished reply that holds one clean block and one hidden one. */
async function columnWithReply(page, url, size) {
  await page.addInitScript(() => {
    try {
      localStorage.removeItem("holzwave.sidebar");
    } catch {
      // nothing remembered
    }
  });
  await boot(page, url, { view: VIEW_P1 });
  await setPrivacy(page, { localContext: "auto" });
  await page.setViewportSize(size);
  await settle(page);
  await openSide(page);
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('#sidebar .chat-consent[data-mode="offer"]');
}

const box = (page, selector) =>
  page.locator(selector).first().evaluate((node) => {
    const r = node.getBoundingClientRect();
    return { w: r.width, h: r.height, right: r.right, left: r.left };
  });

async function narrowCase(page, url) {
  await columnWithReply(page, url, { width: 390, height: 780 });
  const fontSize = await page.locator("#sidebar textarea.chat-input").evaluate((n) => parseFloat(getComputedStyle(n).fontSize));
  assert.ok(fontSize >= 16, `the prepared message is ${fontSize} px`);
  assert.ok((await page.locator("#sidebar textarea.chat-input").inputValue()).startsWith("Explain the error"));
  for (const selector of ["#sidebar .consent-again", "#sidebar .consent-discard"]) {
    assert.ok((await box(page, selector)).h >= 43.5, `${selector} is 44 px high`);
  }
  const overflow = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, client: document.documentElement.clientWidth }));
  assert.ok(overflow.scroll <= overflow.client, "no horizontal overflow of the document");

  await scriptReply(page, { text: "x", html: REPLY, send: sendSide });
  const button = await box(page, "#sidebar button.chat-insert");
  assert.ok(button.h >= 43.5 && button.w >= 43.5, `the button is ${button.w} by ${button.h} px`);
  const message = await box(page, '#sidebar .chat-msg[data-role="assistant"]');
  const bar = await box(page, "#sidebar .chat-codebar");
  assert.ok(bar.right <= message.right + 0.5 && bar.left >= message.left - 0.5, "the bar stays inside the message");

  // A desktop window keeps the 28 px target.
  await columnWithReply(page, url, { width: 1000, height: 640 });
  await scriptReply(page, { text: "x", html: REPLY, send: sendSide });
  const desktop = await box(page, "#sidebar button.chat-insert");
  assert.ok(desktop.h >= 27.5 && desktop.h < 43, `the desktop button is ${desktop.h} px high`);
}

async function modesCase(page, url, engine) {
  await columnWithReply(page, url, { width: 1000, height: 640 });
  await scriptReply(page, { text: "x", html: REPLY, send: sendSide });
  const style = (selector, prop) => page.locator(selector).first().evaluate((n, p) => getComputedStyle(n)[p], prop);
  const button = "#sidebar button.chat-insert";

  const line = await style(button, "borderTopColor");
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  assert.notEqual(await style(button, "borderTopColor"), line, "contrast-high reaches the hairline");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  for (const reduce of [false, true]) {
    await page.evaluate((on) => document.documentElement.classList.toggle("motion-reduce", on), reduce);
    assert.equal(await style(button, "transitionDuration"), "0s", `nothing moves (motion-reduce ${reduce})`);
  }
  await page.evaluate(() => document.documentElement.classList.remove("motion-reduce"));

  const css = readFileSync(`${APP_DIR}dist/inline.css`, "utf8");
  assert.match(css, /@media \(forced-colors: active\)\s*\{/, "a forced-colours block is shipped");

  // A press of Tab from the transcript reaches the button with the keyboard ring of 2 px (WebKit on a Mac tabs with Option).
  await page.locator("#sidebar .chat-log").focus();
  await page.keyboard.press(engine === "webkit" ? "Alt+Tab" : "Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("chat-insert")), true, "the button took the keyboard");
  assert.equal(await style(button, "outlineWidth"), "2px");
  for (const prop of ["backgroundImage", "filter", "boxShadow"]) assert.equal(await style(button, prop), "none", prop);

  const note = "#sidebar p.chat-codenote";
  assert.ok((await style(note, "textDecorationLine")).includes("underline"));
  assert.ok((await page.locator(note).first().textContent()).startsWith("!"));
}
