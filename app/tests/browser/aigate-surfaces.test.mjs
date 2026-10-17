// Seven surfaces of the chat page against the shared hostile strings: a streaming reply, a typed message, refusals, the provider
// chip and picker, the providers dialog, the pane header and the sidebar list. Each shows the strings as text: no forbidden
// element, no attribute that carries the text, `window.__XSS` unset, no polluted prototype, a page that still answers.
import assert from "node:assert/strict";
import { PROFILE, SAVED_A, VIEW_CHAT_ONLY, boot, chatBlock, openAi, openDialog, openSide, saveChats, sendText } from "./ai-lib.mjs";
import {
  HOSTILE, POLLUTION_JSON, armGuards, chars, finish, readWatch, surface, surfaceContext, watchDom,
} from "./aigate-lib.mjs";

const TEXT = HOSTILE.map((h) => h.text);
const JOINED = TEXT.join("\n");
const IDS = HOSTILE.map((h) => h.id);

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(10000);
  const ctx = surfaceContext(page, url, armGuards(page, url));
  await surface(ctx, "stream", (probe) => streamRun(ctx, probe));
  await surface(ctx, "user", () => userRun(ctx));
  await surface(ctx, "faults", (probe) => faultsRun(ctx, probe));
  await surface(ctx, "chip", () => chipRun(ctx));
  await surface(ctx, "providers", (probe) => providersRun(ctx, probe));
  await surface(ctx, "header", (probe) => headerRun(ctx, probe));
  await surface(ctx, "sidebar", () => sidebarRun(ctx));
  finish(ctx, ["stream", "user", "faults", "chip", "providers", "header", "sidebar"]);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The profiles of the chip and the dialog: one per string as name, model and host. */
const profilesOf = () =>
  HOSTILE.map((h, i) => ({
    ...PROFILE, id: `hostile-${i + 1}`, name: h.text, model: h.text, host: h.text, preset: "custom", wire: "openai_chat", locality: "lan",
  }));

/** One turn whose reply is all the strings, one character at a time (the long line in pieces of 250, which is the same for the page). */
async function streamRun({ page, url }, probe) {
  await boot(page, url, { profiles: [PROFILE] });
  await openAi(page);
  const chunks = HOSTILE.flatMap((h, i) => {
    const pieces = h.id === "long-line" ? h.text.match(/[^]{1,250}/g) : chars(h.text);
    return i === 0 ? pieces : ["\n", ...pieces];
  });
  assert.equal(chunks.join(""), JOINED, "the chunks make the joined text");
  await watchDom(page, ".chat-log", { checkIn: ".chat-text", textOnlyBelow: ".chat-partial .chat-text", mode: "text" });
  await page.evaluate((parts) => {
    window.__MOCK__.ai.script({ chunks: parts });
    // The core's renderer: the text as one escaped paragraph.
    window.__MOCK__.on("markdown_render", ({ text }) => `<p>${text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")}</p>`);
  }, chunks);
  await sendText(page, "go");
  await page.waitForFunction(() => document.querySelector('.chat-msg[data-role="assistant"][data-status="complete"]') && !document.querySelector(".chat-partial"), null, { timeout: 30000 });
  await page.waitForFunction(() => document.querySelector(".chat-md"));
  const said = await page.locator('.chat-msg[data-role="assistant"] .chat-text').textContent();
  // The HTML parser drops a NUL in text (the page copies the core's HTML through it), so the message has all but that one.
  const expected = JOINED.replaceAll(String.fromCodePoint(0), "");
  if (said !== expected) probe.fail(`the finished message is not the streamed text (${said.length} of ${expected.length} characters)`);
  for (const line of await readWatch(page)) probe.fail(`while it streamed: ${line}`);
  return { scope: ".chat-log", shown: true };
}

/** The typed message: the strings sent as the text of the user; the log shows them as text. */
async function userRun({ page, url }) {
  await boot(page, url, { profiles: [PROFILE] });
  await openAi(page);
  await sendText(page, JOINED);
  await page.waitForSelector('.chat-msg[data-role="user"]');
  return { scope: '.chat-msg[data-role="user"]', shown: true };
}

/** Five turns that fail with the first five strings, and a refused send for each of the others. */
async function faultsRun({ page, url }, probe) {
  await boot(page, url, { profiles: [PROFILE] });
  await openAi(page);
  const first = HOSTILE.slice(0, 5);
  for (const [i, h] of first.entries()) {
    await page.evaluate((text) => window.__MOCK__.ai.script({ fail: { class: "overload", text } }), h.text);
    await sendText(page, `fail ${i}`);
    await page.waitForFunction((n) => document.querySelectorAll(".chat-end").length === n && !document.querySelector(".chat-partial"), 1);
    await page.waitForFunction((word) => document.querySelector(".chat-end")?.textContent.includes(word), h.shows);
    await probe({ scope: ".chat-end, .chat-status", put: [h.id], shown: true, label: `turn failed with ${h.id}` });
  }
  for (const h of HOSTILE.slice(5)) {
    await page.evaluate((text) => {
      window.__MOCK__.on("ai_send", () => {
        throw { code: "busy", reason: null, text };
      });
    }, h.text);
    await sendText(page, `refused ${h.id}`);
    await page.waitForFunction((word) => document.querySelector(".chat-status")?.textContent.includes(word), h.id === "long-line" ? h.head : h.shows);
    await probe({
      scope: ".chat-end, .chat-status", put: [h.id], shown: h.id === "long-line" ? [] : true, cut: h.id === "long-line" ? [h.id] : [],
      label: `send refused with ${h.id}`,
    });
  }
}

/** Fourteen profiles named, modelled and hosted by the strings: the bar and the picker. The answers also carry a polluting member. */
async function chipRun({ page, url }) {
  await boot(page, url, { profiles: profilesOf() });
  await page.evaluate((json) => {
    window.__MOCK__.on("ai_profiles", () => {
      const list = window.__MOCK__.ai.profiles.map(({ locality, ...rest }) => rest);
      return { profiles: list, notes: [], ...JSON.parse(json) };
    });
  }, POLLUTION_JSON);
  await openAi(page);
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  // A label is cut at 120 characters, so the long line shows its start.
  return { scope: ".chat-bar, .chat-picker", shown: IDS.filter((id) => id !== "long-line"), cut: ["long-line"] };
}

/** The providers dialog with the same profiles, notes made of the strings, and a refused save with each string as its fault. */
async function providersRun({ page, url }, probe) {
  await boot(page, url, { profiles: profilesOf() });
  await openAi(page);
  await page.evaluate((notes) => {
    window.__MOCK__.ai.notes = notes;
  }, TEXT);
  await page.locator(".chat-setup").click();
  await openDialog(page);
  await probe({ scope: ".dlg.ai-settings", shown: IDS.filter((id) => id !== "long-line"), cut: ["long-line"], label: "profiles and notes" });
  await page.locator("#aiset-name").fill("Local");
  await page.locator("#aiset-model").fill("llama3.2");
  for (const h of HOSTILE) {
    await page.evaluate((text) => {
      window.__MOCK__.on("ai_profile_save", () => {
        throw { code: "profile", reason: "invalid", text };
      });
    }, h.text);
    await page.locator(".aiset-save").click();
    await page.waitForFunction((word) => document.querySelector(".aiset-status")?.textContent.includes(word), h.id === "long-line" ? h.head : h.shows);
    await probe({ scope: ".dlg.ai-settings .aiset-status", put: [h.id], shown: h.id === "long-line" ? [] : true, cut: h.id === "long-line" ? [h.id] : [], label: `save refused with ${h.id}` });
  }
}

/** A chat whose first message is a string: the chat title and the header of the pane show it. */
async function headerRun({ page, url }, probe) {
  await boot(page, url, {
    profiles: [PROFILE],
    view: VIEW_CHAT_ONLY,
    blocks: [chatBlock("chat-x")],
    states: { "chat-x": { title: "", profile: "local-1", model: "llama3.2" } },
    starts: 1,
  });
  await page.waitForSelector(".block-chat");
  for (const h of HOSTILE) {
    await page.evaluate(([text]) => {
      const ts = 1800000000;
      window.__MOCK__.ai.setState("chat-x", {
        title: text, messages: [{ id: "first", role: "user", ts, model: null, status: "complete", usage: null, parts: [{ type: "text", text }] }],
      });
      window.dispatchEvent(new Event("focus"));
    }, [h.text]);
    await page.waitForFunction((word) => document.querySelector(".panehead .ph-ctx, .panehead .ph-title")?.parentElement.textContent.includes(word), h.id === "long-line" ? h.head : h.shows);
    await probe({
      scope: ".panehead, .block-chat .chat-log", put: [h.id], shown: h.id === "long-line" ? [] : true, cut: h.id === "long-line" ? [h.id] : [],
      label: `first message ${h.id}`,
    });
  }
}

/** Fourteen stored chats titled with the strings and the sidebar form open with its list. */
async function sidebarRun({ page, url }) {
  await boot(page, url, { profiles: [PROFILE] });
  await saveChats(
    page,
    HOSTILE.map((h, i) => ({ ...SAVED_A, id: `chat-hostile-${i + 1}`, title: h.text, agoMs: (i + 1) * 60000, messages: [["user", h.text], ["assistant", "ok"]] })),
  );
  await openSide(page);
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden]) li.ai-side-row");
  // A title is cut at 80 characters, so the long line shows its start; the hidden characters of a title become blanks.
  return { scope: "#sidebar", shown: IDS.filter((id) => id !== "long-line"), cut: ["long-line"] };
}
