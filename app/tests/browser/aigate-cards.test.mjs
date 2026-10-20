// Seven card surfaces against the shared hostile strings: the consent card, the approval cards of a read, a write and a command,
// the result row of a finished call, the card of a `wsh ai` request, and the button under a reply's code. Each shows the strings
// as text, with hidden characters written out where the surface promises it: no forbidden element, no attribute that carries
// the text, `window.__XSS` unset, no polluted prototype, a page that still answers.
import assert from "node:assert/strict";
import {
  ASK, PROFILE, VIEW_P1, askCard, askView, boot, closeAsk, diffOf, emitAsk, openAi, openCard, openContext, payloadOf, scriptCommand, scriptReply,
  scriptTool, scriptWrite, sendText, tickClock,
} from "./ai-lib.mjs";
import { HOSTILE, PAGE_ADDED, armGuards, finish, surface, surfaceContext } from "./aigate-lib.mjs";

const IDS = HOSTILE.map((h) => h.id);
const S = HOSTILE.map((h) => h.text);

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(10000);
  const ctx = surfaceContext(page, url, armGuards(page, url));
  await surface(ctx, "consent", (probe) => consentRun(ctx, probe));
  await surface(ctx, "read-card", () => readRun(ctx));
  await surface(ctx, "write-card", () => writeRun(ctx));
  await surface(ctx, "command-card", () => commandRun(ctx));
  await surface(ctx, "result-row", () => resultRun(ctx));
  await surface(ctx, "ask", (probe) => askRun(ctx, probe));
  await surface(ctx, "insert", (probe) => insertRun(ctx, probe));
  finish(ctx, ["consent", "read-card", "write-card", "command-card", "result-row", "ask", "insert"]);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The strings at positions `from`, as ids. */
const slice = (from, count) => Array.from({ length: count }, (_, i) => HOSTILE[(from + i) % HOSTILE.length].id);

/** A page with a chat beside the first terminal and one saved profile, the shape the consent cases use. */
async function chatBeside(page, url, extra = {}) {
  await boot(page, url, { view: VIEW_P1, settings: true, profiles: [PROFILE], ...extra });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

/**
 * The consent card in its three modes with the strings as the profile, the model, the host, the labels of the items and the
 * names of their findings. The answers of the three commands are written here, so the card draws what a hostile core could say.
 */
async function consentRun({ page, url }, probe) {
  await chatBeside(page, url);
  const answers = (state, offer) =>
    page.evaluate(
      ([s, o]) => {
        window.__ST = s;
        window.__OFFER = o;
        window.__MOCK__.on("ai_context_state", () => window.__ST);
        window.__MOCK__.on("ai_context_share", () => ({ ...window.__ST, shared: true }));
        window.__MOCK__.on("ai_context_preview", () => window.__OFFER);
      },
      [state, offer],
    );
  const stateOf = (profile, model, host) => ({
    profile, model, host, locality: "cloud", required: true, shared: false, kinds: ["selection", "screen", "last_command"], persist: false,
  });
  const offerOf = (first, count) => ({
    offer: "0123456789abcdef0123456789abcdef", profile: S[0], model: S[1], host: S[2], locality: "cloud", ttl_seconds: 300,
    items: Array.from({ length: count }, (_, i) => ({
      id: i + 1, kind: "screen", label: S[(first + i) % S.length], bytes: 100 + i,
      findings: [{ rule: S[(first + i + 1) % S.length], count: 1 }],
    })),
    empty: [], bytes: 1000, persist: false,
  });

  // The question: the profile, the model and the host are strings; five rounds put every string there.
  for (let round = 0; round < 5; round += 1) {
    const at = round * 3;
    await answers(stateOf(S[at % 14], S[(at + 1) % 14], S[(at + 2) % 14]), offerOf(0, 1));
    await openContext(page);
    await page.waitForSelector('.chat-consent[data-mode="ask"]');
    const put = slice(at, 3);
    const cut = put.includes("long-line") ? ["long-line"] : [];
    await probe({ scope: "section.chat-consent", put, shown: put.filter((id) => id !== "long-line"), cut, label: `question ${round + 1}` });
    await page.locator(".consent-no").click();
    await page.waitForFunction(() => document.querySelector(".chat-consent") === null);
  }

  // The items of an offer: eight labels, then six; each with a finding whose rule is a string.
  await answers(stateOf(S[0], S[1], S[2]), offerOf(0, 8));
  await openContext(page);
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  // The label of an item is shown; the rule of a finding is not (the card says `other change` for a rule it does not know) and
  // must not appear in an attribute either.
  await probe({ scope: "section.chat-consent", put: slice(0, 9), shown: slice(0, 8), cut: [], label: "items 1 to 8" });
  await answers(stateOf(S[0], S[1], S[2]), offerOf(8, 6));
  await page.locator(".consent-again").click();
  await page.waitForFunction(() => document.querySelectorAll(".consent-item").length === 6);
  await probe({
    scope: "section.chat-consent", put: slice(8, 7), shown: slice(8, 6).filter((id) => id !== "long-line"), cut: ["long-line"], label: "items 9 to 14",
  });
}

/** The numbers of the rows of the payload of a card, from the strings: one row each, whatever the card draws of it. */
const rowsOf = () => S.map((text, i) => [`field${i + 1}`, text]);

/** Open a turn that asks for tools and wait for the cards. */
async function askForCards(page, count) {
  await sendText(page, "go");
  await page.waitForFunction((n) => document.querySelectorAll("section.chat-approval").length === n, count);
}

/** A read request whose payload rows hold the strings, with the target and the taint labels made of strings too. */
async function readRun({ page, url }) {
  await chatBeside(page, url);
  await scriptTool(page, {
    calls: [
      { id: "call_1", tool: "read_file", payload: payloadOf("read_file", rowsOf()), target: S[0], remote: true, tainted: true, taint: S.slice(0, 8) },
      { id: "call_2", tool: "read_file", payload: payloadOf("read_file", rowsOf()), target: S[9], remote: false, tainted: true, taint: S.slice(8, 14) },
    ],
    answer: "Done.",
  });
  await askForCards(page, 2);
  return { scope: "section.chat-approval", shown: true };
}

/** A write whose diff rows (context, added and removed) and path hold the strings. */
async function writeRun({ page, url }) {
  await chatBeside(page, url);
  const rows = [];
  let line = 1;
  for (const [i, text] of S.entries()) {
    if (i < 5) rows.push({ op: "ctx", old: line, new: line, text });
    else if (i < 10) rows.push({ op: "add", old: null, new: line, text });
    else rows.push({ op: "del", old: line, new: null, text });
    line += 1;
  }
  await scriptWrite(page, {
    id: "call_1", tool: "write_file", path: S[0], content: "x", diff: diffOf(rows),
    payload: payloadOf("write_file", [["path", S[0]], ["content", S[1]]]), tainted: true, taint: [S[2]],
  });
  await askForCards(page, 1);
  return { scope: "section.chat-approval", shown: true };
}

/** A command made of the strings, with a working directory that is one of them. */
async function commandRun({ page, url }) {
  await chatBeside(page, url);
  await scriptCommand(page, { id: "call_1", command: S.join("\n"), cwd: S[0], tainted: true, taint: [S[1]] });
  await askForCards(page, 1);
  return { scope: 'section.chat-approval[data-effect="exec"]', shown: true };
}

/** A finished call whose result is the strings, with its toggle pressed. */
async function resultRun({ page, url }) {
  await chatBeside(page, url, { clock: true });
  await scriptTool(page, { id: "call_1", tool: "read_file", path: "/work/x.txt", result: S.join("\n"), answer: "Done." });
  await sendText(page, "go");
  const card = await openCard(page);
  await tickClock(page, 700);
  await card.locator(".approval-allow").click();
  await page.waitForSelector("div.chat-tool .tool-toggle");
  await page.locator("div.chat-tool .tool-toggle").click();
  await page.waitForSelector("div.chat-tool pre.tool-result");
  return { scope: "div.chat-tools", shown: true };
}

/** Five requests of `wsh ai` with strings as the prompt, the host, the profile and the model, and a refused answer. */
async function askRun({ page, url }, probe) {
  await boot(page, url, { clock: true });
  const hex = (n) => n.toString(16).padStart(32, "0");
  for (let round = 0; round < 5; round += 1) {
    const view = askView({
      ask: hex(round + 1), prompt: S[round], host: S[(round + 5) % 14], profile: S[(round + 9) % 14], model: S[(round + 10) % 14],
    });
    await emitAsk(page, view);
    await askCard(page).waitFor();
    const put = [...new Set([round, (round + 5) % 14, (round + 9) % 14, (round + 10) % 14].map((i) => IDS[i]))];
    await probe({
      scope: "section.ask", put, shown: put.filter((id) => id !== "long-line"), cut: put.includes("long-line") ? ["long-line"] : [],
      label: `request ${round + 1}`,
    });
    await closeAsk(page, view.ask, "gone");
    await page.waitForFunction(() => document.querySelector("section.ask") === null);
  }
  // A refused answer: the fault is the core's text, and the card shows it as text.
  for (const h of HOSTILE.slice(0, 3)) {
    const view = askView({ ask: hex(100) }, ASK);
    await page.evaluate((text) => window.__MOCK__.ai.ask.fail({ code: "ask", reason: null, text }), h.text);
    await emitAsk(page, view);
    await askCard(page).waitFor();
    await tickClock(page, 700);
    await askCard(page).locator(".ask-yes").click();
    await page.waitForFunction((word) => document.querySelector("section.ask .ask-fault")?.textContent.includes(word), h.shows);
    await probe({ scope: "section.ask", put: [h.id], shown: true, label: `answer refused with ${h.id}` });
    await page.evaluate(() => window.__MOCK__.ai.ask.fail(undefined));
    await closeAsk(page, view.ask, "gone");
    await page.waitForFunction(() => document.querySelector("section.ask") === null);
  }
}

/**
 * A reply whose code blocks are one clean command and the strings. A button is under every block that is one printable line of at
 * most 1024 characters (the clean command and twelve of the strings: what the button does is paste that line at the prompt after
 * a real click, never run it); the string of hidden characters gets the note that it is not insertable, and the line of 5000
 * letters gets nothing. At most sixteen bars exist, and no attribute of any button holds a string.
 */
async function insertRun({ page, url }, probe) {
  await chatBeside(page, url);
  const CLEAN = "find . -type f -size +1G";
  const escape = (text) => text.replace(/&/g, "&amp;").replace(/</g, "&lt;");
  const html = [CLEAN, ...S].map((code) => `<pre><code>${escape(code)}</code></pre>`).join("");
  await scriptReply(page, { text: "x", html });
  const found = await page.evaluate(() => ({
    buttons: document.querySelectorAll("button.chat-insert").length,
    bars: document.querySelectorAll(".chat-codebar, .chat-codenote").length,
    notes: document.querySelectorAll(".chat-codenote").length,
  }));
  if (found.buttons !== 13) probe.fail(`${found.buttons} insert buttons, 13 expected (the clean command and twelve printable lines)`);
  if (found.notes !== 1) probe.fail(`${found.notes} notes, one expected (the hidden characters)`);
  if (found.bars > 16) probe.fail(`${found.bars} bars, at most 16`);
  return { scope: ".chat-log .chat-text", put: IDS, shown: true, options: { mode: "model", extra: PAGE_ADDED }, label: "code blocks" };
}
