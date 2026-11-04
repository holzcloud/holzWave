// Links a program marked (OSC 8, docs/design.md 6.31) in the page: the spans of a frame row are kept in the mirror; with the
// pointer over a link the underline is solid and the status line shows the real target in a LINK segment; with the open
// gesture held the underline is rose and the pointer a hand; the gesture click opens through `open_url` with the pane and
// starts neither a selection nor a mouse report. Only the schemes of the person's list are links. A text that names another
// place than its target asks first (the dialog), the browser opens its tab from the click that confirms, and hint mode lists
// the marked links. The core and the opener are not here: the browser stub plays them.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend, emit } from "./lib.mjs";

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
const rowOf = (y, parts, links, cols = COLS) => {
  const runs = [];
  let used = 0;
  for (const part of parts) {
    const text = typeof part === "string" ? part : part.link;
    runs.push([text, text.length, FG, BG, typeof part === "string" ? 0 : (part.flags ?? LINK)]);
    used += text.length;
  }
  if (used < cols) runs.push([" ".repeat(cols - used), cols - used, FG, BG, 0]);
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
  pane, seq, full: true, cols: COLS, rows: 8, fg: FG, bg: BG, cursor: null, scrollbar: [0, over.rows ?? 8, over.rows ?? 8],
  selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed, ...over,
});

/** Open the page as `os` with the rows in both panes; returns the geometry helpers. */
async function open(page, url, { os = "macos", rows = ROWS(), schemes = null, remote = false, cols = COLS, spy = false } = {}) {
  await page.addInitScript((o) => window.__MOCK__.on("app_info", () => ({ os: o, version: "t", hostname: "h" })), os);
  if (remote) {
    // `remote` is the pane that has a connection profile.
    await page.addInitScript(
      (pane) => window.__MOCK__.on("pane_connections", () => [{ pane, profile: "talos", name: "Talos", state: "connected", attempt: 0, of: 0, secs: 0 }]),
      remote === true ? 1 : remote,
    );
  }
  if (spy) await spyOpen(page);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
  // The page's own modules, for predicates that must answer at once (`waitForFunction` does not wait for a promise).
  await page.evaluate(async () => {
    window.__state = (await import(new URL("state.js", location.href).href)).state;
    window.__conns = (await import(new URL("connstate.js", location.href).href)).paneConns;
  });
  const start = (await callsOf(page, "start_pane"))[0];
  const c = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });
  const cw = start.cell_w;
  const ch = start.cell_h;
  const origin = (pane) => c.left + (pane === 1 ? 0 : c.width / 2) + PAD;
  if (schemes) await setSchemes(page, schemes);
  if (remote) {
    await page.waitForFunction(() => window.__conns.size === 1);
  }
  await emit(page, "frame", frame(1, 1, rows, { cols, rows: rows.length }));
  await emit(page, "frame", frame(2, 1, rows, { cols, rows: rows.length }));
  await page.evaluate(() => window.__MOCK__.reset());
  if (remote) {
    // `reset` drops the handlers; the page asks again when the focus moves.
    await page.evaluate(
      (pane) => window.__MOCK__.on("pane_connections", () => [{ pane, profile: "talos", name: "Talos", state: "connected", attempt: 0, of: 0, secs: 0 }]),
      remote === true ? 1 : remote,
    );
  }
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
  await page.waitForFunction((want) => JSON.stringify(window.__state.settings?.settings.link_schemes) === JSON.stringify(want), schemes);
}

/**
 * Record what the page opens in the browser build: `window.open` is replaced and every click on an anchor is recorded and
 * stopped. Each record says whether the browser counted the call as inside a user gesture (`navigator.userActivation`), which is
 * what lets a tab open.
 */
const spyOpen = (page) =>
  page.addInitScript(() => {
    if (window.__linkSpy) return;
    window.__linkSpy = true;
    window.__opened = [];
    window.__anchors = [];
    window.open = (...args) => {
      window.__opened.push({ args: [...args], active: navigator.userActivation.isActive, event: window.event?.type ?? null });
      return null;
    };
    document.addEventListener(
      "click",
      (e) => {
        const a = e.target instanceof Element ? e.target.closest("a") : null;
        if (!a) return;
        window.__anchors.push({ href: a.href, target: a.getAttribute("target"), rel: a.getAttribute("rel"), active: navigator.userActivation.isActive });
        e.preventDefault();
      },
      true,
    );
  });
const opened = (page) => page.evaluate(() => structuredClone(window.__opened));
const anchors = (page) => page.evaluate(() => structuredClone(window.__anchors));

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

/** Run one case; a failure says which one. */
async function step(name, fn) {
  try {
    await fn();
  } catch (e) {
    const at = [...String(e.stack ?? "").matchAll(/osclinks\.test\.mjs:(\d+)/g)].map((m) => m[1]);
    e.message = `[${name}${at.length ? ` lines ${at.join(" < ")}` : ""}] ${e.message}`;
    throw e;
  }
}

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await step("mirror", () => mirrorCase(page, url));
  await step("hover", () => hoverCase(page, url));
  await step("gesture", () => gestureCase(page, url));
  await step("wrapped", () => wrappedCase(page, url));
  await step("scheme", () => schemeCase(page, url));
  await step("text link", () => textLinkCase(page, url));
  await step("ask", () => askCase(page, url));
  await step("ask keys", () => askKeysCase(page, url));
  await step("ask text", () => askTextCase(page, url));
  await step("file", () => fileCase(page, url));
  await step("hints", () => hintCase(page, url));
  await step("dots", () => dotsCase(page, url));
  if (currentBackend() === "ws") await step("web", () => webCase(page, url));
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

/** A link that wraps onto the next row is two links: each row has its own span, underline, target and question. */
async function wrappedCase(page, url) {
  const wrapped = "https://example.org/a/long/address";
  const rows = [
    rowOf(0, ["x".repeat(40), { link: "https://exa" }], [[40, 50, wrapped]], 51),
    rowOf(1, [{ link: "mple.org/a/long/address" }, " end"], [[0, 22, wrapped]], 51),
  ];
  const g = await open(page, url, { rows, cols: 51 });
  await page.mouse.move(...g.pt(1, 45, 0));
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") !== null);
  near(await boxOf(page, "#overlay .linkon"), g.box(1, 0, 40, 50), "the first row's part is underlined, and only it");
  await page.mouse.move(...g.pt(1, 5, 1));
  await page.waitForFunction(() => document.querySelectorAll("#overlay .linkon").length === 1);
  near(await boxOf(page, "#overlay .linkon"), g.box(1, 1, 0, 22), "the second row's part is underlined, and only it");
  assert.equal((await linkSeg(page)).label, `Link target ${wrapped}`, "both parts go to the same address");
  await gestureClick(page, g.pt(1, 45, 0));
  await waitDialog(page, true);
  assert.equal((await dialog(page)).shown, "https://exa", "the text of the part that was clicked");
  await page.keyboard.press("Escape");
  await waitDialog(page, false);
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  assert.equal((await dialog(page)).shown, "mple.org/a/long/address");
  assert.equal((await dialog(page)).target, wrapped);
  await page.keyboard.press("Escape");
  await waitDialog(page, false);
  assert.equal((await callsOf(page, "open_url")).length, 0);
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

// ---- the question ------------------------------------------------------------------------------------------

/** The dialog, or null: its title, labels, texts, buttons and which button has the focus. */
const dialog = (page) =>
  page.evaluate(() => {
    const d = document.querySelector(".dlg-link");
    if (!d) return null;
    const active = document.activeElement;
    return {
      role: d.getAttribute("role"),
      modal: d.getAttribute("aria-modal"),
      title: d.querySelector(".dlg-title")?.textContent,
      labels: [...d.querySelectorAll(".dlg-lbl")].map((n) => n.textContent),
      shown: d.querySelector(".dlg-shown")?.textContent,
      target: d.querySelector(".dlg-target")?.textContent,
      buttons: [...d.querySelectorAll("button")].map((b) => b.textContent),
      focused: active && d.contains(active) ? active.textContent : null,
      foreign: d.querySelectorAll("img, a, script, [onerror]").length,
    };
  });
const waitDialog = (page, shown) =>
  page.waitForFunction((want) => (document.querySelector(".dlg-link") !== null) === want, shown);

/** A click with the gesture on `pt`, which asks (or not). */
async function gestureClick(page, pt) {
  await page.waitForTimeout(450); // the double click window of the pointer
  await modClick(page, "Meta", pt);
}

/** A link whose text differs from its target asks; Cancel is focused; Escape, Cancel and a click outside cancel; Open opens once. */
async function askCase(page, url) {
  const g = await open(page, url);
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  const d = await dialog(page);
  assert.deepEqual(
    { role: d.role, modal: d.modal, title: d.title, labels: d.labels, shown: d.shown, target: d.target, buttons: d.buttons, focused: d.focused },
    {
      role: "alertdialog", modal: "true", title: "Open this link?", labels: ["The text says", "The link goes to"],
      shown: "docs", target: DOCS, buttons: ["Cancel", "Open"], focused: "Cancel",
    },
  );
  assert.equal((await callsOf(page, "open_url")).length, 0, "nothing opens before the answer");
  const look = await page.evaluate(() => {
    const css = (sel, prop) => getComputedStyle(document.querySelector(sel))[prop];
    const probe = document.createElement("span");
    probe.style.color = "var(--hw-rose-deep)";
    document.body.append(probe);
    const rose = getComputedStyle(probe).color;
    probe.remove();
    return {
      openBg: css(".dlg-open", "backgroundColor"), rose, ring: css(".dlg-cancel", "outlineWidth"),
      mono: css(".dlg-target", "fontFamily"), shadow: css(".dlg-link", "boxShadow") !== "none",
    };
  });
  assert.equal(look.openBg, look.rose, "Open is the solid rose button");
  assert.equal(look.ring, "2px", "Cancel, focused first, has the 2 px ring");
  assert.ok(look.shadow, "the dialog is a floating layer");

  // Escape cancels.
  await page.keyboard.press("Escape");
  await waitDialog(page, false);
  assert.equal((await callsOf(page, "open_url")).length, 0, "Escape opens nothing");

  // The Cancel button.
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.click(".dlg-cancel");
  await waitDialog(page, false);
  assert.equal((await callsOf(page, "open_url")).length, 0, "Cancel opens nothing");

  // A click outside the dialog cancels, but not the tail of the click that asked.
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.mouse.click(5, 5);
  assert.notEqual(await dialog(page), null, "the click that is part of the one that asked does not cancel");
  await page.waitForTimeout(600);
  await page.mouse.click(5, 5);
  await waitDialog(page, false);
  assert.equal((await callsOf(page, "open_url")).length, 0, "a click outside opens nothing");

  // Open opens once, with the pane, and the dialog goes.
  await gestureClick(page, g.pt(2, 5, 1));
  await waitDialog(page, true);
  await page.click(".dlg-open");
  await waitDialog(page, false);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: DOCS, pane: 2 }], "Open opens the address once, for the pane of the link");
}

/** Every key goes to the dialog; Enter answers with the focused button; Tab moves between the two. */
async function askKeysCase(page, url) {
  const g = await open(page, url);
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.keyboard.type("hello");
  await page.keyboard.press("Meta+v");
  assert.deepEqual(await names(page, "input", "text", "write", "paste", "key", "send_input"), [], "no key reaches the terminal");
  assert.notEqual(await dialog(page), null, "other keys do not close it");

  // Enter on the focused Cancel is Cancel.
  await page.keyboard.press("Enter");
  await waitDialog(page, false);
  assert.equal((await callsOf(page, "open_url")).length, 0, "Enter on Cancel cancels");

  // Tab to Open, then Enter opens; Shift+Tab and Space work as well.
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.keyboard.press("Tab");
  assert.equal((await dialog(page)).focused, "Open");
  await page.keyboard.press("Shift+Tab");
  assert.equal((await dialog(page)).focused, "Cancel");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Enter");
  await waitDialog(page, false);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: DOCS, pane: 1 }], "Enter on Open opens once");

  await page.evaluate(() => window.__MOCK__.reset());
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.keyboard.press("Tab");
  await page.keyboard.press(" ");
  await waitDialog(page, false);
  assert.equal((await callsOf(page, "open_url")).length, 1, "Space on Open opens");

  // One question at a time: a second link asks nothing while the first is open.
  await page.evaluate(() => window.__MOCK__.reset());
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  assert.equal(await page.evaluate(() => document.querySelectorAll(".dlg-link").length), 1);
  await page.keyboard.press("Escape");
  await waitDialog(page, false);

  // The terminal has its keys back.
  await page.keyboard.type("x");
  await page.waitForFunction(() => window.__MOCK__.calls.some(([c]) => c === "input" || c === "key" || c === "text" || c === "write" || c === "send_input"));
}

/** The text and the address are text: markup makes no element, the shown text is cut at 120, the address is not. */
async function askTextCase(page, url) {
  const hostileText = "<img src=x onerror=window.__owned=1>";
  const hostileUri = "https://x.example/<img/src=x/onerror=window.__owned=2>";
  const longUri = `https://example.org/${"a".repeat(600)}`;
  const wide = 140;
  const rows = [
    rowOf(0, [{ link: hostileText }, " tail"], [[0, hostileText.length - 1, hostileUri]], wide),
    rowOf(1, [{ link: "w".repeat(130) }], [[0, 129, longUri]], wide),
    rowOf(2, ["x", { link: "    " }, "y"], [[1, 4, DOCS]], wide),
    rowOf(3, [{ link: "example.org/docs" }], [[0, 15, DOCS.replace("https://example.org", "https://paypal.com.evil.example")]], wide),
  ];
  const g = await open(page, url, { rows, cols: wide });

  await gestureClick(page, g.pt(1, 3, 0));
  await waitDialog(page, true);
  let d = await dialog(page);
  assert.equal(d.shown, hostileText, "the markup is shown as text");
  assert.equal(d.target, hostileUri);
  assert.equal(d.foreign, 0, "no element was made from it");
  assert.equal(await page.evaluate(() => window.__owned), undefined);
  await page.keyboard.press("Escape");
  await waitDialog(page, false);

  await gestureClick(page, g.pt(1, 3, 1));
  await waitDialog(page, true);
  d = await dialog(page);
  assert.equal(d.shown, `${"w".repeat(120)}…`, "the text is cut at 120 characters");
  assert.equal(d.target, longUri, "the address is never cut");
  const wrap = await page.evaluate(() => {
    const t = document.querySelector(".dlg-target");
    const box = document.querySelector(".dlg-link").getBoundingClientRect();
    return { fits: t.scrollWidth <= t.clientWidth + 1, inside: t.getBoundingClientRect().right <= box.right, lines: t.getClientRects().length };
  });
  assert.ok(wrap.fits && wrap.inside, "the long address wraps inside the dialog");
  await page.keyboard.press("Escape");
  await waitDialog(page, false);

  // A blank text asks (a hidden link), and so does a lookalike host.
  await gestureClick(page, g.pt(1, 2, 2));
  await waitDialog(page, true);
  assert.equal((await dialog(page)).shown.trim(), "");
  await page.keyboard.press("Escape");
  await waitDialog(page, false);
  await gestureClick(page, g.pt(1, 2, 3));
  await waitDialog(page, true);
  assert.equal((await dialog(page)).target, "https://paypal.com.evil.example/docs");
  await page.keyboard.press("Escape");
  await waitDialog(page, false);
  assert.equal((await callsOf(page, "open_url")).length, 0);
}

// ---- file links -----------------------------------------------------------------------------------------------

const FILE_ROWS = () => [
  rowOf(0, ["open ", { link: "report.txt" }, " now"], [[5, 14, "file:///home/u/report.txt"]]),
  rowOf(1, ["open ", { link: "file:///home/u/a.txt" }, " now"], [[5, 24, "file:///home/u/a.txt"]]),
];

/** A file link is a link in a local pane on the desktop and nothing in a pane with a connection; the opener decides again. */
async function fileCase(page, url) {
  const g = await open(page, url, { rows: FILE_ROWS() });
  await page.mouse.move(...g.pt(1, 7, 0));
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") !== null);
  assert.equal((await linkSeg(page)).label, "Link target file:///home/u/report.txt");
  // The text differs: it asks, and Open reveals through the same command with the pane.
  await gestureClick(page, g.pt(1, 7, 0));
  await waitDialog(page, true);
  await page.click(".dlg-open");
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: "file:///home/u/report.txt", pane: 1 }], "the click hands the file link and the pane to the opener");
  // The text is the address: no question.
  await page.evaluate(() => window.__MOCK__.reset());
  await gestureClick(page, g.pt(1, 10, 1));
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: "file:///home/u/a.txt", pane: 1 }]);
  assert.equal(await dialog(page), null, "equal text asks nothing");

  // A pane with a connection profile: not a link, in the first pane; the second pane has none and still is.
  const r = await open(page, url, { rows: FILE_ROWS(), remote: true });
  await page.mouse.move(...r.pt(1, 7, 0));
  await frames(page);
  assert.equal(await boxOf(page, "#overlay .linkon"), null, "no underline in a pane of a server");
  assert.equal(await linkSeg(page), null, "no target in the status line");
  await gestureClick(page, r.pt(1, 7, 0));
  await gestureClick(page, r.pt(1, 10, 1));
  assert.deepEqual(await callsOf(page, "open_url"), [], "no click");
  assert.equal(await dialog(page), null);
  await page.mouse.move(...r.pt(2, 7, 0));
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") !== null);
  await page.mouse.move(...r.pt(1, 0, 1));

  // Not in the list: not a link, and a list with file but not https shows only the file link.
  const l = await open(page, url, { rows: FILE_ROWS(), schemes: ["https"] });
  await page.mouse.move(...l.pt(1, 7, 0));
  await frames(page);
  assert.equal(await boxOf(page, "#overlay .linkon"), null, "file is not in the list");
}

// ---- hint mode -----------------------------------------------------------------------------------------------

const HINT_ROWS = () => [
  rowOf(0, ["see ", { link: "docs" }, " here"], [[4, 7, DOCS]]),
  rowOf(1, ["go ", { link: "example.org/docs" }, " now"], [[3, 18, DOCS]]),
  rowOf(2, ["https://example.com/x and https://other.example/y"], [[0, 20, "https://example.com/z"]]),
  rowOf(3, ["ssh ", { link: "ssh://host/x" }, " end ", { link: "report" }], [[4, 15, "ssh://host/x"], [21, 26, "file:///home/u/r.txt"]]),
  rowOf(4, ["plain"]),
];

/** Record what the clipboard path is asked to copy. */
const spyClipboard = (page) =>
  page.evaluate(() => {
    window.__exec = [];
    window.__written = [];
    document.execCommand = (cmd) => {
      const a = document.activeElement;
      window.__exec.push([cmd, a && a.tagName, a && a.value]);
      return true;
    };
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: async (t) => { window.__written.push(t); } },
    });
  });
const copied = (page) => page.evaluate(() => window.__exec.map((e) => e[2]).concat(window.__written));

/** The hint labels and what they stand for, in the page's order. */
const hintLabels = (page) =>
  page.evaluate(() => [...document.querySelectorAll("#overlay [data-hint]")].map((n) => { const r = n.getBoundingClientRect(); return [n.dataset.hint, Math.round(r.top), Math.round(r.left)]; }));

/** Hint mode lists the marked links ahead of the printed ones on their row, once each; Shift opens with the question, a letter copies. */
async function hintCase(page, url) {
  const g = await open(page, url, { rows: HINT_ROWS() });
  await spyClipboard(page);
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelector("#overlay [data-hint]") !== null);
  let labels = await hintLabels(page);
  // docs, example.org/docs, the span on row 2, the printed address beside it, the file link: five; the ssh span is not a link.
  assert.deepEqual(labels.map((l) => l[0]), ["a", "b", "c", "d", "e"], "five targets, in reading order");
  const tops = labels.map((l) => l[1]);
  assert.ok(tops[0] < tops[1] && tops[1] < tops[2] && tops[2] === tops[3] && tops[3] < tops[4], "rows in reading order, the two of row 2 on one row");
  assert.ok(labels[2][2] < labels[3][2], "the marked link of the row is ahead of the printed one");

  // Shift+a: the text differs, so the question.
  await page.keyboard.press("Shift+A");
  await waitDialog(page, true);
  assert.equal((await dialog(page)).target, DOCS);
  assert.equal((await callsOf(page, "open_url")).length, 0);
  await page.keyboard.press("Escape");
  await waitDialog(page, false);

  // Shift+b: the text is the address, so it opens at once, with the pane.
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelector("#overlay [data-hint]") !== null);
  await page.keyboard.press("Shift+B");
  await page.waitForFunction(() => window.__MOCK__.callsOf("open_url").length === 1);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: DOCS, pane: 2 }]);
  assert.equal(await dialog(page), null);

  // Shift+d: the printed address beside the span opens as a printed address does.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelector("#overlay [data-hint]") !== null);
  await page.keyboard.press("Shift+D");
  await page.waitForFunction(() => window.__MOCK__.callsOf("open_url").length === 1);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: "https://other.example/y" }]);

  // Shift+e: the file link asks (its text differs) and Open reveals it through the opener.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelector("#overlay [data-hint]") !== null);
  await page.keyboard.press("Shift+E");
  await waitDialog(page, true);
  await page.click(".dlg-open");
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: "file:///home/u/r.txt", pane: 2 }]);

  // A plain letter copies: the address of a marked link, never the visible text.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelector("#overlay [data-hint]") !== null);
  await page.keyboard.press("c");
  assert.deepEqual(await copied(page), ["https://example.com/z"], "the address of the marked link was copied, not its text");
  assert.equal((await callsOf(page, "open_url")).length, 0, "copying opens nothing");
  assert.equal(await dialog(page), null);

  // In a pane of a server the file link is not listed.
  const r = await open(page, url, { rows: HINT_ROWS(), remote: 2 });
  void r;
  await page.keyboard.press("Meta+Shift+Space");
  await page.waitForFunction(() => document.querySelector("#overlay [data-hint]") !== null);
  labels = await hintLabels(page);
  assert.deepEqual(labels.map((l) => l[0]), ["a", "b", "c", "d"], "four targets: the file link is not one in a pane of a server");
  await page.keyboard.press("Escape");
}

// ---- the resting cue in the page -----------------------------------------------------------------------------

/** The colour of the pixel at client position (x, y) of the canvas of the page (the 2D renderer), as 0xRRGGBB. */
const canvasPixel = (page, x, y) =>
  page.evaluate(([cx, cy]) => {
    const c = document.getElementById("term");
    const r = c.getBoundingClientRect();
    const d = c.getContext("2d").getImageData(Math.floor((cx - r.left) * devicePixelRatio), Math.floor((cy - r.top) * devicePixelRatio), 1, 1).data;
    return (d[0] << 16) | (d[1] << 8) | d[2];
  }, [x, y]);

/**
 * The cells of a marked link carry a dotted line in the cells' colour; under forced colours it is the system link colour, and
 * back to the cells' colour when forced colours end. The 2D renderer is read back here, the colour logic of WebGL is in the
 * renderer test; both go through `setLinkColor` of the scene.
 */
async function dotsCase(page, url) {
  const g = await open(page, url);
  // Row 1 holds the link at columns 4 to 7. At a device pixel ratio of 1 its dots are one pixel, on the even pixels of the grid, in the
  // row two above the bottom of the cell.
  const b = g.box(1, 1, 4, 7);
  const dotRow = b.y + g.ch - 2;
  const x0 = Math.round(b.x);
  const cw = Math.round(g.cw);
  const dotPixel = (i) => (4 * cw + i) % 2 === 0;
  const first = [...Array(6).keys()].find((i) => dotPixel(i));
  await frames(page, 3); // the frame is drawn on an animation frame
  assert.equal(await canvasPixel(page, x0 + first + 0.5, dotRow + 0.5), FG, "a dot is in the colour of the cell");
  const gap = [...Array(6).keys()].find((i) => !dotPixel(i));
  assert.equal(await canvasPixel(page, x0 + gap + 0.5, dotRow + 0.5), BG, "between two dots the cell shows");
  assert.equal(await canvasPixel(page, x0 + first + 0.5, dotRow - 1.5), BG, "one pixel above: nothing");
  assert.equal(await canvasPixel(page, x0 + first + 0.5, dotRow + 1.5), BG, "one pixel below: nothing");
  assert.equal(await canvasPixel(page, x0 + first + 0.5, g.box(1, 0, 0, 0).y + g.ch - 2 + 0.5), BG, "a row with no link has none");

  await page.emulateMedia({ forcedColors: "active" });
  const honoured = await page.evaluate(() => matchMedia("(forced-colors: active)").matches);
  console.log(`osclinks: forced-colors emulation ${honoured ? "honoured" : "NOT honoured"} by this engine`);
  // The system link colour while forced colours are active (it is another one when they are not).
  const link = await page.evaluate(() => {
    const p = document.createElement("span");
    p.style.color = "LinkText";
    document.body.append(p);
    const m = /(\d+)\D+(\d+)\D+(\d+)/.exec(getComputedStyle(p).color);
    p.remove();
    return (Number(m[1]) << 16) | (Number(m[2]) << 8) | Number(m[3]);
  });

  if (honoured) {
    await page.waitForFunction(
      ([x, y, want]) => {
        const c = document.getElementById("term");
        const r = c.getBoundingClientRect();
        const d = c.getContext("2d").getImageData(Math.floor(x - r.left), Math.floor(y - r.top), 1, 1).data;
        return ((d[0] << 16) | (d[1] << 8) | d[2]) === want;
      },
      [x0 + first + 0.5, dotRow + 0.5, link],
    );
    assert.equal(await canvasPixel(page, x0 + first + 0.5, dotRow + 0.5), link, "under forced colours the dot is the system link colour");
    assert.equal(await canvasPixel(page, x0 + gap + 0.5, dotRow + 0.5) === link, false, "and the gaps stay gaps");
    await page.emulateMedia({ forcedColors: "none" });
    await page.waitForFunction(
      ([x, y, want]) => {
        const c = document.getElementById("term");
        const r = c.getBoundingClientRect();
        const d = c.getContext("2d").getImageData(Math.floor(x - r.left), Math.floor(y - r.top), 1, 1).data;
        return ((d[0] << 16) | (d[1] << 8) | d[2]) === want;
      },
      [x0 + first + 0.5, dotRow + 0.5, FG],
    );
  }
}

// ---- the browser build ---------------------------------------------------------------------------------------

const WEB_ROWS = () => [
  rowOf(0, ["go ", { link: "example.org/docs" }, " now"], [[3, 18, DOCS]]),
  rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, DOCS]]),
  rowOf(2, ["mail ", { link: "me@example.org" }, " please"], [[5, 18, "mailto:me@example.org"]]),
  rowOf(3, ["open ", { link: "report.txt" }, " now"], [[5, 14, "file:///home/u/report.txt"]]),
  rowOf(4, ["ssh ", { link: "ssh://host/x" }, " end"], [[4, 15, "ssh://host/x"]]),
  rowOf(5, ["mail ", { link: "click here" }, " please"], [[5, 14, "mailto:me@example.org"]]),
];
const FEATURES = "noopener,noreferrer";

/** In the browser a marked link opens a tab with `noopener,noreferrer` from the gesture, a mail address through an anchor, and file or other schemes are text. */
async function webCase(page, url) {
  const g = await open(page, url, { os: "web", rows: WEB_ROWS(), spy: true });
  const here = page.url();

  // Text equal to the address: the click opens at once, in a new tab, inside the click.
  await modClick(page, "Control", g.pt(1, 8, 0));
  let seen = await opened(page);
  assert.deepEqual(seen.map((o) => o.args), [[DOCS, "_blank", FEATURES]], "a new tab, cut off from the page");
  assert.ok(seen.every((o) => o.active), "opened from inside the gesture");
  assert.deepEqual(seen.map((o) => o.event), ["pointerup"], "in the same task as the release of the click, nothing awaited between");
  assert.equal(page.url(), here, "the page keeps its tab");
  assert.equal((await callsOf(page, "open_url")).length, 0, "the shell's open_url is not used on the web");
  await page.evaluate(() => { window.__opened.length = 0; });

  // The text differs: the question; the tab opens from inside the click on Open (and from inside Enter on Open).
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  assert.deepEqual(await opened(page), [], "nothing opens before the answer");
  await page.click(".dlg-open");
  await waitDialog(page, false);
  seen = await opened(page);
  assert.deepEqual(seen.map((o) => o.args), [[DOCS, "_blank", FEATURES]]);
  assert.ok(seen.every((o) => o.active), "opened from inside the click on Open");
  assert.deepEqual(seen.map((o) => o.event), ["click"], "in the task of the click on Open, nothing awaited between");
  await page.evaluate(() => { window.__opened.length = 0; });
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.keyboard.press("Tab");
  await page.keyboard.press("Enter");
  await waitDialog(page, false);
  seen = await opened(page);
  assert.equal(seen.length, 1);
  assert.ok(seen[0].active, "opened from inside the key on Open");
  assert.equal(seen[0].event, "keydown", "in the task of the key, nothing awaited between");
  assert.equal(page.url(), here);

  // Cancel opens nothing.
  await page.evaluate(() => { window.__opened.length = 0; });
  await gestureClick(page, g.pt(1, 5, 1));
  await waitDialog(page, true);
  await page.keyboard.press("Escape");
  await waitDialog(page, false);
  assert.deepEqual(await opened(page), []);

  // Mail: an anchor without a target, from inside the gesture; at once when the text is the address, after the question otherwise.
  const anchored = async () => (await anchors(page)).map((a) => [a.href, a.target, a.rel, a.active]);
  await gestureClick(page, g.pt(1, 8, 2));
  assert.deepEqual(await anchored(), [["mailto:me@example.org", null, "noopener noreferrer", true]]);
  await page.evaluate(() => { window.__anchors.length = 0; });
  await gestureClick(page, g.pt(1, 8, 5));
  await waitDialog(page, true);
  assert.deepEqual(await anchored(), [], "the question comes first");
  await page.click(".dlg-open");
  assert.deepEqual(await anchored(), [["mailto:me@example.org", null, "noopener noreferrer", true]]);
  assert.deepEqual(await opened(page), [], "a mail address opens no tab");

  // File and other schemes: text.
  for (const row of [3, 4]) {
    await page.mouse.move(...g.pt(1, 0, 0));
    await page.mouse.move(...g.pt(1, 7, row));
    await frames(page);
    assert.equal(await boxOf(page, "#overlay .linkon"), null, `row ${row} is plain text in the browser`);
    await gestureClick(page, g.pt(1, 7, row));
    assert.equal(await dialog(page), null);
  }
  assert.deepEqual(await opened(page), []);
  assert.equal((await callsOf(page, "open_url")).length + (await callsOf(page, "reveal_path")).length, 0);

  // Hint mode: no file link.
  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("hints.open"));
  await page.waitForFunction(() => document.querySelector("#overlay [data-hint]") !== null);
  assert.deepEqual((await hintLabels(page)).map((l) => l[0]), ["a", "b", "c", "d"], "docs, docs, mail, mail: no file link and no ssh");
  await page.keyboard.press("Escape");
}
