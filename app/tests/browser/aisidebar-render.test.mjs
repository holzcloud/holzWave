// The AI form of the sidebar column on a narrow window and under the accessibility modes (design.md section 6.18): at 640 px
// and narrower the column is the screen with 44 px targets and 16 px composer text, the chat's own narrow rules and the provider
// sheet work inside it, and contrast, motion and forced colours reach the new controls.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { OTHER_PROFILE, PROFILE, SAVED_A, SAVED_B, TOOL_PATH, TOOL_TEXT, boot, openSide, pickChat, saveChats, scriptTool, sendSide, tickClock } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await narrowCase(page, url);
  await modesCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const CONTROLS = ".ai-side-back, .ai-side-title, .ai-side-new, .ai-side-pick, .ai-side-del";
const sizes = (page, selector) =>
  page.locator(selector).evaluateAll((nodes) =>
    nodes.filter((n) => n.getClientRects().length > 0).map((n) => {
      const r = n.getBoundingClientRect();
      return { cls: n.className, w: r.width, h: r.height };
    }));

async function columnOnA(page, url, extra = {}) {
  await boot(page, url, { clock: true, ...extra });
  await saveChats(page, [SAVED_A, SAVED_B]);
}

async function narrowCase(page, url) {
  await columnOnA(page, url);
  await page.setViewportSize({ width: 390, height: 780 });
  await openSide(page);
  await pickChat(page, "chat-saved-a");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden]) li.ai-side-row");

  const side = await page.evaluate(() => {
    const n = document.getElementById("sidebar");
    return { width: n.getBoundingClientRect().width, form: n.dataset.form, scroll: n.scrollWidth, client: n.clientWidth };
  });
  assert.equal(side.form, "chat");
  assert.ok(Math.abs(side.width - 390) <= 1, `the column is the screen (${side.width})`);
  assert.ok(side.scroll <= side.client, "no horizontal overflow");
  const found = await sizes(page, CONTROLS);
  assert.ok(found.length >= 6, "the controls were found");
  for (const c of found) assert.ok(c.h >= 43.5, `${c.cls} is ${c.h} px high`);
  for (const c of found.filter((n) => n.cls.includes("ai-side-del"))) assert.ok(c.w >= 43.5, `× is ${c.w} px wide`);
  await page.locator(".ai-side-title").click();
  assert.ok((await page.locator("#sidebar .chat-input").evaluate((n) => parseFloat(getComputedStyle(n).fontSize))) >= 16, "composer text of 16 px");
  for (const c of await sizes(page, "#sidebar .chat-send, #sidebar .chat-chip, #sidebar .chat-input")) assert.ok(c.h >= 43.5, `${c.cls} is ${c.h} px high`);

  // An approval card in the column: the strip is at most 60 % of the screen and scrolls.
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, result: TOOL_TEXT, answer: "Done." });
  await sendSide(page, "read my notes");
  await page.waitForSelector("#sidebar section.chat-approval");
  const strip = await page.locator("#sidebar .chat-cards").evaluate((n) => ({
    h: n.getBoundingClientRect().height, scroll: n.scrollHeight, client: n.clientHeight, overflowY: getComputedStyle(n).overflowY,
    wide: n.scrollWidth <= n.clientWidth,
  }));
  assert.ok(strip.h <= 780 * 0.6 + 1, `the strip is ${strip.h} px`);
  assert.ok(strip.scroll <= strip.client || ["auto", "scroll"].includes(strip.overflowY), "a taller strip scrolls");
  assert.equal(strip.wide, true);
  for (const c of await sizes(page, "#sidebar .chat-approval button")) assert.ok(c.h >= 43.5, `${c.cls} is ${c.h} px high`);

  // Two providers: the picker opened from the chip is a sheet fixed to the bottom edge.
  await columnOnA(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  await page.setViewportSize({ width: 390, height: 780 });
  await openSide(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
  await page.waitForSelector("#sidebar button.chat-chip");
  await page.locator("#sidebar button.chat-chip").click();
  await page.waitForSelector("#sidebar .chat-picker");
  const sheet = await page.locator("#sidebar .chat-picker").evaluate((n) => {
    const r = n.getBoundingClientRect();
    return { position: getComputedStyle(n).position, bottom: r.bottom, width: r.width };
  });
  assert.equal(sheet.position, "fixed");
  assert.ok(Math.abs(sheet.bottom - 780) <= 1, `at the bottom edge (${sheet.bottom})`);
  assert.ok(Math.abs(sheet.width - 390) <= 1);

  // A desktop window: the controls keep 28 px, the form is 23.333 rem wide and the way back gives the 18 rem sidebar.
  await columnOnA(page, url);
  await page.setViewportSize({ width: 1000, height: 640 });
  await openSide(page);
  await pickChat(page, "chat-saved-a");
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden]) li.ai-side-row");
  const rem = await page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).fontSize));
  for (const c of await sizes(page, CONTROLS)) assert.ok(c.h >= 27.5, `${c.cls} is ${c.h} px high`);
  assert.ok(Math.abs((await page.locator("#sidebar").evaluate((n) => n.getBoundingClientRect().width)) - (70 / 3) * rem) < 1);
  await page.locator(".ai-side-back").click();
  await page.waitForSelector('#sidebar[data-form="sidebar"]');
  assert.ok(Math.abs((await page.locator("#sidebar").evaluate((n) => n.getBoundingClientRect().width)) - 18 * rem) < 1);
}

async function modesCase(page, url) {
  await boot(page, url);
  await saveChats(page, [SAVED_A, SAVED_B]);
  await page.setViewportSize({ width: 1000, height: 640 });
  await openSide(page);
  // A control reached with the keyboard has a 2 px ring (before any press of the mouse, which makes the ring unwanted).
  await page.keyboard.press("Shift");
  await page.locator(".ai-side-new").focus();
  assert.equal(await page.locator(".ai-side-new").evaluate((n) => getComputedStyle(n).outlineWidth), "2px");
  await pickChat(page, "chat-saved-a");
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden]) li.ai-side-row");
  const border = () => page.locator(".ai-side-head").evaluate((n) => getComputedStyle(n).borderBottomColor);
  const before = await border();
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  assert.notEqual(await border(), before, "the hairline follows contrast-high");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  // The form has no motion, with or without the setting.
  const motion = () =>
    page.evaluate(() =>
      [".ai-side-title", ".ai-side-new", ".ai-side-pick", ".ai-side-del"].map((s) => getComputedStyle(document.querySelector(s)).transitionDuration));
  for (const reduce of [false, true]) {
    await page.evaluate((on) => document.documentElement.classList.toggle("motion-reduce", on), reduce);
    for (const d of await motion()) assert.equal(d, "0s");
  }

  const css = readFileSync(new URL("../../src/aisidebar.css", import.meta.url), "utf8");
  assert.match(css, /@media \(forced-colors: active\)[^{]*\{[^}]*(Highlight|ButtonText)/, "a forced-colours block with a system colour");

  // No gradient, filter or shadow on any element of the column's own parts.
  const flat = await page.evaluate(() =>
    [...document.querySelectorAll(".sb-ai, .ai-side-head, .ai-side-bar, .ai-side-title, .ai-side-new, .ai-side-status, .ai-side-chats, .ai-side-row, .ai-side-pick, .ai-side-del")]
      .filter((n) => {
        const s = getComputedStyle(n);
        return s.backgroundImage !== "none" || s.filter !== "none" || s.boxShadow !== "none";
      }).map((n) => n.className));
  assert.deepEqual(flat, []);
}
