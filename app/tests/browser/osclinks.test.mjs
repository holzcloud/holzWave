// Links a program marked (OSC 8, docs/design.md 6.31) in the page: the spans of a frame row are kept in the mirror; with the
// pointer over a link the underline is solid and the status line shows the real target in a LINK segment; with the open
// gesture held the underline is rose and the pointer a hand; the gesture click opens through `open_url` with the pane and
// starts neither a selection nor a mouse report. Only the schemes of the person's list are links. A text that names another
// place than its target asks first (the dialog), the browser opens its tab from the click that confirms, and hint mode lists
// the marked links. The core and the opener are not here: the browser stub plays them.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const HEADER = 24; // CSS px of a pane's header strip at the base font size
const BG = 0x14091a;
const FG = 0xf6efe6;
const LINK = 16; // the wire flag of a cell inside a marked link
const MODES = { alt: false, bracketed_paste: false, mouse: false };
const COLS = 50;

const DOCS = "https://example.org/docs";
const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));

/**
 * One row of `COLS` cells from parts: a string is plain text, `{ link: "text" }` is text with the flag LINK. `links` are the
 * spans the core would send for it, which the test writes out itself so that the page is tested against the wire.
 */
const rowOf = (y, parts, links) => {
  const runs = [];
  let used = 0;
  for (const part of parts) {
    const text = typeof part === "string" ? part : part.link;
    runs.push([text, text.length, FG, BG, typeof part === "string" ? 0 : (part.flags ?? LINK)]);
    used += text.length;
  }
  if (used < COLS) runs.push([" ".repeat(COLS - used), COLS - used, FG, BG, 0]);
  return { y, runs, ...(links ? { links } : {}) };
};

// Row 1: a link whose text is not its address. Row 2: a link whose text is its address. Row 3 and 4: mail and a scheme that is
// not in the default list. Row 5: an address a core would never deliver. Row 6: a printed address. Row 7: a span over one.
const ROWS = () => [
  rowOf(0, ["plain text row"]),
  rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, DOCS]]),
  rowOf(2, ["go ", { link: "example.org/docs" }, " now"], [[3, 18, DOCS]]),
  rowOf(3, ["mail ", { link: "me@example.org" }, " please"], [[5, 18, "mailto:me@example.org"]]),
  rowOf(4, ["ssh ", { link: "ssh://host/x" }, " end"], [[4, 15, "ssh://host/x"]]),
  rowOf(5, ["bad ", { link: "run" }, " end"], [[4, 6, "javascript:alert(1)"]]),
  rowOf(6, ["print https://example.com/a?b=1 now"]),
  rowOf(7, ["https://example.com/x here"], [[0, 20, "https://example.com/z"]]),
];

const frame = (pane, seq, changed, over = {}) => ({
  pane, seq, full: true, cols: COLS, rows: 8, fg: FG, bg: BG, cursor: null, scrollbar: [0, 8, 8],
  selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed, ...over,
});

/** Open the page as `os` with the rows in both panes; returns the geometry helpers. */
async function open(page, url, { os = "macos", rows = ROWS(), schemes = null } = {}) {
  await page.addInitScript((o) => window.__MOCK__.on("app_info", () => ({ os: o, version: "t", hostname: "h" })), os);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
  const start = (await callsOf(page, "start_pane"))[0];
  const c = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });
  const cw = start.cell_w;
  const ch = start.cell_h;
  const origin = (pane) => c.left + (pane === 1 ? 0 : c.width / 2) + PAD;
  if (schemes) await setSchemes(page, schemes);
  await emit(page, "frame", frame(1, 1, rows));
  await emit(page, "frame", frame(2, 1, rows));
  await page.evaluate(() => window.__MOCK__.reset());
  return {
    cw, ch,
    pt: (pane, cx, cy) => [origin(pane) + (cx + 0.5) * cw, c.top + HEADER + (cy + 0.5) * ch],
    box: (pane, y, x0, x1) => ({ x: origin(pane) + x0 * cw, y: c.top + HEADER + y * ch, w: (x1 - x0 + 1) * cw, h: ch }),
  };
}

/** The settings the core would send, with the list of link schemes the test asks for. */
async function setSchemes(page, schemes) {
  const view = structuredClone(golden.settings_view);
  view.settings.link_schemes = schemes;
  await emit(page, "settings", view);
  await page.waitForFunction(
    async (want) => {
      const { state } = await import(new URL("state.js", location.href).href);
      return JSON.stringify(state.settings?.settings.link_schemes) === JSON.stringify(want);
    },
    schemes,
  );
}

const frames = (page, n = 2) =>
  page.evaluate(async (count) => {
    for (let i = 0; i < count; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  }, n);
const names = (page, ...cmds) =>
  page.evaluate((set) => window.__MOCK__.calls.filter(([c]) => set.includes(c)).map(([c, a]) => [c, a]), cmds);

/** Hold `mod`, click at (x, y), release. */
async function modClick(page, mod, [x, y]) {
  await page.keyboard.down(mod);
  await page.mouse.click(x, y);
  await page.keyboard.up(mod);
}

/** The box of the overlay element `selector` (client pixels), or null. */
const boxOf = (page, selector) =>
  page.evaluate((sel) => {
    const n = document.querySelector(sel);
    if (!n) return null;
    const r = n.getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  }, selector);
const near = (got, want, what) => {
  assert.ok(got, `${what}: nothing is there`);
  for (const k of ["x", "y", "w", "h"]) assert.ok(Math.abs(got[k] - want[k]) < 0.6, `${what}: ${k} ${got[k]} != ${want[k]}`);
};
/** What the LINK segment of the status line holds: its text and its accessible name, or null when it is not shown. */
const linkSeg = (page) =>
  page.evaluate(() => {
    const n = document.querySelector("#status .seg.link");
    if (!n || getComputedStyle(n).display === "none") return null;
    return { text: n.textContent, label: n.getAttribute("aria-label"), kids: [...n.children].map((k) => [k.className, k.textContent]) };
  });
const cursor = (page) => page.evaluate(() => getComputedStyle(document.getElementById("term")).cursor);
/** Wait until the LINK segment is shown (or gone). */
const waitSeg = (page, shown) =>
  page.waitForFunction((want) => {
    const n = document.querySelector("#status .seg.link");
    return (n !== null && getComputedStyle(n).display !== "none") === want;
  }, shown);

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await mirrorCase(page, url);
  await hoverCase(page, url);
  await gestureCase(page, url);
  await schemeCase(page, url);
  await textLinkCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The mirror keeps the spans of a row until it is sent again, and keeps only spans it can use. */
async function mirrorCase(page, url) {
  await open(page, url);
  const spans = () =>
    page.evaluate(async () => {
      const { state } = await import(new URL("state.js", location.href).href);
      return state.frames.get(1).rows_.map((r) => (r && r.links ? r.links : null));
    });
  assert.deepEqual((await spans())[1], [[4, 7, DOCS]]);
  assert.deepEqual((await spans())[0], null);

  // A frame that sends another row keeps this one's spans.
  await emit(page, "frame", frame(1, 2, [rowOf(0, ["changed"])], { full: false }));
  assert.deepEqual((await spans())[1], [[4, 7, DOCS]], "an incremental frame leaves the other rows' spans alone");

  // Malformed spans are dropped, at most eight stay.
  const bad = [
    [-1, 3, "https://a.example/"], [5, 2, "https://b.example/"], [0, COLS, "https://c.example/"], [1.5, 2, "https://d.example/"],
    ["1", 2, "https://e.example/"], [1, 2, 3], [1, 2], [1, 2, ""],
  ];
  const many = Array.from({ length: 10 }, (_, i) => [i * 4, i * 4 + 2, `https://h${i}.example/`]);
  await emit(page, "frame", frame(1, 3, [rowOf(2, ["go example.org/docs now"], [...bad, ...many])], { full: false }));
  const kept = (await spans())[2];
  assert.equal(kept.length, 8, "eight spans at most");
  assert.deepEqual(kept.map((s) => s[2]), many.slice(0, 8).map((s) => s[2]), "the malformed ones are not among them");

  // A row sent again without spans has none.
  await emit(page, "frame", frame(1, 4, [rowOf(1, ["see docs here"])], { full: false }));
  assert.equal((await spans())[1], null);
}

/** The pointer over a link underlines it solid and the status line shows the real target; leaving removes both; nothing opens. */
async function hoverCase(page, url) {
  const g = await open(page, url);
  assert.equal(await linkSeg(page), null, "no link under the pointer: no segment");

  await page.mouse.move(...g.pt(1, 5, 1));
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") !== null);
  near(await boxOf(page, "#overlay .linkon"), g.box(1, 1, 4, 7), "the solid underline covers the four cells");
  const line = await page.evaluate(() => parseFloat(getComputedStyle(document.querySelector("#overlay .linkon"), "::after").height));
  assert.equal(line, 1, "the cue is a line of one pixel");
  assert.equal(
    await page.evaluate(() => getComputedStyle(document.querySelector("#overlay .linkon"), "::after").backgroundColor),
    "rgb(246, 239, 230)",
    "the line is in the colour of the cells",
  );
  assert.equal(await boxOf(page, "#overlay .linkhover"), null, "no rose without the gesture");
  assert.notEqual(await cursor(page), "pointer", "the hand is the gesture's");
  await waitSeg(page, true);
  const seg = await linkSeg(page);
  assert.equal(seg.text, `LINK${DOCS}`, "the label and the real target");
  assert.equal(seg.label, `Link target ${DOCS}`, "the full target is the accessible name");
  assert.deepEqual(seg.kids.map((k) => k[1]), ["LINK", "https://example.org", "/docs"], "the host in one part, the rest in another");
  assert.equal((await callsOf(page, "open_url")).length, 0, "hovering opens nothing");

  // Over the next cell of the same link nothing changes; off the link both go.
  await page.mouse.move(...g.pt(1, 7, 1));
  near(await boxOf(page, "#overlay .linkon"), g.box(1, 1, 4, 7), "still the same link");
  await page.mouse.move(...g.pt(1, 2, 1));
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") === null);
  await waitSeg(page, false);

  // Another link shows its own target.
  await page.mouse.move(...g.pt(1, 5, 3));
  await waitSeg(page, true);
  assert.equal((await linkSeg(page)).label, "Link target mailto:me@example.org");

  // The address is text: markup in it makes no element.
  const hostile = "https://x.example/<img/src=x/onerror=window.__owned=1>";
  await emit(page, "frame", frame(1, 2, [rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, hostile]])], { full: false }));
  await page.mouse.move(...g.pt(1, 0, 0));
  await page.mouse.move(...g.pt(1, 5, 1));
  await waitSeg(page, true);
  assert.equal((await linkSeg(page)).label, `Link target ${hostile}`);
  assert.equal(await page.evaluate(() => document.querySelectorAll("#status img, #overlay img").length), 0, "no element is made from a target");
  assert.equal(await page.evaluate(() => window.__owned), undefined);
}

/** With the gesture held the underline is rose and the pointer a hand; the click opens once with the pane and selects nothing. */
async function gestureCase(page, url) {
  const g = await open(page, url);
  await page.mouse.move(...g.pt(1, 5, 1));
  await page.keyboard.down("Meta");
  await page.waitForFunction(() => document.querySelector("#overlay .linkhover") !== null);
  near(await boxOf(page, "#overlay .linkhover"), g.box(1, 1, 4, 7), "the rose underline covers the four cells");
  assert.equal(await boxOf(page, "#overlay .linkon"), null, "the rose replaces the solid one");
  assert.equal(await cursor(page), "pointer", "the hand");
  await waitSeg(page, true);
  assert.equal((await linkSeg(page)).label, `Link target ${DOCS}`, "the target stays in the status line");
  await page.keyboard.up("Meta");
  await page.waitForFunction(() => document.querySelector("#overlay .linkhover") === null);
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") !== null);
  assert.notEqual(await cursor(page), "pointer");

  // A link whose text is its address opens at once, with the pane.
  await page.mouse.move(...g.pt(1, 0, 0));
  await modClick(page, "Meta", g.pt(1, 8, 2));
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: DOCS, pane: 1 }], "the click opens the address and names the pane");
  assert.deepEqual(await names(page, "select", "mouse", "focus_pane"), [], "no selection, no mouse report, no focus change");

  // The second pane names itself.
  await page.waitForTimeout(450);
  await page.evaluate(() => window.__MOCK__.reset());
  await modClick(page, "Meta", g.pt(2, 8, 2));
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: DOCS, pane: 2 }], "the pane is the one the link is in");

  // A plain click on the link selects as always and opens nothing.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.waitForTimeout(450);
  await page.mouse.click(...g.pt(1, 8, 2));
  await frames(page);
  assert.deepEqual((await callsOf(page, "select")).map((a) => a.op.kind), ["begin", "clear"], "a plain click selects");
  assert.equal((await callsOf(page, "open_url")).length, 0, "a plain click opens nothing");

  // The press and the release must be on the same link: a release elsewhere opens nothing.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.waitForTimeout(450);
  await page.keyboard.down("Meta");
  await page.mouse.move(...g.pt(1, 8, 2));
  await page.mouse.down();
  await page.mouse.move(...g.pt(1, 30, 6));
  await page.mouse.up();
  await page.keyboard.up("Meta");
  assert.equal((await callsOf(page, "open_url")).length, 0, "a drag off the link opens nothing");

  // Off macOS the gesture is Ctrl.
  const l = await open(page, url, { os: "linux" });
  await modClick(page, "Meta", l.pt(1, 8, 2));
  assert.equal((await callsOf(page, "open_url")).length, 0, "Cmd is not the gesture on linux");
  await page.waitForTimeout(450);
  await modClick(page, "Control", l.pt(1, 8, 2));
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: DOCS, pane: 1 }], "Ctrl opens on linux");
}

/** Only the schemes of the list are links: no underline, no segment, no click for the others. */
async function schemeCase(page, url) {
  const g = await open(page, url);
  const hoverAt = async (cx, cy) => {
    await page.mouse.move(...g.pt(1, 0, 7));
    await page.mouse.move(...g.pt(1, cx, cy));
    await frames(page);
    return (await boxOf(page, "#overlay .linkon")) !== null;
  };
  assert.equal(await hoverAt(6, 3), true, "mailto is a link by default");
  assert.equal(await hoverAt(6, 4), false, "ssh is not a link by default");
  assert.equal(await hoverAt(5, 5), false, "javascript is never a link");
  assert.equal(await linkSeg(page), null, "a span that is not a link has no target in the status line");
  await page.keyboard.down("Meta");
  await page.mouse.click(...g.pt(1, 6, 4));
  await page.mouse.click(...g.pt(1, 5, 5));
  await page.keyboard.up("Meta");
  assert.equal((await callsOf(page, "open_url")).length, 0, "a click on a span that is not a link opens nothing");

  await setSchemes(page, ["https"]);
  assert.equal(await hoverAt(6, 3), false, "mailto is not in the list now");
  assert.equal(await hoverAt(8, 2), true, "https is");

  await setSchemes(page, ["https", "ssh"]);
  assert.equal(await hoverAt(6, 4), true, "ssh is a link when the list says so");
  await setSchemes(page, ["javascript", "https"]);
  assert.equal(await hoverAt(5, 5), false, "javascript is not a link whatever the list says");

  await setSchemes(page, []);
  assert.equal(await hoverAt(8, 2), false, "an empty list: nothing is a link");
}

/** A printed address keeps its look and its gesture, and a marked link over it is the link. */
async function textLinkCase(page, url) {
  const g = await open(page, url);
  // No hover cue without the gesture for a printed address.
  await page.mouse.move(...g.pt(1, 12, 6));
  await frames(page);
  assert.equal(await boxOf(page, "#overlay .linkon"), null, "a printed address shows nothing without the gesture");
  assert.equal(await linkSeg(page), null);

  await modClick(page, "Meta", g.pt(1, 12, 6));
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: "https://example.com/a?b=1" }], "a printed address opens as before");

  // Where a marked link covers a printed address the marked one is found.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.waitForTimeout(450);
  await page.keyboard.down("Meta");
  await page.mouse.move(...g.pt(1, 5, 7));
  await waitSeg(page, true);
  assert.equal((await linkSeg(page)).label, "Link target https://example.com/z", "the span wins over the guessed address");
  near(await boxOf(page, "#overlay .linkhover"), g.box(1, 7, 0, 20), "the underline is the span's");
  await page.keyboard.up("Meta");
}
