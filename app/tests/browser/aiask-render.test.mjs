// The look of the card of a `wsh ai` request on a narrow screen and in the accessibility modes (docs/design.md section 6.20): a bottom
// sheet over the full width with 44 px targets and 16 px text at 640 px and narrower, a floating layer of at most 24 rem at the top
// right on a desktop, and contrast, motion and forced colours reaching it. It is a floating layer: it has the one shadow of the
// page and no backdrop.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { APP_DIR } from "./lib.mjs";
import { ASK, LOCAL_ASK, askCard, boot, emitAsk } from "./ai-lib.mjs";

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  await narrowCase(page, url);
  await modesCase(page, url, engine);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page of the given size with the card of `view` on screen. */
async function withCard(page, url, size, view = ASK) {
  await boot(page, url);
  await page.setViewportSize(size);
  await emitAsk(page, view);
  await askCard(page).waitFor();
}

const box = (page, selector) =>
  page.locator(selector).first().evaluate((node) => {
    const r = node.getBoundingClientRect();
    return { w: r.width, h: r.height, left: r.left, right: r.right, top: r.top, bottom: r.bottom };
  });

const style = (page, selector, prop) => page.locator(selector).first().evaluate((n, p) => getComputedStyle(n)[p], prop);

async function narrowCase(page, url) {
  await withCard(page, url, { width: 390, height: 780 });
  const card = await box(page, "section.ask");
  assert.ok(Math.abs(card.w - 390) <= 1, `the sheet spans the width: ${card.w}`);
  assert.ok(Math.abs(card.bottom - 780) <= 1.5, `the sheet sits at the bottom edge: ${card.bottom}`);
  const overflow = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, client: document.documentElement.clientWidth }));
  assert.ok(overflow.scroll <= overflow.client, "no horizontal overflow of the document");
  for (const selector of [".ask-text", ".ask-counts", ".ask-title", ".ask-lead", ".ask-route"]) {
    const size = parseFloat(await style(page, `section.ask ${selector}`, "fontSize"));
    assert.ok(size >= 15.99, `${selector} is ${size} px`);
  }
  for (const selector of ["button.ask-no", "button.ask-yes"]) {
    const target = await box(page, `section.ask ${selector}`);
    assert.ok(target.h >= 43.5 && target.w >= 43.5, `${selector} is ${target.w} by ${target.h} px`);
  }

  // On a desktop the card is a layer of at most 24 rem at the top right, and the targets keep the 28 px minimum.
  await withCard(page, url, { width: 1000, height: 640 });
  const rem = await page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).fontSize));
  const desk = await box(page, "section.ask");
  assert.ok(desk.w <= 24 * rem + 0.5, `the card is ${desk.w} px, at most ${24 * rem}`);
  assert.ok(desk.right <= 1000 + 0.5 && desk.left >= 0 && desk.top >= 0, "the card lies inside the window");
  assert.ok(desk.right >= 1000 - 2 * rem - 1, "at the right edge");
  const bar = await box(page, "#bar");
  assert.ok(desk.top >= bar.bottom - 0.5, `under the title bar: ${desk.top} against ${bar.bottom}`);
  for (const selector of ["button.ask-no", "button.ask-yes"]) {
    const target = await box(page, `section.ask ${selector}`);
    assert.ok(target.h >= 27.5 && target.h < 43, `${selector} is ${target.h} px high on a desktop`);
  }
  // A destination on this computer draws the same card without the warn mark.
  await withCard(page, url, { width: 1000, height: 640 }, LOCAL_ASK);
  assert.equal(await page.locator("section.ask .ask-best").count(), 0);
}

async function modesCase(page, url, engine) {
  await withCard(page, url, { width: 1000, height: 640 });
  const card = "section.ask";

  // Contrast: the hairlines follow the stronger ink.
  const line = await style(page, card, "borderTopColor");
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  assert.notEqual(await style(page, card, "borderTopColor"), line, "contrast-high reaches the hairline of the card");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  // Motion: nothing moves, with and without the root class.
  for (const reduce of [false, true]) {
    await page.evaluate((on) => document.documentElement.classList.toggle("motion-reduce", on), reduce);
    for (const selector of ["button.ask-no", "button.ask-yes", card]) {
      assert.equal(await style(page, `${card === selector ? "" : "section.ask "}${selector}`.trim(), "transitionDuration"), "0s", `${selector}, reduced ${reduce}`);
    }
  }
  await page.evaluate(() => document.documentElement.classList.remove("motion-reduce"));

  const css = readFileSync(`${APP_DIR}dist/ask.css`, "utf8");
  assert.match(css, /@media \(forced-colors: active\)\s*\{/, "a forced-colours block is shipped");

  // A press of Tab from the prompt reaches `Don't send` with the keyboard ring of 2 px (WebKit on a Mac tabs with Option).
  await page.locator("section.ask .ask-text").focus();
  await page.keyboard.press(engine === "webkit" && process.platform === "darwin" ? "Alt+Tab" : "Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("ask-no")), true, "the button took the keyboard");
  assert.equal(await style(page, "section.ask button.ask-no", "outlineWidth"), "2px");
  for (const selector of ["button.ask-no", "button.ask-yes"]) {
    for (const prop of ["backgroundImage", "filter", "boxShadow"]) assert.equal(await style(page, `section.ask ${selector}`, prop), "none", `${selector} ${prop}`);
  }

  // The card is a floating layer: it has the shadow of the dialogs, and no backdrop exists.
  assert.notEqual(await style(page, card, "boxShadow"), "none", "the card has the shadow of a floating layer");
  assert.equal(await page.locator(".dlg-backdrop").count(), 0, "no backdrop");
  assert.equal(await style(page, card, "position"), "fixed");

  // The chip that says the destination leaves this computer has the words and a colour of its own.
  const place = "section.ask .ask-place";
  assert.ok((await page.locator(place).textContent()).includes("leaves this computer"));
  const warn = await style(page, place, "color");
  assert.ok((await style(page, place, "textDecorationLine")).includes("underline"), "underlined as well as coloured");
  await withCard(page, url, { width: 1000, height: 640 }, LOCAL_ASK);
  assert.notEqual(await style(page, place, "color"), warn, "the chip of a destination on this computer has another colour");
  assert.ok(!(await page.locator(place).textContent()).includes("leaves"));
}
