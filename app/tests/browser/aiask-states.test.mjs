// The states of the card of a `wsh ai` request (docs/design.md section 6.20): the oldest request is shown and the others are counted,
// a reload brings the cards back, every end of a request leaves its sentence, a refusal of the core is told in its words, and the
// mock that plays the core has the member names of the core's golden.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ASK, LOCAL_ASK, askCalls, askCard, askView, boot, closeAsk, emitAsk, tickClock } from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/ai_ask.json", import.meta.url), "utf8"));
const PROTOCOL = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const SECOND = "f0e1d2c3b4a5968778695a4b3c2d1e0f";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await queueCase(page, url);
  await reloadCase(page, url);
  await closedCase(page, url);
  await faultCase(page, url);
  await contractCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const start = (page, url, extra = {}) => boot(page, url, { clock: true, ...extra });
const gone = (page) => page.waitForFunction(() => document.querySelector("section.ask") === null);
const said = (page, selector) => askCard(page).locator(selector).textContent();
const notice = (page) => page.locator("#status .seg.notice").textContent();
const listings = (page) => page.evaluate(() => window.__MOCK__.callsOf("ai_ask_pending").length);

async function show(page, view = ASK) {
  await emitAsk(page, view);
  await askCard(page).waitFor();
}

async function queueCase(page, url) {
  await start(page, url);
  await show(page);
  await emitAsk(page, askView({ ask: SECOND, pane: 1, prompt: "the second one" }));
  await page.waitForFunction(() => document.querySelector(".ask-count") !== null);
  assert.equal(await said(page, ".ask-count"), "1 of 2");
  assert.equal(await said(page, ".ask-text"), "why does this fail", "the oldest is shown");

  // Closing the first shows the second with no count and a window of its own.
  await tickClock(page, 2000);
  await closeAsk(page, ASK.ask, "sent");
  await page.waitForFunction(() => document.querySelector(".ask-text")?.textContent === "the second one");
  assert.equal(await askCard(page).locator(".ask-count").count(), 0, "one left, no count");
  assert.equal(await said(page, ".ask-who"), "wsh ai · pane 1");
  await tickClock(page, 300);
  await askCard(page).locator(".ask-yes").click();
  await page.waitForTimeout(50);
  assert.deepEqual(await askCalls(page), [], "the second card was shown 300 ms ago: Send once is ignored");
  await tickClock(page, 300);
  await askCard(page).locator(".ask-yes").click();
  await gone(page);
  assert.deepEqual(await askCalls(page), [{ ask: SECOND, verdict: "send", seal: ASK.seal }]);

  // A fifth request and a repeated one are ignored.
  for (let n = 1; n <= 5; n += 1) await emitAsk(page, askView({ ask: String(n).repeat(32) }));
  await emitAsk(page, askView({ ask: "1".repeat(32), prompt: "a repeat" }));
  await page.waitForFunction(() => document.querySelector(".ask-count")?.textContent === "1 of 4");
  assert.equal(await said(page, ".ask-text"), "why does this fail", "a repeat does not replace the card");
  await page.waitForTimeout(50);
  assert.equal(await said(page, ".ask-count"), "1 of 4", "four at most");
}

async function reloadCase(page, url) {
  await start(page, url, { ask: { pending: [ASK] } });
  await askCard(page).waitFor();
  assert.equal(await said(page, ".ask-text"), "why does this fail");
  assert.equal(await listings(page), 1);
  await page.reload();
  await page.waitForSelector(".divider");
  await askCard(page).waitFor();
  assert.equal(await said(page, ".ask-who"), "wsh ai · pane 2", "the card is back without any event");
  assert.equal(await listings(page), 1, "one listing for each load");
  assert.deepEqual(await askCalls(page), []);

  // A listing that is refused shows no card and no error.
  await page.addInitScript(() => {
    window.__MOCK__.on("ai_ask_pending", () => {
      throw "the shell could not answer";
    });
  });
  await page.reload();
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_ask_pending").length >= 1);
  await page.waitForTimeout(100);
  assert.equal(await askCard(page).count(), 0, "a refused listing shows nothing");
}

async function closedCase(page, url) {
  await start(page, url);
  // An answered request, and the end of the app, leave nothing.
  for (const why of ["sent", "refused", "shutting_down"]) {
    await show(page);
    await closeAsk(page, ASK.ask, why);
    await gone(page);
    assert.equal(await notice(page), "", `${why} leaves no sentence`);
  }
  // The others leave one sentence with the number of the pane.
  const sentences = [
    ["gone", "The wsh ai request from pane 2 ended before it was answered."],
    ["timed_out", "The wsh ai request from pane 2 timed out; nothing was sent."],
    ["pane_closed", "The wsh ai request from pane 2 was withdrawn: the pane closed."],
  ];
  for (const [why, sentence] of sentences) {
    await show(page);
    await closeAsk(page, ASK.ask, why);
    await gone(page);
    assert.equal(await notice(page), sentence, why);
  }
  // A close for a request this window does not hold changes nothing and says nothing.
  await closeAsk(page, "9".repeat(32), "gone");
  assert.equal(await notice(page), sentences[2][1]);

  // The palette entry is gone after the last card.
  await show(page);
  await show(page, askView({ ask: SECOND }));
  await closeAsk(page, ASK.ask, "sent");
  const listed = () => page.evaluate(() => window.__registry.listActions().some((a) => a.id === "ai.ask_focus"));
  assert.equal(await listed(), true, "one card still waits");
  await closeAsk(page, SECOND, "sent");
  await gone(page);
  assert.equal(await listed(), false, "the entry is gone with the last card");
}

async function faultCase(page, url) {
  await start(page, url);
  const answer = async () => {
    await tickClock(page, 600);
    await askCard(page).locator(".ask-yes").click();
  };

  // A changed request: the card stays with the core's words, the buttons are free and the cards are asked for again.
  await show(page);
  await page.evaluate((text) => window.__MOCK__.ai.ask.fail({ code: "ask", reason: "seal_mismatch", text }), GOLDEN.faults.seal_mismatch);
  await answer();
  await page.waitForFunction(() => document.querySelector(".ask-fault")?.textContent?.length > 0);
  assert.equal((await said(page, ".ask-fault")).replace(/^!\s*/, ""), GOLDEN.faults.seal_mismatch);
  assert.equal(await askCard(page).locator(".ask-yes").isEnabled(), true);
  assert.equal(await askCard(page).locator(".ask-no").isEnabled(), true);
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_ask_pending").length === 2);
  assert.equal(await listings(page), 2, "the cards were asked for once more");

  // An unknown request: the card goes and the status line says why.
  await page.evaluate((text) => window.__MOCK__.ai.ask.fail({ code: "ask", reason: "unknown_ask", text }), GOLDEN.faults.unknown_ask);
  await askCard(page).locator(".ask-yes").click();
  await gone(page);
  assert.equal(await notice(page), "That request is not waiting any more.");

  // Any other object shows its text, a string shows the fixed sentence, and markup in a text stays text.
  await show(page);
  await page.evaluate(() => window.__MOCK__.ai.ask.fail({ code: "ask", reason: "bad_verdict", text: "refused <b>for a reason</b>" }));
  await answer();
  await page.waitForFunction(() => document.querySelector(".ask-fault")?.textContent?.includes("refused"));
  assert.ok((await said(page, ".ask-fault")).includes("refused <b>for a reason</b>"));
  assert.equal(await askCard(page).locator("b").count(), 0, "markup in a fault is text");
  await page.evaluate(() => window.__MOCK__.ai.ask.fail("the shell failed"));
  await askCard(page).locator(".ask-yes").click();
  await page.waitForFunction(() => document.querySelector(".ask-fault")?.textContent?.includes("could not be sent"));
  assert.ok((await said(page, ".ask-fault")).endsWith("The answer could not be sent."));
  // The next press goes through, and the card is gone.
  await askCard(page).locator(".ask-yes").click();
  await gone(page);
}

async function contractCase(page, url) {
  // The mock's cards and events have the member names of the core's golden and of the protocol sample.
  const sorted = (o) => Object.keys(o).sort();
  assert.deepEqual(sorted(ASK), sorted(GOLDEN.view));
  assert.deepEqual(sorted(LOCAL_ASK), sorted(GOLDEN.view));
  assert.deepEqual(sorted(ASK.input), sorted(GOLDEN.view.input));
  assert.deepEqual(sorted(GOLDEN.view), sorted(PROTOCOL.events["ai-ask"].view));
  assert.equal(LOCAL_ASK.input, null);
  assert.deepEqual(GOLDEN.closed.sort(), ["gone", "pane_closed", "refused", "sent", "shutting_down", "timed_out"]);

  await start(page, url);
  await show(page, GOLDEN.view);
  assert.equal(await said(page, ".ask-who"), "wsh ai · pane 3", "the page's reader accepts the card of the golden");
  await askCard(page).locator(".ask-no").click();
  await gone(page);
  await show(page, GOLDEN.view_prompt_only);
  assert.equal(await said(page, ".ask-who"), "wsh ai · pane 1");
  assert.equal(await askCard(page).getAttribute("data-leaves"), "false", "a destination on this computer is not marked");
  assert.equal(await askCard(page).locator('[data-row="input"] .ask-counts').textContent(), "none");
  assert.equal(await askCard(page).locator(".ask-best").count(), 0, "no best-effort sentence without piped text");
  assert.equal(await said(page, ".ask-place"), "this computer");
  await askCard(page).locator(".ask-no").click();
  await gone(page);

  // The answer carries the argument names of the command of the protocol sample, and the events the names of the golden.
  const made = await askCalls(page);
  assert.deepEqual(Object.keys(made[0]), PROTOCOL.commands.ai_ask_answer);
  assert.deepEqual(PROTOCOL.commands.ai_ask_pending, []);
  const sent = await page.evaluate(() => window.__MOCK__.ai.ask.answers);
  assert.equal(sent.length, 2);
  assert.deepEqual(GOLDEN.verdicts, ["send", "refuse"]);
  assert.deepEqual(made.map((c) => c.verdict), ["refuse", "refuse"]);
}
