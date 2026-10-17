// The consent card of the chat (docs/design.md section 6.14): a press on `Context…` asks the core once, the pane is shared,
// the screen is previewed into a chip and the message goes out with the offer id and the picked number only. The page sends
// ids and never text, and a mock that plays the core is held to the shell's golden.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { OTHER_PROFILE, PANE_KEY, PANE_TEXT, PROFILE, VIEW_P1, boot, calls, ctxCalls, openAi, openContext, sendText } from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await contractCase(page, url);
  await idsCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with one local profile, the settings core, two terminals and a chat opened beside pane 1 (the chat is pane 3). */
async function chatBesidePane1(page, url, extra = {}) {
  await boot(page, url, { view: VIEW_P1, settings: true, ...extra });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  // Both ends of the focus move have been drawn: the page now knows the pane the chat acts for.
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

/** Press `Context…`, share pane 1, tick the screen and preview it; leaves the card in the mode `offer`. */
async function attachScreen(page) {
  await openContext(page);
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
}

/** Wait for the reply of the turn that was sent to end. */
async function settled(page, messages) {
  await page.waitForFunction(
    (n) => document.querySelectorAll(".chat-msg").length === n && !document.querySelector(".chat-partial"),
    messages,
  );
}

async function traceCase(page, url) {
  await chatBesidePane1(page, url);
  // Until the first press nothing is asked: not at mount, not at the focus of the composer, not at a redraw.
  await page.locator(".chat-input").fill("draft");
  await page.locator(".chat-input").fill("");
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  assert.deepEqual(await ctxCalls(page), [], "no context command before the first press");
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context…");
  assert.equal(await page.locator("button.chat-ctx").getAttribute("aria-expanded"), "false");

  await page.locator("button.chat-ctx").click();
  await page.waitForSelector('.chat-cards .chat-consent[data-mode="ask"]');
  const states = await calls(page, "ai_context_state");
  assert.equal(states.length, 1);
  assert.deepEqual(states[0], { conversation: "chat-mock-1", pane: 1, profile: "local-1", model: null });
  assert.deepEqual(Object.keys(states[0]), ["conversation", "pane", "profile", "model"]);
  // The strip sits between the transcript and the status line.
  const order = await page.evaluate(() =>
    [...document.querySelector(".block-chat").children].map((c) => c.className.split(" ")[0]));
  assert.ok(order.indexOf("chat-cards") === order.indexOf("chat-log") + 1, `the strip follows the log: ${order}`);
  assert.ok(order.indexOf("chat-status") === order.indexOf("chat-cards") + 1, `the status line follows the strip: ${order}`);
  const text = await page.locator(".chat-consent").textContent();
  assert.ok(text.includes("Consent needed"), text);
  assert.ok(text.includes("Share pane 1"), text);
  assert.equal(await page.evaluate(() => document.activeElement?.className), "consent-share", "the focus is on the first control");
  assert.equal(await page.locator("button.chat-ctx").getAttribute("aria-expanded"), "true");

  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  const shares = await calls(page, "ai_context_share");
  assert.equal(shares.length, 1);
  assert.deepEqual(shares[0], { conversation: "chat-mock-1", pane: 1, profile: "local-1", model: null, shared: true });
  const words = await page.locator("label.consent-kind").allTextContents();
  assert.ok(words.includes("Selection") && words.includes("Visible screen"), words.join());
  assert.equal(await page.locator(".consent-kind input:checked").count(), 0, "none is ticked");
  assert.equal(await page.locator(".consent-preview").isDisabled(), true);
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  assert.equal(await page.locator(".consent-preview").isDisabled(), false);

  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  const previews = await calls(page, "ai_context_preview");
  assert.equal(previews.length, 1);
  assert.deepEqual(previews[0], { conversation: "chat-mock-1", pane: 1, profile: "local-1", model: null, kinds: ["screen"] });
  assert.equal(await page.locator("li.consent-item").count(), 1);
  const item = page.locator('li.consent-item[data-id="1"]');
  assert.equal(await item.locator(".consent-label").textContent(), "Screen of pane 1");
  assert.equal(await item.locator(".consent-size").textContent(), "2.4 KiB");
  assert.equal(await item.locator("input").isChecked(), true);
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context · 1 item");

  await sendText(page, "explain");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent), ["conversation", "text", "pane", "profile", "model", "offer", "picked"]);
  assert.equal(sent.conversation, "chat-mock-1");
  assert.equal(sent.text, "explain");
  assert.equal(sent.pane, 1, "the pane the chat acts for, not the chat's own pane 3");
  assert.equal(sent.profile, "local-1");
  assert.match(sent.offer, /^[0-9a-f]{32}$/);
  assert.deepEqual(sent.picked, [1]);

  await page.waitForSelector('.chat-msg[data-role="user"] p.chat-ctxnote');
  const note = await page.locator("p.chat-ctxnote").textContent();
  assert.ok(note.startsWith("Context sent"), note);
  assert.ok(note.includes("Screen of pane 1"), note);
  assert.equal(await page.locator(".chat-cards .chat-consent").count(), 0, "the card is gone");
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context…");
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.context.offers.length), 0, "the offer is spent");
  await settled(page, 2);

  // A message after that goes out plain: the five keys only.
  await sendText(page, "again");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 2);
  const plain = (await calls(page, "ai_send"))[1];
  assert.deepEqual(Object.keys(plain), ["conversation", "text", "pane", "profile", "model"]);
  assert.equal(plain.pane, 1);
  assert.equal(await page.locator("p.chat-ctxnote").count(), 1, "only the first message carried context");
}

/** The sorted member names of an object. */
const names = (o) => Object.keys(o).sort();

async function contractCase(page, url) {
  const findings = [{ rule: "anthropic_key", count: 1 }];
  await chatBesidePane1(page, url, {
    context: {
      panes: {
        1: {
          kinds: ["selection", "screen", "last_command"],
          items: { screen: { label: "Screen of pane 1", bytes: 2457, findings, text: "t" } },
        },
      },
    },
  });
  await attachScreen(page);
  await page.locator(".consent-again").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_context_preview").length === 2);
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  await page.locator(".consent-discard").click();
  await page.locator(".consent-unshare").click();
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  await page.locator(".consent-no").click();

  const answers = await page.evaluate(() => structuredClone(window.__MOCK__.ai.context.answers));
  assert.ok(answers.length >= 4, "state, share and two previews were answered");
  const goldenState = GOLDEN.answers.ai.context_state;
  const goldenOffer = GOLDEN.answers.ai.offer;
  for (const [command, answer] of answers) {
    if (command === "ai_context_preview") {
      assert.deepEqual(names(answer), names(goldenOffer), command);
      for (const item of answer.items) {
        assert.deepEqual(names(item), names(goldenOffer.items[0]), "an item");
        for (const finding of item.findings) assert.deepEqual(names(finding), names(goldenOffer.items[0].findings[0]));
      }
    } else {
      assert.deepEqual(names(answer), names(goldenState), command);
      assert.ok(answer.kinds.every((kind) => GOLDEN.ops.context_kind.includes(kind)), answer.kinds.join());
    }
  }
  assert.ok(answers.some(([, a]) => Array.isArray(a.items) && a.items.some((i) => i.findings.length > 0)), "a finding was answered");

  // The page reads each answer without losing or adding a member.
  const read = await page.evaluate(async (list) => {
    const m = await import(new URL("consentstate.js", location.href).href);
    return list.map(([command, answer]) => (command === "ai_context_preview" ? m.readOffer(answer) : m.readContextState(answer)));
  }, answers);
  read.forEach((got, i) => assert.deepEqual(got, answers[i][1], `answer ${i} is read as it was sent`));

  // The arguments of the calls are the key lists of the golden, in its order.
  const keysOf = async (cmd) => (await calls(page, cmd)).map(Object.keys);
  for (const keys of await keysOf("ai_context_state")) assert.deepEqual(keys, GOLDEN.commands.ai_context_state);
  for (const keys of await keysOf("ai_context_share")) assert.deepEqual(keys, GOLDEN.commands.ai_context_share);
  for (const keys of await keysOf("ai_context_preview")) assert.deepEqual(keys, GOLDEN.commands.ai_context_preview);

  // And a send with an offer has the golden's list of `ai_send`.
  await attachScreen(page);
  await sendText(page, "explain");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.deepEqual(Object.keys((await calls(page, "ai_send"))[0]), GOLDEN.commands.ai_send);
}

async function idsCase(page, url) {
  await chatBesidePane1(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  // The canaries live only in the mock's source table.
  assert.equal(await page.evaluate(() => JSON.stringify(window.__MOCK__.ai.context.panes[1]).includes("zq-consent-text-71b9e4")), true);
  await page.locator(".chat-chip").click();
  await page.locator('.chat-choice[data-id="local-1"]').click();
  await attachScreen(page);
  const id = await page.evaluate(() => window.__MOCK__.ai.context.offers[0].id);
  await sendText(page, "explain");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  await settled(page, 2);

  const calls_ = await page.evaluate(() => JSON.stringify(window.__MOCK__.calls));
  const everywhere = await page.evaluate(() => {
    const attrs = [];
    for (const n of document.querySelectorAll("*")) for (const a of n.attributes) attrs.push(a.value);
    const texts = [];
    const walker = document.createTreeWalker(document.documentElement, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) texts.push(n.data);
    const store = [];
    for (const area of [localStorage, sessionStorage]) for (let i = 0; i < area.length; i++) store.push(area.key(i), area.getItem(area.key(i)));
    return JSON.stringify({ html: document.documentElement.outerHTML, attrs, texts, store, cookie: document.cookie });
  });
  for (const canary of [PANE_TEXT, PANE_KEY, "zqconsent5e90b3a"]) {
    assert.ok(!calls_.includes(canary), `${canary} is in a call`);
    assert.ok(!everywhere.includes(canary), `${canary} is on the page`);
  }

  // The three context commands name ids, a flag and kinds, nothing else.
  const allowed = ["conversation", "pane", "profile", "model", "shared", "kinds"];
  const all = await page.evaluate(() => window.__MOCK__.calls.map((c) => [c[0], structuredClone(c[1])]));
  for (const [command, args] of all) {
    if (command.startsWith("ai_context_")) {
      for (const key of Object.keys(args)) assert.ok(allowed.includes(key), `${command} has the argument ${key}`);
    } else if (command !== "ai_send") {
      for (const key of ["text", "content", "context", "body", "envelope"]) {
        assert.ok(!(args && typeof args === "object" && key in args && command.startsWith("ai_")), `${command} has the argument ${key}`);
      }
    }
  }
  // The offer id occurs in the arguments only as the value of `offer`.
  const holders = [];
  const find = (value, path) => {
    if (value === id) holders.push(path);
    else if (Array.isArray(value)) value.forEach((v, i) => find(v, `${path}[${i}]`));
    else if (value && typeof value === "object") for (const [k, v] of Object.entries(value)) find(v, `${path}.${k}`);
  };
  all.forEach(([command, args], i) => find(args, `${i}:${command}`));
  assert.deepEqual(holders.map((h) => h.replace(/^\d+:/, "")), ["ai_send.offer"]);
  // Only `ai_send` carries text.
  for (const [command, args] of all) {
    if (command !== "ai_send" && command !== "markdown_render") assert.ok(!(args && "text" in args), `${command} has a text argument`);
  }
}
