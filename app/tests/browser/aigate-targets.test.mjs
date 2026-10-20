// The touch-target sweep of the chat, its cards and its rows: at 390 by 780 every control is at least 44 CSS pixels in both
// dimensions, at 1000 by 640 at least 28. Each state names the controls it must show; a control that the table forgot is found
// by a scan of every visible control of the chat, and an exemption is named with its reason in `TARGET_EXEMPT`.
import assert from "node:assert/strict";
import {
  OTHER_PROFILE, PROFILE, VIEW3, boot, chatBlock, openCard, openContext, scriptCommand, scriptReply, scriptTool, scriptWrite, seedFile, sendText, setPrivacy,
  tickClock,
} from "./ai-lib.mjs";
import { TARGET_EXEMPT, WINDOWS, measure } from "./aigate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  assert.ok(TARGET_EXEMPT.length >= 2);
  for (const window of WINDOWS) await targetsCase(page, url, window);
  await sweepCanFail(page);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A terminal strip above a chat that takes the rest of the window: the shape of a phone, with a pane the chat can act for. */
const VIEW_STACK = {
  ...VIEW3,
  tabs: [{ title: "shell", active: true, focus: 1, panes: 2 }],
  workspaces: [{ name: "main", active: true, panes: 2 }],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 1, h: 0.08 }], [3, { x: 0, y: 0.08, w: 1, h: 0.92 }]],
  dividers: [{ split: 10, axis: "Column", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.08 }],
};

/** The chat of a window of `size`, with two providers so that the chip is a button, a terminal pane to act for and a frozen clock. */
async function open(page, url, { size }, extra = {}) {
  await page.setViewportSize(size);
  await boot(page, url, {
    view: VIEW_STACK,
    profiles: [PROFILE, OTHER_PROFILE],
    blocks: [chatBlock("chat-x")],
    states: { "chat-x": { title: "Earlier question", profile: "local-1", model: "llama3.2" } },
    starts: 2,
    clock: true,
    settings: true,
    ...extra,
  });
  await page.waitForSelector(".block-chat");
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
}

/** Measure the controls of the chat root for the window, and add what is wrong to `found` under the name of the state. */
async function sweep(page, window, found, state, selectors) {
  const lines = await measure(page, { selectors, scope: ".block-chat", min: window.min, narrow: window.narrow });
  for (const line of lines) found.push(`${window.name} / ${state}: ${line}`);
}

/** The chat bar, the picker and the composer; the title-bar button on a desktop only. */
async function chatState(page, url, window, found) {
  await open(page, url, window);
  await setPrivacy(page, { localOnly: true });
  await page.waitForSelector(".chat-local");
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  const selectors = [".chat-chip", ".chat-choice", ".chat-input", ".chat-send", ".chat-ctx", ".chat-local", ".chat-setup"];
  if (!window.narrow) selectors.push(".ai-btn");
  await sweep(page, window, found, "chat", selectors);
  // The title-bar button is exempt from the 44 pixel minimum (TARGET_EXEMPT) and not measured against it; its size is written down.
  const bar = await page.evaluate(() => {
    const r = document.querySelector(".ai-btn")?.getBoundingClientRect();
    return r === undefined ? null : [Math.round(r.width * 10) / 10, Math.round(r.height * 10) / 10];
  });
  console.log(`aigate targets: ${window.name}: the title-bar button is ${bar === null ? "not drawn" : bar.join(" by ")}`);
}

/** The consent card in its three modes. */
async function consentState(page, url, window, found) {
  await open(page, url, window);
  await openContext(page, "chat-x");
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  await sweep(page, window, found, "consent: ask", [".consent-share", ".consent-no"]);
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  await sweep(page, window, found, "consent: pick", ["label.consent-kind", ".consent-preview", ".consent-unshare", ".consent-close"]);
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  await sweep(page, window, found, "consent: offer", ["li.consent-item", ".consent-again", ".consent-discard"]);
}

/** Open the first approval card of a turn and wait for it. */
async function ask(page) {
  await sendText(page, "go");
  return openCard(page);
}

/** Allow the card with a real press after the window of half a second and wait for the answer to be drawn. */
async function allow(page, card) {
  await tickClock(page, 1000);
  await card.locator(".approval-allow").click();
  await page.waitForSelector("div.chat-tool");
}

/** The cards of a read, a write and a command, and the rows of the calls that ran. */
async function toolStates(page, url, window, found) {
  await open(page, url, window);
  await scriptTool(page, { id: "call_1", tool: "read_file", path: "/work/notes.txt", result: "text", answer: "Done." });
  let card = await ask(page);
  await sweep(page, window, found, "read card", [".approval-allow", ".approval-deny"]);
  await allow(page, card);
  await page.waitForSelector("button.tool-toggle");
  await sweep(page, window, found, "read row", [".tool-toggle"]);

  await open(page, url, window);
  await seedFile(page, "/work/proj/todo.txt", "alpha\nbeta\n");
  await scriptWrite(page, { id: "call_1", tool: "write_file", path: "/work/proj/todo.txt", content: "alpha\ngamma\n", answer: "Done." });
  card = await ask(page);
  await sweep(page, window, found, "write card", [".approval-allow", ".approval-deny"]);
  await allow(page, card);
  await page.waitForSelector("button.tool-undo");
  await sweep(page, window, found, "write row", [".tool-undo"]);

  await open(page, url, window);
  await scriptCommand(page, { id: "call_1", command: "ls -la", cwd: "/work/proj" });
  card = await ask(page);
  await sweep(page, window, found, "command card", [".approval-allow", ".approval-deny"]);
}

/** The button under a one-line code block of a reply. */
async function insertState(page, url, window, found) {
  await open(page, url, window);
  await scriptReply(page, { text: "x", html: "<pre><code>ls -la</code></pre>" });
  await page.waitForSelector("button.chat-insert");
  await sweep(page, window, found, "insert", ["button.chat-insert"]);
}

/** One window of the sweep: every state, and then the names of everything that was too small, together. */
async function targetsCase(page, url, window) {
  const found = [];
  await chatState(page, url, window, found);
  await consentState(page, url, window, found);
  await toolStates(page, url, window, found);
  await insertState(page, url, window, found);
  console.log(`aigate targets: ${window.name}: ${found.length} problems`);
  assert.deepEqual(found, [], `targets of the ${window.name} window`);
}

/**
 * The sweep can fail: a control that is too small and in no table is reported by the scan, one that is exempt is not, a selector
 * that matches nothing is reported, and a minimum nothing can meet makes every control of the table small.
 */
async function sweepCanFail(page) {
  await page.evaluate(() => {
    const tiny = document.createElement("button");
    tiny.className = "zz-tiny";
    tiny.style.cssText = "width: 10px; height: 10px";
    document.querySelector(".block-chat").append(tiny);
    const link = document.createElement("a");
    link.href = "https://example.org/";
    link.textContent = "x";
    link.style.cssText = "display: inline-block; width: 10px; height: 10px";
    const text = document.createElement("div");
    text.className = "chat-text";
    text.append(link);
    document.querySelector(".block-chat").append(text);
  });
  const lines = await measure(page, { selectors: [".chat-send", ".zz-missing"], scope: ".block-chat", min: 28, narrow: true });
  assert.ok(lines.some((l) => l.startsWith("unlisted") && l.includes("zz-tiny")), `the scan missed a small control: ${lines.join(" | ")}`);
  assert.equal(lines.filter((l) => l.startsWith("unlisted")).length, 1, "only the control that is not exempt is reported, not the link inside running text");
  assert.ok(lines.some((l) => l.startsWith("missing") && l.includes(".zz-missing")), "a selector that matches nothing is reported");
  const small = await measure(page, { selectors: [".chat-send"], scope: null, min: 9999, narrow: false });
  assert.ok(small.some((l) => l.startsWith("small")), "a minimum nothing can meet reports the control");
  console.log("aigate targets: the sweep reported a small control, a missing selector and a minimum nothing meets");
}
