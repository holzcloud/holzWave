// The card of a `wsh ai` request (docs/design.md section 6.20): a floating layer that shows the exact prompt, the piped text by
// counts and findings, and the destination, and that never takes the keyboard. One real press after half a second sends, with the
// request, the word and the card's own seal and nothing else; a refusal is always possible; a script cannot press. The page holds
// no text but the prompt and draws it as text. A mock that plays the core is held to the golden of the core's types.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ASK, ASK_WORD, LOCAL_ASK, askCalls, askCard, askView, boot, closeAsk, emitAsk, tickClock } from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/ai_ask.json", import.meta.url), "utf8"));
const OTHER = "f0e1d2c3b4a5968778695a4b3c2d1e0f";

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await quietCase(page, url);
  await idsCase(page, url);
  await guardCase(page, url);
  await keysCase(page, url, engine);
  await hostileCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with a frozen clock, so a window measured in milliseconds is the same on every machine. */
const start = (page, url, extra = {}) => boot(page, url, { clock: true, ...extra });

/** Show the card of `view` and wait for it. */
async function show(page, view = ASK) {
  await emitAsk(page, view);
  await askCard(page).waitFor();
}

/** The text of an element of the card. */
const said = (page, selector) => askCard(page).locator(selector).textContent();

const gone = (page) => page.waitForFunction(() => document.querySelector("section.ask") === null);

async function traceCase(page, url) {
  await start(page, url);
  assert.equal(await askCard(page).count(), 0, "no card at first");
  assert.ok((await page.evaluate(() => window.__MOCK__.listening("ai-ask"))) >= 1, "the page listens for the requests");
  await page.evaluate(() => {
    window.__before = document.activeElement;
  });
  await show(page);

  // One card in the window, with the words of the design.
  assert.equal(await page.locator("section.ask").count(), 1);
  assert.equal(await page.evaluate(() => document.querySelector("section.ask").parentElement === document.body), true);
  const card = askCard(page);
  assert.equal(await card.getAttribute("role"), "alertdialog");
  assert.equal(await card.getAttribute("data-leaves"), "true");
  assert.equal(await card.getAttribute("data-state"), "waiting");
  assert.equal(await said(page, ".ask-who"), "wsh ai · pane 2");
  assert.equal(await said(page, ".ask-title"), "Send this to the model?");
  assert.equal(await said(page, ".ask-lead"), "A program in pane 2 asked this. Nothing has left this computer yet.");
  assert.equal(await said(page, ".ask-text"), "why does this fail");
  const input = await card.locator('[data-row="input"]').textContent();
  for (const words of ["142 lines · 6.1 KiB, 5.8 KiB after cleaning", "Anthropic key hidden", "Best effort, not a guarantee"]) {
    assert.ok(input.includes(words), `${words} in ${input}`);
  }
  assert.equal(await said(page, ".ask-route"), "anthropic · example-model");
  assert.equal(await said(page, ".ask-host"), "api.example.org");
  assert.equal(await said(page, ".ask-place"), "cloud · leaves this computer");
  assert.equal(await said(page, ".ask-foot"), "The answer is printed in pane 2. holzWave does not keep this request. Esc refuses.");
  assert.equal(await card.locator(".ask-count").count(), 0, "one request, no count");
  assert.equal(await said(page, ".ask-no"), "Don't send");
  assert.equal(await said(page, ".ask-yes"), "Send once");

  // The card never takes the keyboard, and nothing is answered yet.
  assert.equal(await page.evaluate(() => document.activeElement === window.__before), true, "the focus did not move");
  assert.deepEqual(await askCalls(page), []);

  // A real press on `Don't send` is one call with the three values as read; the close event removes the card.
  await card.locator(".ask-no").click();
  await gone(page);
  const refused = await askCalls(page);
  assert.deepEqual(refused, [{ ask: ASK.ask, verdict: "refuse", seal: ASK.seal }]);
  assert.deepEqual(Object.keys(refused[0]), ["ask", "verdict", "seal"]);

  // A second card: after half a second `Send once` is one call; the foot says `Sending…` while the answer is held.
  await page.evaluate(() => window.__MOCK__.ai.ask.hold(true));
  await show(page);
  await tickClock(page, 600);
  await card.locator(".ask-yes").click();
  await page.waitForFunction(() => document.querySelector(".ask-foot")?.textContent === "Sending…");
  assert.equal(await card.getAttribute("data-state"), "sending");
  assert.equal(await card.locator(".ask-yes").isDisabled(), true);
  assert.equal(await card.locator(".ask-no").isDisabled(), true);
  assert.deepEqual((await askCalls(page)).map((c) => c.verdict), ["refuse", "send"]);
  assert.deepEqual((await askCalls(page))[1], { ask: ASK.ask, verdict: "send", seal: ASK.seal });
  await page.evaluate(() => window.__MOCK__.ai.ask.release());
  await gone(page);
}

async function quietCase(page, url) {
  await start(page, url);
  const names = await page.evaluate(() => window.__MOCK__.calls.map((c) => c[0]));
  assert.equal(names.filter((n) => n === "ai_ask_pending").length, 1, "the list is asked for once, at load");
  for (const name of ["ai_ask_answer", "ai_send", "ai_profiles", "ai_conversation_new", "ai_context_state", "ai_context_share", "ai_context_preview"]) {
    assert.ok(!names.includes(name), `${name} was called at load`);
  }
  // An event for another window shows no card here, and no event makes the page ask again.
  await emitAsk(page, ASK, "window-2");
  await page.waitForTimeout(100);
  assert.equal(await askCard(page).count(), 0, "a card for another window is not drawn");
  await show(page);
  await closeAsk(page, ASK.ask, "sent");
  await gone(page);
  await show(page, askView({ ask: OTHER }));
  const after = await page.evaluate(() => window.__MOCK__.calls.map((c) => c[0]));
  assert.equal(after.filter((n) => n === "ai_ask_pending").length, 1, "later events cause no further listing");
  assert.ok(!after.includes("ai_ask_answer"));
}

async function idsCase(page, url) {
  await start(page, url);
  await show(page, askView({ prompt: `explain ${ASK_WORD}` }));
  assert.equal(await said(page, ".ask-text"), `explain ${ASK_WORD}`);
  await askCard(page).locator(".ask-no").click();
  await gone(page);
  const made = await askCalls(page);
  assert.deepEqual(made, [{ ask: ASK.ask, verdict: "refuse", seal: ASK.seal }], "only the three ids");
  const everything = await page.evaluate(() => JSON.stringify(window.__MOCK__.calls));
  assert.ok(!everything.includes(ASK_WORD), "the prompt occurs in no call of any command");
  assert.equal(await page.evaluate(() => localStorage.length + sessionStorage.length), 0, "nothing is stored");
}

async function guardCase(page, url) {
  await start(page, url);
  // `Send once` within 100 ms makes no call; `Don't send` at once makes one.
  await show(page);
  await tickClock(page, 100);
  await askCard(page).locator(".ask-yes").click();
  await page.waitForTimeout(50);
  assert.deepEqual(await askCalls(page), [], "Send once within the window");
  await askCard(page).locator(".ask-no").click();
  await gone(page);
  assert.deepEqual((await askCalls(page)).map((c) => c.verdict), ["refuse"], "Don't send is never held back");

  // A click that a script made, inside the page, makes no call on either button.
  await show(page);
  await tickClock(page, 1000);
  await page.evaluate(() => {
    document.querySelector(".ask-yes").click();
    document.querySelector(".ask-no").click();
  });
  await page.waitForTimeout(50);
  assert.equal((await askCalls(page)).length, 1, "a scripted click answers nothing");
  assert.equal(await askCard(page).count(), 1, "and the card stays");

  // A double click after the window is one call.
  await askCard(page).locator(".ask-yes").dblclick();
  await gone(page);
  assert.deepEqual((await askCalls(page)).map((c) => c.verdict), ["refuse", "send"]);

  // While the answer is held a second real press on the buttons makes no call.
  await page.evaluate(() => window.__MOCK__.ai.ask.hold(true));
  await show(page);
  await tickClock(page, 1000);
  await askCard(page).locator(".ask-yes").click();
  await page.waitForFunction(() => document.querySelector(".ask-yes")?.disabled === true);
  for (const selector of [".ask-yes", ".ask-no"]) {
    const box = await askCard(page).locator(selector).boundingBox();
    await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  }
  await page.waitForTimeout(50);
  assert.equal((await askCalls(page)).length, 3, "one more call, made by the first press");
  await page.evaluate(() => window.__MOCK__.ai.ask.release());
  await gone(page);
}

async function keysCase(page, url, engine) {
  await start(page, url);
  const forward = () => page.keyboard.press(engine === "webkit" && process.platform === "darwin" ? "Alt+Tab" : "Tab");
  const openPalette = async () => {
    await page.keyboard.press("Meta+k");
    await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  };
  const titles = () => page.evaluate(() => [...document.querySelectorAll(".palette-row .palette-title")].map((n) => n.textContent));

  // The palette has no such entry while no card waits.
  await openPalette();
  await page.keyboard.type("wsh ai");
  assert.ok(!(await titles()).includes("Answer the wsh ai request"), "no entry without a card");
  await page.keyboard.press("Escape");

  // `Escape` with the focus inside the card refuses; with the focus in the terminal it answers nothing.
  await show(page);
  await page.evaluate(() => document.querySelector(".ask-no").focus());
  await page.keyboard.press("Escape");
  await gone(page);
  assert.deepEqual((await askCalls(page)).map((c) => c.verdict), ["refuse"]);
  await show(page);
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(50);
  assert.equal((await askCalls(page)).length, 1, "Escape in the terminal does not answer");
  assert.equal(await askCard(page).count(), 1);

  // While the card waits the palette lists the entry, and running it puts the keyboard on `Don't send`.
  await openPalette();
  await page.keyboard.type("wsh ai");
  assert.ok((await titles()).includes("Answer the wsh ai request"));
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.activeElement?.classList.contains("ask-no"));
  assert.equal(await page.evaluate(() => document.getElementById("ime") === document.activeElement), false);

  // Every control is a field of the page; the tab order is `Don't send`, then `Send once`; `Enter` early answers nothing.
  assert.equal(await page.evaluate(() => [...document.querySelectorAll("section.ask button, section.ask .ask-text")].every((n) => n.dataset.field !== undefined)), true);
  await forward();
  await page.waitForFunction(() => document.activeElement?.classList.contains("ask-yes"));
  await page.keyboard.press("Enter");
  await page.waitForTimeout(50);
  assert.equal((await askCalls(page)).length, 1, "Enter on Send once within the window answers nothing");
  assert.equal(await askCard(page).count(), 1);

  // After the card closes the entry is gone.
  await closeAsk(page, ASK.ask, "sent");
  await gone(page);
  await openPalette();
  await page.keyboard.type("wsh ai");
  assert.ok(!(await titles()).includes("Answer the wsh ai request"), "the entry went with the card");
  await page.keyboard.press("Escape");
}

async function hostileCase(page, url) {
  await start(page, url);
  const markup = `<img src=x onerror="window.__XSS=1"> ${ASK_WORD}‮\u001b`;
  await show(page, askView({ prompt: markup, host: "<b>api</b>.example.org", model: "<i>m</i>" }));
  const card = askCard(page);
  assert.equal(await page.evaluate(() => window.__XSS), undefined, "no handler ran");
  assert.equal(await card.locator("img, b, i, script").count(), 0, "no element was made from the text");
  const text = await said(page, ".ask-text");
  assert.ok(text.includes('<img src=x onerror="window.__XSS=1">'), "the markup is shown as text");
  assert.ok(text.includes("\\u{202e}") && text.includes("\\u{1b}"), `hidden characters are escapes: ${text}`);
  assert.ok(!text.includes("‮") && !text.includes("\u001b"), "no raw hidden character");
  assert.ok((await said(page, ".ask-host")).includes("<b>api</b>"), "a host with markup characters is text");
  const holders = await page.evaluate(
    (word) =>
      [...document.querySelectorAll("section.ask, section.ask *")].flatMap((n) =>
        [...n.attributes].filter((a) => a.value.includes(word) || a.value.includes("onerror")).map((a) => `${n.className}[${a.name}]`),
      ),
    ASK_WORD,
  );
  assert.deepEqual(holders, [], "no attribute carries the prompt");
  await card.locator(".ask-no").click();
  await gone(page);

  // A prompt of 16384 characters scrolls inside its box and is whole in the text.
  const long = Array.from({ length: 16384 }, (_, n) => (n % 64 === 63 ? "\n" : "w")).join("");
  await show(page, askView({ prompt: long }));
  const sizes = await page.evaluate(() => {
    const box = document.querySelector(".ask-text");
    return { scroll: box.scrollHeight, client: box.clientHeight, length: box.textContent.length };
  });
  assert.ok(sizes.scroll > sizes.client, `the box scrolls: ${JSON.stringify(sizes)}`);
  assert.equal(sizes.length, long.length, "the whole prompt is in the text");
  assert.equal(await page.evaluate(() => window.__XSS), undefined);
}
