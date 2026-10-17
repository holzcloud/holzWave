// The consent card of the chat (docs/design.md section 6.14): a press on `Context…` asks the core once, the pane is shared,
// the screen is previewed into a chip and the message goes out with the offer id and the picked number only. The page sends
// ids and never text, and a mock that plays the core is held to the shell's golden.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  OTHER_PROFILE, PANE_KEY, PANE_TEXT, PROFILE, VIEW3, VIEW_P1, boot, calls, chatBlock, ctxCalls, fillField, openAi, openContext, openDialog,
  sendText, setPrivacy,
} from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const CONSENT = JSON.parse(readFileSync(new URL("../fixtures/ai_consent.json", import.meta.url), "utf8"));

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await contractCase(page, url);
  await idsCase(page, url);
  await askCase(page, url);
  await switchCase(page, url);
  await staleCase(page, url);
  await keysCase(page, url, engine);
  await wordsCase(page, url);
  await localOnlyCase(page, url);
  await reportCase(page, url);
  await noteCase(page, url);
  await narrowCase(page, url);
  await modesCase(page, url, engine);
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

/** Press `Context…`, share pane 1 when asked, tick the screen and preview it; leaves the card in the mode `offer`. */
async function attachScreen(page) {
  await openContext(page);
  if ((await page.locator(".chat-consent").getAttribute("data-mode")) === "ask") await page.locator(".consent-share").click();
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

/** How many calls of the three context commands the page has made. */
const ctxCount = async (page) => (await ctxCalls(page)).length;

/** The text of the status line. */
const status = (page) => page.locator(".chat-status").textContent();

/** Choose a provider in the picker of the chat bar. */
async function choose(page, id) {
  await page.locator(".chat-chip").click();
  await page.locator(`.chat-choice[data-id="${id}"]`).click();
  await page.waitForFunction(() => !document.querySelector(".chat-picker"));
}

/** The focused pane changes to `pane`, as the core announces it. */
async function focusPane(page, pane) {
  await page.evaluate((n) => {
    const v = window.__MOCK__.view();
    v.focus = n;
    v.tabs[0].focus = n;
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  }, pane);
  await page.waitForFunction((n) => document.querySelector(`.panehead[data-pane="${n}"].focused, .panehead[data-pane="${n}"]`) !== null, pane);
}

async function askCase(page, url) {
  await chatBesidePane1(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  await choose(page, "local-1");
  await openContext(page);
  assert.equal(await page.locator(".chat-consent").getAttribute("data-mode"), "ask");
  await page.locator(".consent-no").click();
  assert.equal(await page.locator(".chat-consent").count(), 0);
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-ctx", "the focus returns to the button");
  assert.equal(await page.locator("button.chat-ctx").getAttribute("aria-expanded"), "false");
  assert.equal((await calls(page, "ai_context_share")).length, 0, "No shares nothing");
  assert.equal((await calls(page, "ai_context_state")).length, 1);

  // Share, then stop sharing: the card asks again.
  await openContext(page);
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  await page.locator(".consent-unshare").click();
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  const shares = await calls(page, "ai_context_share");
  assert.deepEqual(shares.map((a) => a.shared), [true, false]);
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.context.shares.length), 0);
  await page.locator(".consent-no").click();

  // `Models on this computer › Do not ask`: a local model skips the question, a cloud model does not.
  await setPrivacy(page, { localContext: "auto" });
  const before = (await calls(page, "ai_context_share")).length;
  await openContext(page);
  assert.equal(await page.locator(".chat-consent").getAttribute("data-mode"), "pick", "straight to the pick list");
  assert.ok((await page.locator(".chat-consent").textContent()).includes("Asking is off for models on this computer."));
  assert.equal((await calls(page, "ai_context_share")).length, before, "no share call");
  assert.equal(await page.locator(".consent-unshare").count(), 0, "nothing is shared, so nothing to stop");
  await page.locator(".consent-close").click();
  await choose(page, "other-1");
  await openContext(page);
  assert.equal(await page.locator(".chat-consent").getAttribute("data-mode"), "ask", "a cloud provider always asks");
  await page.locator(".consent-no").click();

  // The entry for a palette action: the card opens for the pane the chat acts for and previews what it was asked to.
  await choose(page, "local-1");
  await page.evaluate(() => window.__MOCK__.ai.context.shares.push({
    conversation: "chat-mock-1", pane: 1, route: "local-1|llama3.2|http://localhost:11434",
  }));
  const reading = await page.evaluate(async () => {
    const chat = await import(new URL("chat.js", location.href).href);
    return chat.attachContext("chat-mock-1", ["screen"]);
  });
  assert.equal(reading, null);
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  assert.equal(await page.locator('li.consent-item[data-kind="screen"]').count(), 1, "the screen item was read without a tick");
  assert.deepEqual((await calls(page, "ai_context_preview")).map((a) => a.kinds), [["screen"]]);
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();
  // On a pane that is not shared the card asks, and previews once the user shares.
  await page.evaluate(() => (window.__MOCK__.ai.context.shares.length = 0));
  await setPrivacy(page, { localContext: "ask" });
  await page.evaluate(async () => {
    const chat = await import(new URL("chat.js", location.href).href);
    await chat.attachContext("chat-mock-1", ["screen"]);
  });
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  const previews = (await calls(page, "ai_context_preview")).length;
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  assert.equal((await calls(page, "ai_context_preview")).length, previews + 1);
  assert.equal(await page.evaluate(async () => {
    const chat = await import(new URL("chat.js", location.href).href);
    return chat.attachContext("no-such-chat", ["screen"]);
  }), "The chat is not open.");
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();

  // A pane that offers nothing: blocked, with words and `Close`.
  await page.evaluate(() => (window.__MOCK__.ai.context.panes[1].kinds = []));
  await openContext(page);
  assert.equal(await page.locator(".chat-consent").getAttribute("data-mode"), "blocked");
  assert.ok((await page.locator(".chat-consent .consent-text").textContent()).length > 10);
  assert.equal(await page.locator(".consent-close").count(), 1);
  await page.locator(".consent-close").click();
  assert.equal(await page.locator(".chat-consent").count(), 0);
}

async function switchCase(page, url) {
  await chatBesidePane1(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  await choose(page, "local-1");
  await attachScreen(page);
  const calls1 = await ctxCount(page);

  // Another provider in the picker: the card closes, the page says so, and nothing is asked.
  await choose(page, "other-1");
  assert.equal(await page.locator(".chat-consent").count(), 0);
  assert.ok((await status(page)).startsWith("Sharing ended because the provider changed"), await status(page));
  assert.equal(await page.locator(".chat-status").getAttribute("data-kind"), "note");
  assert.equal(await ctxCount(page), calls1, "no context command by the page itself");
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context…");
  await openContext(page);
  const states = await calls(page, "ai_context_state");
  assert.equal(states[states.length - 1].profile, "other-1");
  assert.equal(await page.locator(".chat-consent").getAttribute("data-mode"), "ask");
  await page.locator(".consent-no").click();

  // A model saved in the providers dialog is another route.
  await choose(page, "local-1");
  await attachScreen(page);
  const calls2 = await ctxCount(page);
  await page.locator(".chat-setup").click();
  await openDialog(page);
  await fillField(page, "#aiset-model", "llama3.3");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".aiset-status")?.textContent === "Saved.");
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => !document.querySelector(".dlg.ai-settings"));
  assert.equal(await page.locator(".chat-consent").count(), 0, "the card closed");
  assert.ok((await status(page)).startsWith("Sharing ended because the provider changed"), await status(page));
  assert.equal(await ctxCount(page), calls2);

  // Another pane focused: the card closes and the next send names it.
  await attachScreen(page);
  const calls3 = await ctxCount(page);
  await focusPane(page, 2);
  await page.waitForFunction(() => !document.querySelector(".chat-consent"));
  assert.ok((await status(page)).startsWith("The chat now acts for another pane"), await status(page));
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.context.offers.length), 1, "the core still has its offer; the page dropped it");
  assert.equal(await ctxCount(page), calls3);
  await page.locator(".chat-input").fill("plain");
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const plain = (await calls(page, "ai_send"))[0];
  assert.equal(plain.pane, 2);
  assert.deepEqual(Object.keys(plain), ["conversation", "text", "pane", "profile", "model"]);
  await settled(page, 2);

  // A privacy setting changed with an offer attached.
  await focusPane(page, 1);
  await page.locator(".chat-input").focus();
  await attachScreen(page);
  const calls4 = await ctxCount(page);
  await setPrivacy(page, { localContext: "auto" });
  await page.waitForFunction(() => !document.querySelector(".chat-consent"));
  assert.ok((await status(page)).startsWith("A privacy setting changed"), await status(page));
  assert.equal(await ctxCount(page), calls4);

  // The acting pane closes: the button cannot be used and a press says why.
  await page.evaluate(() => {
    window.__MOCK__.ai.context.closePane(1);
    const v = window.__MOCK__.view();
    v.focus = 3;
    v.tabs[0].focus = 3;
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  });
  await focusPane(page, 3);
  // Pane 2 is still open and was the last terminal focused, so the chat acts for it; close it as well.
  await page.evaluate(() => {
    window.__MOCK__.ai.context.closePane(2);
    window.__MOCK__.emit("layout", window.__MOCK__.view());
  });
  await page.waitForFunction(() => document.querySelector("button.chat-ctx")?.getAttribute("aria-disabled") === "true");
  const calls5 = await ctxCount(page);
  await page.locator("button.chat-ctx").click({ force: true });
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "Focus a terminal or preview pane first.");
  assert.equal(await ctxCount(page), calls5, "nothing was asked");
  assert.equal(await page.locator(".chat-consent").count(), 0);
}

async function staleCase(page, url) {
  await chatBesidePane1(page, url);
  await attachScreen(page);
  await page.evaluate(() => window.__MOCK__.ai.context.expire());
  await sendText(page, "explain");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  await page.waitForFunction(() => document.querySelector(".chat-status")?.dataset.kind === "error");
  assert.equal(await page.locator(".chat-input").inputValue(), "explain", "the draft stays");
  assert.equal(await page.locator(".chat-cards").evaluate((n) => n.children.length), 0, "the card is closed");
  assert.equal(await page.locator(".chat-msg").count(), 0, "no message was added");
  assert.ok((await status(page)).length > 10, await status(page));
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context…");

  // An offer older than its time is not sent: the page checks it before the send.
  await page.evaluate(() => (window.__MOCK__.ai.context.ttl = 1));
  await openContext(page);
  assert.equal(await page.locator(".chat-consent").getAttribute("data-mode"), "pick", "the pane is still shared");
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  await page.waitForTimeout(1200);
  await page.locator(".chat-input").press("Enter");
  assert.equal(await status(page), "The attached context expired. Add it again.");
  assert.equal((await calls(page, "ai_send")).length, 1, "nothing was sent");
  assert.equal(await page.locator(".chat-consent").count(), 0);
  await page.evaluate(() => (window.__MOCK__.ai.context.ttl = 300));

  // The card is busy: two presses on Preview make one call, and a send waits.
  await openContext(page);
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.evaluate(() => window.__MOCK__.ai.context.hold("ai_context_preview"));
  const before = (await calls(page, "ai_context_preview")).length;
  await page.evaluate(() => {
    const button = document.querySelector(".consent-preview");
    button.click();
    button.click();
  });
  assert.equal((await calls(page, "ai_context_preview")).length, before + 1);
  await page.locator(".chat-input").press("Enter");
  assert.equal(await status(page), "Wait for the context card to finish.");
  assert.equal((await calls(page, "ai_send")).length, 1);
  await page.evaluate(() => window.__MOCK__.ai.context.release());
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();

  // A kind that was ticked and never previewed blocks the send.
  await openContext(page);
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".chat-input").press("Enter");
  assert.equal(await status(page), "Preview what you ticked, or untick it, before sending.");
  assert.equal((await calls(page, "ai_send")).length, 1);
  assert.equal(await page.locator(".chat-input").inputValue(), "explain");
  await page.locator(".consent-close").click();

  // Every item unticked sends a plain message.
  await attachScreen(page);
  await page.locator('li.consent-item[data-id="1"] input').uncheck();
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context · none");
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 2);
  assert.deepEqual(Object.keys((await calls(page, "ai_send"))[1]), ["conversation", "text", "pane", "profile", "model"]);
  await settled(page, 2);
  assert.equal(await page.locator(".chat-cards").evaluate((n) => n.children.length), 0, "a plain send closes the card");

  // A send refused for another reason keeps the offer: a reply is already running.
  await page.evaluate(() => window.__MOCK__.ai.script({ stall: true }));
  await sendText(page, "start");
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "stop");
  await attachScreen(page);
  await page.locator(".chat-input").fill("explain");
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => document.querySelector(".chat-status")?.dataset.kind === "error");
  assert.equal(await page.locator(".chat-consent").getAttribute("data-mode"), "offer", "the offer is kept");
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context · 1 item");
  await page.locator(".chat-send").click();
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "send");

  // The same content twice in one chat is refused by the core: the offer is dropped and the draft stays.
  await page.locator(".chat-input").fill("first");
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();
  await attachScreen(page);
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 5);
  await settled(page, 6);
  await attachScreen(page);
  await page.locator(".chat-input").fill("again");
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 6);
  await page.waitForFunction(() => document.querySelector(".chat-status")?.dataset.kind === "error");
  assert.equal(await status(page), "This content was already sent in this chat. Change it, or ask again after it changed.");
  assert.equal(await page.locator(".chat-input").inputValue(), "again");
  assert.equal(await page.locator(".chat-cards").evaluate((n) => n.children.length), 0);
}

async function keysCase(page, url, engine) {
  // WebKit on macOS visits buttons with Option+Tab only, as Safari does by default.
  const tab = () => page.keyboard.press(engine === "webkit" && process.platform === "darwin" ? "Alt+Tab" : "Tab");
  await chatBesidePane1(page, url);
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  assert.equal(await page.evaluate(() => document.activeElement?.className), "consent-share", "the first control has the focus");
  assert.equal(await page.locator(".chat-consent").getAttribute("role"), "group");
  assert.equal(await page.locator(".chat-consent").getAttribute("aria-label"), "Share context");
  await tab();
  assert.equal(await page.evaluate(() => document.activeElement?.className), "consent-no", "Tab visits the controls in order");

  // Every control has `data-field`, and a character typed in the card reaches no terminal.
  const loose = await page.evaluate(() => [...document.querySelectorAll(".chat-consent button, .chat-consent input")].filter((n) => !n.hasAttribute("data-field")).length);
  assert.equal(loose, 0, "every control carries data-field");
  const terminal = () => page.evaluate(() => window.__MOCK__.calls.filter((c) => c[0] === "key" || c[0] === "text").length);
  const typed = await terminal();
  await page.keyboard.type("x");
  assert.equal(await terminal(), typed, "nothing reached the terminal");

  // Share, then pick with the keyboard: `Space` ticks.
  await page.locator(".consent-share").focus();
  await page.keyboard.press("Enter");
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  assert.equal(await page.evaluate(() => document.activeElement?.matches(".consent-kind input")), true, "the focus is on the first checkbox");
  await page.keyboard.press("Space");
  assert.equal(await page.locator(".consent-kind input:checked").count(), 1);
  assert.equal(await page.evaluate(() => document.activeElement?.matches(".consent-kind input")), true, "a tick keeps the focus");
  const typed2 = await terminal();
  await page.keyboard.type("y");
  assert.equal(await terminal(), typed2);

  // `Escape` in the card closes it, returns the focus to the button, and neither stops a reply nor reaches the terminal.
  await page.evaluate(() => window.__MOCK__.ai.script({ stall: true }));
  await page.locator(".chat-input").fill("go");
  await page.locator(".consent-close").focus();
  await page.locator(".chat-input").press("Enter");
  assert.equal(await status(page), "Preview what you ticked, or untick it, before sending.");
  await page.locator('label.consent-kind[data-kind="selection"] input').uncheck();
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "stop");
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector(".chat-consent:not([aria-busy])");
  await page.locator('label.consent-kind[data-kind="screen"] input').focus();
  await page.keyboard.press("Escape");
  assert.equal(await page.locator(".chat-consent").count(), 0, "Escape closed the card");
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-ctx", "the focus is on the button again");
  assert.equal((await calls(page, "ai_cancel")).length, 0, "the reply keeps running");
  assert.equal(await page.locator(".chat-send").getAttribute("data-mode"), "stop");

  // `Escape` in the composer keeps the chat's own behaviour: the running reply stops, the card stays.
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector(".chat-consent:not([aria-busy])");
  await page.locator(".chat-input").focus();
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_cancel").length === 1);
  assert.equal(await page.locator(".chat-consent").count(), 1, "the card stays");
  assert.equal(await page.locator("button.chat-ctx").getAttribute("aria-expanded"), "true");
}

/** The sentences of the page for the core's reasons, as the design record has them. */
const REASON_SENTENCES = {
  not_shared: "This pane is not shared with the chat yet. Share it first.",
  stale_offer: "The attached context is no longer valid. Read it again.",
  bad_pick: "The chosen items do not match what was read. Read the context again.",
  bad_kinds: "That choice of content cannot be read. Tick one to five kinds.",
  unsupported_kind: "This pane cannot offer that kind of content.",
  nothing_to_share: "There is nothing in this pane that can be shared.",
  pane_gone: "The pane is closed or no longer shows what was shared.",
  denied: "This place may not be shared.",
  not_text: "This file is not text, so it cannot be shared.",
  unreadable: "The pane or file could not be read. Try again.",
  busy: "The pane did not answer within two seconds. Try again.",
  destination: "The address of this provider cannot be checked or is not allowed. Nothing was sent.",
  local_only: "Local only is on, and this provider is not on this computer.",
};

async function wordsCase(page, url) {
  assert.deepEqual(Object.keys(REASON_SENTENCES), CONSENT.reasons, "the test holds a sentence for each of the core's reasons");
  await chatBesidePane1(page, url);
  for (const reason of CONSENT.reasons) {
    await page.evaluate(([r]) => window.__MOCK__.ai.context.fail("ai_context_state", r, "words of the core"), [reason]);
    await page.locator("button.chat-ctx").click();
    await page.waitForSelector('.chat-consent[data-mode="blocked"]');
    assert.equal(await page.locator('.chat-consent p.consent-text[data-kind="error"]').textContent(), REASON_SENTENCES[reason], reason);
    const providers = await page.locator(".consent-providers").count();
    assert.equal(providers, reason === "local_only" || reason === "destination" ? 1 : 0, `${reason}: the button to the providers`);
    await page.locator(".consent-close").click();
  }
  // A reason the table does not know is shown in the core's own text.
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_state", "from_a_newer_core", "the core's own words"));
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector('.chat-consent[data-mode="blocked"]');
  assert.equal(await page.locator(".chat-consent p.consent-text").textContent(), "the core's own words");
  await page.locator(".consent-close").click();
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_state", null));

  // A refused read leaves the pick list with the words.
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_preview", "busy", "core words"));
  await attachScreen(page).catch(() => {});
  await page.waitForSelector('.chat-consent[data-mode="pick"] p.consent-text[data-kind="error"]');
  assert.equal(await page.locator("p.consent-text[data-kind=\"error\"]").textContent(), REASON_SENTENCES.busy);
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_preview", null));
  await page.locator(".consent-close").click();

  // `local_only`: `Providers…` opens the dialog and the keyboard comes back to the composer.
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_state", "local_only"));
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector(".consent-providers");
  await page.locator(".consent-providers").click();
  await openDialog(page);
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => !document.querySelector(".dlg.ai-settings"));
  assert.equal(await page.evaluate(() => document.activeElement?.matches("textarea.chat-input")), true, "the composer has the keyboard");
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_state", null));
  await page.locator(".consent-close").click();

  // The place is the core's word: the same profile name on a cloud locality reads cloud, an unknown one reads unknown place.
  for (const [locality, place, leaves, host] of [
    ["cloud", "cloud", "true", "localhost:11434"],
    ["loopback", "this computer", "false", "localhost:11434"],
    ["lan", "local network", "true", "localhost:11434"],
    ["on_device", "on device", "false", "localhost:11434"],
    ["mars", "unknown place", "false", "localhost:11434"],
  ]) {
    await chatBesidePane1(page, url, { profiles: [{ ...PROFILE, locality }] });
    await page.locator("button.chat-ctx").click();
    await page.waitForSelector('.chat-consent[data-mode="ask"]');
    assert.equal(await page.locator(".consent-place").textContent(), place, locality);
    assert.equal(await page.locator(".consent-place").getAttribute("data-leaves"), leaves, locality);
    assert.equal(await page.locator(".consent-host").textContent(), host);
    assert.ok((await page.locator(".chat-consent").textContent()).includes(`(${place})`), "the question names the place");
  }
  // A destination that leaves the computer is in the warn colour, and the word is underlined: a second cue.
  await chatBesidePane1(page, url, { profiles: [{ ...PROFILE, locality: "cloud" }] });
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector(".consent-place");
  const warn = await page.evaluate(() => {
    const chip = getComputedStyle(document.querySelector(".consent-place"));
    const probe = document.createElement("i");
    probe.style.color = "var(--hw-warn)";
    document.body.append(probe);
    const want = getComputedStyle(probe).color;
    probe.remove();
    return { color: chip.color, want, line: chip.textDecorationLine };
  });
  assert.equal(warn.color, warn.want);
  assert.ok(warn.line.includes("underline"));
}

async function localOnlyCase(page, url) {
  await chatBesidePane1(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  await choose(page, "local-1");
  assert.equal(await page.locator(".chat-local").isVisible(), false, "not drawn while the switch is off");
  await setPrivacy(page, { localOnly: true });
  await page.waitForSelector(".chat-bar button.chat-local");
  assert.equal(await page.locator("button.chat-local").textContent(), "local only");
  const label = await page.locator("button.chat-local").getAttribute("aria-label");
  assert.ok(label.includes("refused") && label.includes("provider settings"), label);
  await page.locator("button.chat-local").click();
  await openDialog(page);
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => !document.querySelector(".dlg.ai-settings"));
  assert.equal(await page.evaluate(() => document.activeElement?.matches("textarea.chat-input")), true, "the composer has the keyboard");

  // With the switch on, a destination that leaves the computer cannot be shared.
  await choose(page, "other-1");
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector('.chat-consent[data-mode="blocked"]');
  assert.equal(await page.locator("p.consent-text").textContent(), REASON_SENTENCES.local_only);
  assert.equal(await page.locator(".consent-providers").count(), 1);
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.context.offers.length), 0, "no offer");
  await page.locator(".consent-close").click();

  await setPrivacy(page, { localOnly: false });
  await page.waitForFunction(() => document.querySelector(".chat-local")?.hidden === true);
  assert.equal(await page.locator(".chat-local").isVisible(), false, "gone without a reload");

  // A core that cannot answer the settings: nothing is drawn from a guess.
  await chatBesidePane1(page, url, { settings: { unavailable: true } });
  await page.evaluate(() => (window.__MOCK__.ai.context.localOnly = true));
  assert.equal(await page.locator(".chat-local").isVisible(), false);
}

/** Open the card, share when asked, tick `kinds` and preview them; leaves the card in the mode `offer`. */
async function attachKinds(page, kinds) {
  await openContext(page);
  if ((await page.locator(".chat-consent").getAttribute("data-mode")) === "ask") await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  for (const kind of kinds) await page.locator(`label.consent-kind[data-kind="${kind}"] input`).check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
}

async function reportCase(page, url) {
  const findings = [{ rule: "hidden_text", count: 15 }, { rule: "anthropic_key", count: 1 }];
  const screen = { label: "Screen of pane 1", bytes: 2457, findings, text: "t" };
  await chatBesidePane1(page, url, { context: { panes: { 1: { kinds: ["selection", "screen", "last_command"], items: { screen } } } } });
  await attachKinds(page, ["selection", "screen"]);
  const list = page.locator("li.consent-finding");
  assert.deepEqual(await list.allTextContents(), ["hidden text removed (15 characters)", "Anthropic key hidden"]);
  assert.equal(await list.nth(0).getAttribute("data-tone"), "warn");
  assert.equal(await list.nth(1).getAttribute("data-tone"), "plain");
  const mark = await list.nth(0).evaluate((n) => ({ line: getComputedStyle(n).textDecorationLine, before: getComputedStyle(n, "::before").content }));
  assert.ok(mark.line.includes("underline"), "underlined");
  assert.equal(mark.before, '"! "', "a ! mark");
  assert.equal(await page.locator(".consent-total").textContent(), "1 item · 2.4 KiB · ≈ 819 tokens");
  assert.equal(await page.locator(".consent-persist").textContent(), "This chat keeps a note of what was sent (labels and sizes), not the text.");
  assert.equal(await page.locator(".consent-valid").textContent(), "Valid for 5 minutes.");
  assert.ok((await page.locator(".consent-best").textContent()).includes("Best effort, not a guarantee"));
  assert.equal(await page.locator("p.consent-empty").textContent(), "Nothing there: Selection.");
  assert.equal(await page.locator(".consent-dest .consent-host").textContent(), "localhost:11434");
  // The page keeps no text of the item: only labels, sizes and findings are on the card.
  assert.equal((await page.locator(".chat-consent").textContent()).includes("zq-consent"), false);

  // The persistence line follows the core, both ways.
  await page.evaluate(() => (window.__MOCK__.ai.context.persist = true));
  await page.locator(".consent-again").click();
  await page.waitForFunction(() => document.querySelector(".consent-persist")?.textContent.startsWith("This chat keeps the text"));
  assert.equal(await page.locator(".consent-persist").textContent(), "This chat keeps the text that was sent, on this computer.");

  // Unticking an item updates the total and the button.
  await page.locator('li.consent-item[data-id="1"] input').uncheck();
  assert.equal(await page.locator(".consent-total").textContent(), "0 items · 0 B");
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context · none");
  await page.locator('li.consent-item[data-id="1"] input').check();
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context · 1 item");
  await page.locator(".consent-discard").click();

  // The last command is offered only when the core lists it.
  assert.equal(await page.locator('label.consent-kind[data-kind="last_command"]').textContent(), "Last command and its output");
  assert.ok((await page.locator(".consent-note").textContent()).includes("start-up text"), "a hint about a fresh pane");
  await page.locator(".consent-close").click();
  await page.evaluate(() => (window.__MOCK__.ai.context.panes[1].kinds = ["selection", "screen"]));
  await openContext(page);
  assert.equal(await page.locator('label.consent-kind[data-kind="last_command"]').count(), 0, "the card offers what the core lists");
  assert.equal(await page.locator(".consent-note").count(), 0);
  await page.locator(".consent-close").click();

  // A label that holds markup is a text node, and a long one is cut.
  const hostile = "zq-label-<img src=x onerror=alert(1)>-8c3f";
  await page.evaluate((label) => (window.__MOCK__.ai.context.panes[1].items.screen.label = label), hostile);
  await attachKinds(page, ["screen"]);
  assert.equal(await page.locator(".consent-label").textContent(), hostile);
  assert.equal(await page.locator(".chat-consent img, .chat-consent [onerror]").count(), 0);
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();
  await page.evaluate(() => (window.__MOCK__.ai.context.panes[1].items.screen.label = "y".repeat(400)));
  await attachKinds(page, ["screen"]);
  const long = await page.locator(".consent-label").textContent();
  assert.equal(Array.from(long).length, 200);
  assert.ok(long.endsWith("…"));
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();

  // Eight items draw eight rows; nine are not an offer the page can read.
  const many = (n) => Array.from({ length: n }, (_, i) => ({ label: `File ${i + 1}`, bytes: 100 * (i + 1), findings: [], text: "t" }));
  await page.evaluate((items) => {
    const pane = window.__MOCK__.ai.context.panes[1];
    pane.kinds = ["selection", "screen", "file"];
    pane.items.file = items;
  }, many(8));
  await attachKinds(page, ["file"]);
  assert.equal(await page.locator("li.consent-item").count(), 8);
  assert.equal(await page.locator(".consent-total").textContent(), "8 items · 3.5 KiB · ≈ 1.2k tokens");
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();
  await page.evaluate((items) => (window.__MOCK__.ai.context.panes[1].items.file = items), many(9));
  await openContext(page);
  await page.locator('label.consent-kind[data-kind="file"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="blocked"]');
  assert.equal(await page.locator("p.consent-text").textContent(), "The answer was not understood.");
  assert.equal(await page.locator("li.consent-item").count(), 0, "no part of the offer is shown");
  assert.equal(await page.locator("button.chat-ctx").textContent(), "Context…");
}

async function noteCase(page, url) {
  // A send with two items: the labels and sizes are joined with `; ` and the place is the core's.
  await chatBesidePane1(page, url);
  await attachKinds(page, ["screen", "last_command"]);
  await sendText(page, "explain");
  await page.waitForSelector('.chat-msg[data-role="user"] p.chat-ctxnote');
  assert.equal(
    await page.locator("p.chat-ctxnote").textContent(),
    "Context sent: Screen of pane 1 (2.4 KiB); Last command output of pane 1 (918 B) → this computer",
  );
  await settled(page, 2);
  assert.equal(await page.locator('.chat-msg[data-role="assistant"] p.chat-ctxnote').count(), 0);
  const noted = await page.evaluate(() => window.__MOCK__.ai.chat("chat-mock-1").messages[0].parts.find((p) => p.type === "context").hash);
  assert.equal((await page.locator("p.chat-ctxnote").textContent()).includes(noted), false, "no hash on the line");

  // The place is the one the part carries (the page's reader refuses a part with a place the core does not have).
  await chatBesidePane1(page, url, { profiles: [{ ...PROFILE, locality: "cloud" }] });
  await attachKinds(page, ["screen"]);
  await sendText(page, "explain");
  await page.waitForSelector("p.chat-ctxnote");
  assert.equal(await page.locator("p.chat-ctxnote").textContent(), "Context sent: Screen of pane 1 (2.4 KiB) → cloud");

  // After a reload the snapshot brings the same message and the same line; a message without context has none.
  const part = {
    type: "context", hash: "ab".repeat(32), provider: "local-1", locality: "lan",
    items: [{ kind: "screen", label: "Screen of pane 1", bytes: 2457 }],
  };
  const user = (id, parts) => ({ id, role: "user", ts: 1800000000, model: null, status: "complete", usage: null, parts });
  const text = (value) => ({ type: "text", text: value });
  await boot(page, url, {
    view: VIEW3,
    blocks: [chatBlock("chat-x")],
    states: {
      "chat-x": {
        title: "Earlier", profile: "local-1", model: "llama3.2", seq: 2, turn: 1800000001,
        messages: [
          user("m1-user", [part, text("with context")]),
          { id: "m1-1", role: "assistant", ts: 1800000000, model: "llama3.2", status: "complete", usage: null, parts: [text("a reply")] },
          user("m2-user", [text("without context")]),
        ],
      },
    },
  });
  await page.waitForSelector('.block[data-pane="3"] .chat-msg[data-id="m1-user"]');
  await page.waitForSelector('.chat-msg[data-id="m2-user"]');
  const notes = await page.locator("p.chat-ctxnote").allTextContents();
  assert.deepEqual(notes, ["Context sent: Screen of pane 1 (2.4 KiB) → local network"]);
  assert.equal(await page.locator('.chat-msg[data-id="m2-user"] p.chat-ctxnote').count(), 0);
  assert.equal(await page.locator('.chat-msg[data-id="m1-1"] p.chat-ctxnote').count(), 0);
  assert.equal((notes[0]).includes("ab".repeat(8)), false, "no hash");
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

/** A chat that fills the window below a terminal strip; `size` is the window. */
async function openSized(page, url, size, extra = {}) {
  await page.setViewportSize(size);
  await boot(page, url, {
    view: VIEW_STACK,
    blocks: [chatBlock("chat-x")],
    states: { "chat-x": { title: "Earlier question", profile: "local-1", model: "llama3.2" } },
    starts: 2,
    ...extra,
  });
  await page.waitForSelector(".block-chat");
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

/** The heights of the controls of the card and of the composer. */
const targets = (page) =>
  page.evaluate(() => {
    const heights = (sel) => [...document.querySelectorAll(sel)].map((n) => n.getBoundingClientRect().height);
    const strip = document.querySelector(".chat-cards");
    const block = document.querySelector(".block-chat");
    const input = document.querySelector(".chat-input").getBoundingClientRect();
    return {
      ctx: heights("button.chat-ctx"),
      buttons: heights(".chat-consent button"),
      kinds: heights("label.consent-kind"),
      items: heights("li.consent-item"),
      strip: strip.getBoundingClientRect().height,
      block: block.getBoundingClientRect().height,
      scrolls: getComputedStyle(strip).overflowY,
      inside: input.bottom <= innerHeight && input.top >= 0,
      font: parseFloat(getComputedStyle(document.querySelector(".chat-input")).fontSize),
      view: innerHeight,
    };
  });

/** Walk the card through its four modes and measure each. */
async function everyMode(page, check) {
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_state", "denied"));
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector('.chat-consent[data-mode="blocked"]');
  await check("blocked", await targets(page));
  await page.locator(".consent-close").click();
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_state", null));
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  await check("ask", await targets(page));
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  await check("pick", await targets(page));
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  await check("offer", await targets(page));
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();
}

async function narrowCase(page, url) {
  const findings = [{ rule: "hidden_text", count: 15 }, { rule: "anthropic_key", count: 1 }];
  const screen = { label: "Screen of pane 1", bytes: 2457, findings, text: "t" };
  const context = { panes: { 1: { kinds: ["selection", "screen", "last_command"], items: { screen } } } };

  // A phone: every target is at least 44 px, the card is at most 60 % of the window and scrolls, the composer stays in view.
  await openSized(page, url, { width: 390, height: 780 }, { context });
  await everyMode(page, (mode, t) => {
    for (const [name, list] of [["ctx", t.ctx], ["buttons", t.buttons], ["kinds", t.kinds], ["items", t.items]]) {
      for (const h of list) assert.ok(h >= 44 - 0.5, `${mode}: ${name} is ${h} px high at 390 px`);
    }
    assert.ok(t.strip <= t.view * 0.6 + 1, `${mode}: the card is ${t.strip} of ${t.view}`);
    assert.equal(t.scrolls, "auto", `${mode}: the card scrolls inside`);
    assert.equal(t.inside, true, `${mode}: the composer is in the window`);
    assert.ok(t.font >= 16, `${mode}: composer text is ${t.font} px`);
  });
  // A tall offer scrolls inside the card, and the composer is still on the screen.
  const many = Array.from({ length: 8 }, (_, i) => ({ label: `File ${i + 1}`, bytes: 100, findings, text: "t" }));
  await page.evaluate((items) => {
    const pane = window.__MOCK__.ai.context.panes[1];
    pane.kinds = ["file"];
    pane.items.file = items;
  }, many);
  await attachKinds(page, ["file"]);
  const tall = await page.locator(".chat-cards").evaluate((n) => ({ scroll: n.scrollHeight, client: n.clientHeight }));
  assert.ok(tall.scroll > tall.client, `the card scrolls (${tall.scroll} in ${tall.client})`);
  assert.equal((await targets(page)).inside, true);
  await page.locator(".consent-discard").click();
  await page.locator(".consent-close").click();

  // A desktop window: 28 px targets that keep the desktop size, and the card is at most half the pane.
  await openSized(page, url, { width: 1000, height: 640 }, { context });
  await everyMode(page, (mode, t) => {
    for (const [name, list] of [["ctx", t.ctx], ["buttons", t.buttons], ["kinds", t.kinds], ["items", t.items]]) {
      for (const h of list) {
        assert.ok(h >= 28 - 0.5, `${mode}: ${name} is ${h} px high on the desktop`);
        // A row of items also holds its findings, so only the controls keep the desktop size.
        if (name !== "items") assert.ok(h < 44, `${mode}: ${name} keeps the desktop size (${h})`);
      }
    }
    assert.ok(t.strip <= t.block * 0.5 + 1, `${mode}: the card is ${t.strip} of the pane's ${t.block}`);
  });
  await page.setViewportSize({ width: 1000, height: 640 });
}

async function modesCase(page, url, engine) {
  // WebKit on macOS visits buttons with Option+Tab only, as Safari does by default.
  const back = () => page.keyboard.press(engine === "webkit" && process.platform === "darwin" ? "Shift+Alt+Tab" : "Shift+Tab");
  await openSized(page, url, { width: 1000, height: 640 });
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  const root = (add, name) => page.evaluate(([a, n]) => document.documentElement.classList[a ? "add" : "remove"](n), [add, name]);
  const edge = () => page.locator(".chat-consent").evaluate((n) => getComputedStyle(n).borderTopColor);
  const duration = (sel) => page.locator(sel).evaluate((n) => getComputedStyle(n).transitionDuration);

  // Higher contrast: the hairline follows `--hw-line`, which that mode swaps.
  const normal = await edge();
  await root(true, "contrast-high");
  assert.notEqual(await edge(), normal, "the card's hairline follows the token");
  await root(false, "contrast-high");
  assert.equal(await edge(), normal);

  // The card has no motion, with and without the setting.
  for (const reduced of [false, true]) {
    await root(reduced, "motion-reduce");
    for (const sel of ["button.chat-ctx", ".consent-share", ".consent-no"]) assert.equal(await duration(sel), "0s", `${sel} reduced ${reduced}`);
  }
  await root(false, "motion-reduce");
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  assert.equal(await duration(".consent-preview"), "0s");

  // Forced colours: the style sheet outlines the card, the place chip and the local-only chip with a system colour.
  const css = await (await fetch(`${url}consent.css`)).text();
  const forced = css.slice(css.indexOf("@media (forced-colors: active)"));
  assert.ok(forced.startsWith("@media"), "consent.css has a forced-colors block");
  assert.match(forced, /\.chat-consent[^{]*\{[^}]*outline[^}]*(CanvasText|Highlight)/);
  assert.match(forced, /\.consent-place[^{]*\{[^}]*outline[^}]*Highlight/);
  assert.match(forced, /\.chat-local[^{]*\{[^}]*outline[^}]*Highlight/);

  // The keyboard ring is 2 px on a button, and a checkbox uses the rose token for its ring.
  const rose = await page.evaluate(() => {
    const probe = document.createElement("i");
    probe.style.color = "var(--hw-rose)";
    document.body.append(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  });
  await page.locator(".chat-input").focus();
  await back();
  assert.equal(await page.evaluate(() => document.activeElement?.className), "consent-close", "the keyboard walks back into the card");
  assert.equal(await page.locator(".consent-close").evaluate((n) => getComputedStyle(n).outlineWidth), "2px", "a button: a 2 px ring");
  await back();
  assert.equal(await page.evaluate(() => document.activeElement?.className), "consent-unshare");
  await back();
  assert.equal(await page.evaluate(() => document.activeElement?.matches(".consent-kind input")), true, "then the last checkbox");
  const ring = await page.evaluate(() => ({ w: getComputedStyle(document.activeElement).outlineWidth, c: getComputedStyle(document.activeElement).outlineColor }));
  assert.equal(ring.w, "2px");
  assert.equal(ring.c, rose, "a checkbox: the rose ring");

  // Nothing in the card or on its button has a gradient, a filter or a shadow.
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  const flat = await page.evaluate(() =>
    [...document.querySelectorAll(".chat-cards, .chat-cards *, .chat-ctx, .chat-ctxnote")]
      .filter((n) => {
        const css = getComputedStyle(n);
        return css.backgroundImage !== "none" || css.filter !== "none" || css.boxShadow !== "none";
      })
      .map((n) => n.className),
  );
  assert.deepEqual(flat, []);
}
