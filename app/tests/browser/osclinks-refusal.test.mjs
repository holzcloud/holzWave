// A click the host refuses or cannot carry out is answered in the status line (docs/design.md 6.31, rule 5 of section 2): the
// notice segment holds the host's own sentence for the usual time, as text, cut at 200 characters, and anything that is not a plain
// string becomes one fixed sentence. A click that succeeds says nothing, and the browser build never asks the host. The cases of the
// link tests that do not need a clock of five seconds are in osclinks.test.mjs; this file has its own time budget.
import assert from "node:assert/strict";
import { callsOf, currentBackend } from "./lib.mjs";
import { DOCS, frames, gestureClick, modClick, open, rowOf, step, waitDialog } from "./osclinks.test.mjs";

const FIXED = "The link could not be opened.";
const HOST = "file links are not in the list of link schemes in the settings";
const NOTICE_MS = 5000;

// Row 0: a link whose text is its address (opens at once). Row 1: a link whose text is not (asks first). Row 2: a printed address.
// Row 3: a printed path. Row 4: nothing.
const ROWS = () => [
  rowOf(0, ["go ", { link: DOCS }, " now"], [[3, 3 + DOCS.length - 1, DOCS]]),
  rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, DOCS]]),
  rowOf(2, ["print https://example.com/a?b=1 now"]),
  rowOf(3, ["see ./notes/a.txt here"]),
  rowOf(4, ["plain"]),
];

/** What the notice segment of the status line holds, or null when there is none. */
const notice = (page) =>
  page.evaluate(() => {
    const n = document.querySelector("#status .seg.notice");
    return n ? { text: n.textContent, kids: n.children.length } : null;
  });
const waitNotice = (page, want) =>
  page.waitForFunction((w) => document.querySelector("#status .seg.notice")?.textContent === w, want);
/** Say nothing in the status line, so the next case cannot read the last one's words. */
const clearNotice = (page) =>
  page.evaluate(async () => (await import(new URL("status.js", location.href).href)).showNotice(""));
/** Make the host's command reject with `value` (a string, as the shell's error, or anything else). */
const refuse = (page, cmd, value) =>
  page.evaluate(([c, v]) => window.__MOCK__.on(c, () => { throw v; }), [cmd, value]);

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await step("refused click", () => refusedCase(page, url));
  await step("what is said", () => saidCase(page, url));
  await step("hint and question", () => pathsCase(page, url));
  if (currentBackend() === "ws") await step("web", () => webCase(page, url));
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A click the host refuses is answered with its sentence; the sentence goes after the usual time; a click that succeeds says nothing. */
async function refusedCase(page, url) {
  const g = await open(page, url, { rows: ROWS() });
  assert.deepEqual((await notice(page))?.text ?? "", "", "nothing is said before a click");

  await refuse(page, "open_url", HOST);
  const t0 = Date.now();
  await gestureClick(page, g.pt(1, 8, 0));
  await waitNotice(page, HOST);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: DOCS, pane: 1 }], "the host was asked once");
  assert.equal((await notice(page)).kids, 0, "the sentence is text");

  // It goes after the usual time, by itself.
  await page.waitForFunction(() => document.querySelector("#status .seg.notice")?.textContent === "", null, { timeout: NOTICE_MS + 3000 });
  assert.ok(Date.now() - t0 >= NOTICE_MS - 500, `the notice stays for the usual time (${Date.now() - t0} ms)`);

  // The control: a click that succeeds says nothing, after a refusal as well.
  await page.evaluate(() => window.__MOCK__.on("open_url", null));
  await gestureClick(page, g.pt(1, 8, 0));
  await page.waitForFunction(() => window.__MOCK__.callsOf("open_url").length === 2);
  await frames(page, 3);
  await page.waitForTimeout(300);
  assert.equal((await notice(page))?.text ?? "", "", "a click that succeeds shows no notice");
}

/** Only a plain, short string is said as it is; anything else is the fixed sentence; the text makes no element. */
async function saidCase(page, url) {
  const g = await open(page, url, { rows: ROWS() });
  const markup = "<img src=x onerror=window.__owned=1> refused";
  const cases = [
    ["a host sentence is said as it is", HOST, HOST],
    ["an object", { code: "open", text: "no" }, FIXED],
    ["an empty string", "", FIXED],
    ["a blank string", "   ", FIXED],
    ["a control character", "cannot\u0007 start", FIXED],
    ["a new line", "cannot\nstart", FIXED],
    ["a bidirectional override", "report ‮fdp.exe", FIXED],
    ["an isolate", "report ⁦x⁩", FIXED],
    ["a zero width character", "a​b", FIXED],
    ["a line separator", "one two", FIXED],
    ["a paragraph separator", "one two", FIXED],
    ["500 characters are cut to 200", "a".repeat(500), "a".repeat(200)],
    ["200 characters stay whole", "b".repeat(200), "b".repeat(200)],
    ["200 characters of two units each count as 200", "\u{1F600}".repeat(300), "\u{1F600}".repeat(200)],
    ["markup is text", markup, markup],
  ];
  let calls = 0;
  for (const [what, value, want] of cases) {
    await clearNotice(page);
    await refuse(page, "open_url", value);
    await gestureClick(page, g.pt(1, 8, 0));
    calls += 1;
    await page.waitForFunction((n) => window.__MOCK__.callsOf("open_url").length === n, calls);
    try {
      await waitNotice(page, want);
    } catch {
      assert.equal((await notice(page))?.text, want, what);
    }
    assert.equal((await notice(page)).kids, 0, `${what}: no element`);
  }
  assert.equal(await page.evaluate(() => document.querySelectorAll("#status img").length), 0, "no element is made from a refusal");
  assert.equal(await page.evaluate(() => window.__owned), undefined);
}

/** A printed address (no pane), a path, a hint chosen with Shift and the Open button of the question are answered the same way. */
async function pathsCase(page, url) {
  const g = await open(page, url, { rows: ROWS() });

  // A printed address: the call has no pane.
  await refuse(page, "open_url", HOST);
  await modClick(page, "Meta", g.pt(1, 12, 2));
  await waitNotice(page, HOST);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: "https://example.com/a?b=1" }], "a printed address asks without a pane");

  // A path: `reveal_path`, with its own sentence.
  await clearNotice(page);
  const gone = "there is no such file or folder";
  await refuse(page, "reveal_path", gone);
  await gestureClick(page, g.pt(1, 8, 3));
  await waitNotice(page, gone);
  assert.equal((await callsOf(page, "reveal_path")).length, 1, "the path was revealed through the host");

  // The Open button of the question.
  await clearNotice(page);
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.click(".dlg-open");
  await waitDialog(page, false);
  await waitNotice(page, HOST);
  assert.equal((await callsOf(page, "open_url")).length, 2, "Open asked the host once more");

  // A link chosen in hint mode with Shift held: pane 2 is focused, and its link of row 0 is the first target.
  await clearNotice(page);
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelector("#overlay [data-hint]") !== null);
  await page.keyboard.press("Shift+A");
  await waitNotice(page, HOST);
  assert.equal((await callsOf(page, "open_url")).length, 3, "the hint asked the host once more");
  assert.deepEqual((await callsOf(page, "open_url")).at(-1), { url: DOCS, pane: 2 });
}

/** The browser build opens a tab itself and never asks the host, so a host that refuses is not heard. */
async function webCase(page, url) {
  const g = await open(page, url, { os: "web", rows: ROWS(), spy: true });
  await refuse(page, "open_url", HOST);
  await modClick(page, "Control", g.pt(1, 8, 0));
  await page.waitForFunction(() => window.__opened.length === 1);
  await frames(page, 3);
  assert.equal((await callsOf(page, "open_url")).length, 0, "the host is not asked");
  assert.equal((await notice(page))?.text ?? "", "", "nothing is said");
}
