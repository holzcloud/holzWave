// The Markdown corpus of the chat on both engines. The core's test (core/tests/ai_xss_golden.rs) writes the corpus with the
// HTML the real renderer made for each case and the raw HTML the parser made before the sanitiser; this file replays both
// through the real chat page, so the page's own allow-list is held even if the core's sanitiser were to fail.
import assert from "node:assert/strict";
import { PROFILE, boot, calls, openAi, sendText } from "./ai-lib.mjs";
import {
  PAGE_ADDED, armGuards, checkerCase, domProblems, dropRawReports, installMessages, loadCorpus, openCorpusChat, pageClean, responsive,
} from "./aigate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  const corpus = loadCorpus();
  await boot(page, url, { profiles: [PROFILE] });
  const guards = armGuards(page, url);
  // The checker builds an inert hostile document in the page; the policy reports the inline style and handlers it holds.
  await checkerCase(page, errors);
  await traceCase(page, guards, corpus);
  await coreCase(page, url, guards, corpus);
  await rawCase(page, url, guards, corpus);
  // The raw pass feeds hostile markup to `appendSafe`, which parses it in an inert document: the policy reports its handlers.
  await dropRawReports(page, errors);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Wait until the chat shows `n` finished replies, the core was asked `asked` times and the Markdown is drawn. */
async function replied(page, n, asked) {
  await page.waitForFunction(
    ([count, calls]) =>
      document.querySelectorAll('.chat-msg[data-role="assistant"][data-status="complete"]').length === count &&
      window.__MOCK__.callsOf("markdown_render").length === calls &&
      !document.querySelector(".chat-partial"),
    [n, asked],
  );
  await page.waitForTimeout(50);
}

/**
 * The tracer: one hostile reply goes through the real chat page twice, once as the HTML the core's renderer made and once
 * as the raw HTML a failed sanitiser would deliver. Both leave a transcript with no problem.
 */
async function traceCase(page, guards, corpus) {
  const one = corpus.cases.find((c) => c.id === "script-block");
  assert.ok(one, "the corpus has the case script-block");
  assert.ok(one.raw_html.includes(one.marker), "the case is live in the raw HTML");
  assert.ok(one.core_html !== null && !one.core_html.includes(one.marker), "the case is dead in the renderer's output");
  await openAi(page);
  let finished = 0;
  for (const field of ["core_html", "raw_html"]) {
    await page.evaluate(
      ([source, html]) => {
        window.__CORPUS = { [source]: html };
        window.__MOCK__.on("markdown_render", (a) => window.__CORPUS[a.text] ?? null);
        window.__MOCK__.ai.script({ chunks: [source] });
      },
      [one.source, one[field]],
    );
    await sendText(page, `go ${field}`);
    finished += 1;
    await replied(page, finished, finished);
    assert.equal((await calls(page, "markdown_render")).at(-1).text, one.source, "the core was asked for the case");
    const found = await domProblems(page, ".chat-msg:last-of-type .chat-text");
    assert.deepEqual(found, [], `${field}: the transcript has problems`);
    assert.equal(await page.locator(".chat-msg:last-of-type .chat-text.chat-md").count(), 1, `${field}: the reply is drawn as Markdown`);
    assert.deepEqual(await pageClean(page), [], `${field}: the page is not clean`);
    await guards.assertQuiet(field);
    await responsive(page);
  }
  console.log(`aigate trace: ${corpus.cases.length} cases in the corpus, replayed as core_html and raw_html`);
}

/**
 * Every case of the corpus in one chat: the page copy of `field` (the core's cleaned HTML or the parser's raw HTML) of every
 * case, one message each. No message has a problem, no text case lost its text, the page is clean and nothing left the page.
 */
async function replay(page, url, guards, corpus, field, tag) {
  await openCorpusChat(page, url);
  const cases = corpus.cases.filter((c) => c[field] !== null);
  const skipped = corpus.cases.length - cases.length;
  const sources = new Set(cases.map((c) => c.source));
  assert.equal(sources.size, cases.length, "the sources of the corpus are unique");
  await page.evaluate(
    (map) => {
      window.__CORPUS = map;
      window.__MOCK__.on("markdown_render", (a) => window.__CORPUS[a.text] ?? null);
    },
    Object.fromEntries(cases.map((c) => [c.source, c[field]])),
  );
  await installMessages(page, "chat-x", cases.map((c) => c.source), tag);
  const failures = [];
  let replayed = 0;
  for (const [i, c] of cases.entries()) {
    const scope = `.chat-msg[data-id="${tag}-${i + 1}"] .chat-text`;
    if ((await page.locator(scope).count()) !== 1) {
      failures.push(`${c.id}: the message is not on the page`);
      continue;
    }
    const found = await domProblems(page, scope, { extra: PAGE_ADDED });
    for (const line of found) failures.push(`${c.id}: ${line}`);
    if (c.expect === "text") {
      const shown = await page.locator(scope).textContent();
      if (!shown.includes(c.visible)) failures.push(`${c.id}: the text ${JSON.stringify(c.visible)} is not shown: ${JSON.stringify(shown.slice(0, 80))}`);
    }
    if (found.length === 0) replayed += 1;
  }
  const asked = await page.evaluate(() => window.__MOCK__.callsOf("markdown_render").length);
  console.log(`aigate corpus: ${field}: ${cases.length} cases, ${replayed} replayed clean, ${skipped} skipped, ${asked} asked of the core`);
  assert.deepEqual(failures, [], `${field}: the transcript has problems`);
  assert.equal(replayed, cases.length, "every case was replayed");
  assert.equal(replayed, corpus.cases.length - skipped, "the count matches the corpus");
  assert.deepEqual(await pageClean(page), [], `${field}: the page is not clean`);
  await guards.assertQuiet(field);
  await responsive(page);
}

/** The core's cleaned HTML of every case; a case the renderer refused is skipped and counted. */
async function coreCase(page, url, guards, corpus) {
  await replay(page, url, guards, corpus, "core_html", "core");
}

/** The raw HTML of the parser for every case: what a failed sanitiser would deliver. The page's own allow-list is the only guard. */
export async function rawCase(page, url, guards, corpus) {
  await replay(page, url, guards, corpus, "raw_html", "raw");
}
