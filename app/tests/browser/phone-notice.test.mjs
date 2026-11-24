// A notice on a phone (docs/design.md 6.6, the paragraph Notices): the phone draws no status line, so a notice that the status line
// would hold is a line over the top of the stage. A terminal link that the host refuses shows its sentence there with the `!` mark, an
// underline and the warn colour, at least 44 px high, while the status line stays hidden. The platform is mocked (`app_info`
// answers `android`) and the window is sized like a phone in the desktop engines; a phone with a hardware keyboard is played by the
// keyboard route of the page (hint mode with Shift). Nothing here was run on a real phone or with a real host.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";
import { frame, open, shown } from "./touch-lib.mjs";

// The phone layout is the Android shell's; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the phone layout belongs to the Android shell, which the web backend never is";

const REFUSAL = "No app on this device can open this link.";
const URL_TEXT = "https://example.com/a";

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(6000);
  for (const [name, run] of [["a refused terminal link", terminalLinkCase]]) {
    try {
      await run(page, url, engine);
    } catch (e) {
      // The case is named, and the line of the test that failed is kept, because the runner prints one line.
      const where = String(e?.stack ?? "").split("\n").find((l) => l.includes("phone-notice.test.mjs"))?.trim() ?? "";
      throw new Error(`${name}: ${e?.message ?? e} ${where}`);
    }
  }
  assert.deepEqual(errors, [], `the page reported errors (${engine})`);
}

/** The line of the phone, as the page draws it, or null while there is none. */
const lineOf = (page) =>
  page.evaluate(() => {
    const n = document.querySelector("#phone-notice .phone-notice-line");
    if (!n) return null;
    const r = n.getBoundingClientRect();
    const st = getComputedStyle(n);
    const mark = n.querySelector(".phone-notice-mark");
    return {
      text: n.querySelector(".phone-notice-text")?.textContent ?? null,
      tone: n.dataset.tone,
      mark: mark ? { text: mark.textContent, hidden: mark.getAttribute("aria-hidden") } : null,
      decoration: st.textDecorationLine,
      height: r.height,
      top: r.top,
      bottom: r.bottom,
      border: st.borderTopWidth,
      animation: st.animationName,
      transition: st.transitionDuration,
      children: n.children.length,
    };
  });

const statusShown = (page) => page.evaluate(() => getComputedStyle(document.getElementById("status")).display !== "none");

/** One row of 50 cells with `text` in it. */
const rowWith = (y, text, cols = 50) => ({ y, runs: [[text.padEnd(cols), cols, 0xf6efe6, 0x14091a, 0]] });

/**
 * A printed address in the one pane of a phone, and `open_url` answering with `sentence` as a refusal. The address is opened by hint
 * mode and Shift plus its label (a phone with a hardware keyboard); a touch gesture for a link is not built.
 */
async function terminalLinkCase(page, url) {
  await open(page, url);
  const g = await shown(page, 2);
  await emit(page, "frame", frame(2, g.cols, g.rows, [], { changed: [rowWith(0, `see ${URL_TEXT} now`, g.cols)] }));
  await page.evaluate((text) => window.__MOCK__.on("open_url", () => { throw text; }), REFUSAL);
  await page.evaluate(() => document.getElementById("term").focus());
  assert.equal(await statusShown(page), false, "the status line is not drawn on a phone");

  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("hints.open"));
  await page.waitForFunction(() => document.querySelectorAll("#overlay [data-hint]").length > 0);
  const label = await page.evaluate(() => document.querySelector("#overlay [data-hint]").textContent);
  await page.keyboard.press(`Shift+${label}`);
  await page.waitForFunction(() => document.querySelector("#phone-notice .phone-notice-line") !== null);

  assert.deepEqual((await callsOf(page, "open_url")).map((a) => a.url), [URL_TEXT], "the host was asked once");
  const line = await lineOf(page);
  assert.equal(line.text, REFUSAL, "the host's sentence is the line");
  assert.equal(line.tone, "warn", "a refused link is the warn tone");
  assert.deepEqual(line.mark, { text: "!", hidden: "true" }, "the mark is `!` and is hidden from assistive technology");
  assert.match(line.decoration, /underline/, "an underline besides the colour");
  assert.ok(line.height >= 44, `the line is ${line.height} px high, under 44 px`);
  assert.equal(await statusShown(page), false, "the status line stays hidden on a phone");
}
