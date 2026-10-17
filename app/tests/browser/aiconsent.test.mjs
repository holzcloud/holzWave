// The consent card of the chat (docs/design.md section 6.14): a press on `Context…` asks the core once, the pane is shared,
// the screen is previewed into a chip and the message goes out with the offer id and the picked number only. The page sends
// ids and never text, and a mock that plays the core is held to the shell's golden.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  OTHER_PROFILE, PANE_KEY, PANE_TEXT, PROFILE, VIEW_P1, boot, calls, ctxCalls, fillField, openAi, openContext, openDialog, sendText,
  setPrivacy, watchStatus,
} from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await contractCase(page, url);
  await idsCase(page, url);
  await askCase(page, url);
  await switchCase(page, url);
  await staleCase(page, url);
  await keysCase(page, url, engine);
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
