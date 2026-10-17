// The Markdown corpus of the chat on both engines. The core's test (core/tests/ai_xss_golden.rs) writes the corpus with the
// HTML the real renderer made for each case and the raw HTML the parser made before the sanitiser; this file replays both
// through the real chat page, so the page's own allow-list is held even if the core's sanitiser were to fail.
import assert from "node:assert/strict";
import { PROFILE, boot, calls, openAi, sendText } from "./ai-lib.mjs";
import { dropInertReports } from "./lib.mjs";
import { armGuards, checkerCase, domProblems, loadCorpus, pageClean, responsive } from "./aigate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  const corpus = loadCorpus();
  await boot(page, url, { profiles: [PROFILE] });
  const guards = armGuards(page, url);
  // The checker builds an inert hostile document in the page; the policy reports the inline style and handlers it holds.
  await checkerCase(page, errors);
  await traceCase(page, guards, corpus);
  // The raw pass feeds hostile markup to `appendSafe`, which parses it in an inert document: the policy reports its handlers.
  await dropInertReports(page, errors);
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
  console.log(`aigate trace: ${corpus.cases.length} case in the corpus, replayed as core_html and raw_html`);
}
