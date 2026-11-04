// Hyperlinks in a real browser against the real server over TLS (POL-01, D-05, D-06, D-07): a shell in a real pane prints OSC 8 links, the core
// makes spans of them, and the page shows, asks and opens as the link policy says. The popups are caught with the browser's own events, the
// page is the product's own with its shipped Content-Security-Policy, and nothing here is a switch of the product.
//
// The target host of the popups is answered by a route of the context (a page of one line, no network is used); a route that aborts instead
// would leave the popup on an error page, where `window.opener` could not be asked. The address of the popup is the address the page opened.
//
// Waiting for something that must NOT happen needs a window: NEGATIVE_MS is long enough for a popup to appear (the positive cases show the
// popup within a fraction of a second, and the file prints how long it took) and short enough to keep the file fast.
//
// With HW_SELF_CHECK=1 two deliberately wrong expectations run too and must fail.
import {
  runTest, signIn, poll, check, equal, sleep, oracle, gridOf, cellCentre, readColours, typeInto, RESET, cursorTo, marker, osc8, line,
} from "./lib.mjs";

const SELF_CHECK = process.env.HW_SELF_CHECK === "1";

/** Run an expectation that cannot hold; the runner counts the `selfcheck.fail` lines (two are due, no `selfcheck.pass`). */
async function expectFailure(name, attempt) {
  try {
    await attempt();
  } catch (e) {
    console.log(`selfcheck.fail ${name}: ${String(e.message).split("\n")[0].slice(0, 120)}`);
    return;
  }
  console.log(`selfcheck.pass ${name}`);
}

/** How long a test waits for a popup that must not come. */
const NEGATIVE_MS = 2_000;
/** The double click window of the page's pointer: two clicks closer than this are one multi-click, not two gestures. */
const DOUBLE_CLICK_MS = 450;

const TARGET_ORIGIN = "https://example.org";
const DOCS = `${TARGET_ORIGIN}/docs`;
const ASK = `${TARGET_ORIGIN}/x`;
const WRAPPED = `${TARGET_ORIGIN}/wrapped-address`;
const MARKUP = `${TARGET_ORIGIN}/<img/src=x/onerror=alert(1)>`;

await runTest("links", {}, async ({ engine, harness, open }) => {
  const person = await harness.account({ role: "admin", name: "ln-owner" });
  const owned = await open({ allow: [TARGET_ORIGIN] });
  const context = owned.context;
  await context.route(`${TARGET_ORIGIN}/**`, (route) =>
    route.fulfill({ status: 200, contentType: "text/html", body: "<!doctype html><title>link target</title><p>answered by the test</p>" }),
  );

  // The popups: every page that appears while a gesture is made and that is not one the test made itself.
  const popups = [];
  let making = false;
  context.on("page", (page) => {
    if (making) return;
    owned.foreign(page); // what happens inside a page of another site is not the application's
    popups.push(page);
  });

  const first = await context.newPage();
  await signIn(first, harness, person);
  await first.close();

  making = true;
  const page = await context.newPage();
  making = false;
  const alerts = [];
  page.on("dialog", (dialog) => {
    alerts.push(dialog.message());
    void dialog.dismiss();
  });
  await page.goto(`${harness.origin}/`);
  await page.waitForSelector("canvas#term");
  const grid = await poll(async () => {
    const g = await gridOf(page);
    return g && g.cols >= 60 && g.rows >= 24 ? g : undefined;
  }, "the page lays out a pane that is big enough", 20_000);
  const sock = await oracle(page);
  const pane = grid.pane;
  const shows = (word) =>
    poll(async () => ((await sock.screen(pane)).split("\n").some((l) => l.trim() === word) ? true : undefined), `the screen shows ${word}`, 20_000);

  // ---- what the shell prints ----------------------------------------------------------------------------------------------------------
  const ROW = { equal: 3, differs: 4, file: 5, javascript: 6, ssh: 7, data: 8, markup: 9, blank: 10, wrap: 11, plain: 14, ulink: 16, ufile: 17, uplain: 18 };
  const at = (row, text) => [cursorTo(0, row), `printf '${text}'`];
  await typeInto(
    sock,
    pane,
    line(
      RESET,
      ...at(ROW.equal, "see "), osc8(DOCS, "example.org/docs"),
      ...at(ROW.differs, "see "), osc8(ASK, "click here"),
      ...at(ROW.file, "see "), osc8("file:///etc/hosts", "file link"),
      ...at(ROW.javascript, "see "), osc8("javascript:alert(1)", "js link"),
      ...at(ROW.ssh, "see "), osc8("ssh://host/x", "ssh link"),
      ...at(ROW.data, "see "), osc8("data:text/html,x", "data link"),
      ...at(ROW.markup, "see "), osc8(MARKUP, "markup link"),
      ...at(ROW.blank, "see "), osc8("https://example.org/a b", "blank link"),
      // A link as long as its address that starts ten cells before the right edge: it wraps onto the next row.
      cursorTo(grid.cols - 10, ROW.wrap), osc8(WRAPPED, WRAPPED),
      ...at(ROW.plain, "plain text row"),
      // The same word three times: with a link, with a file link, without a link (no descender, so the bottom rows of a cell hold no glyph).
      cursorTo(4, ROW.ulink), osc8(`${TARGET_ORIGIN}/u`, "welcome"),
      cursorTo(4, ROW.ufile), osc8("file:///etc/motd", "welcome"),
      ...at(ROW.uplain, "    welcome"),
      cursorTo(0, 21), marker("links"),
    ),
  );
  await shows("hw-links");

  // ---- helpers of the cases -----------------------------------------------------------------------------------------------------------
  const point = (col, row) => cellCentre(grid, col, row);
  const hover = async (col, row) => {
    const p = point(col, row);
    await page.mouse.move(p.pageX, p.pageY);
  };
  /** The LINK segment of the status line: its text and accessible name, or null when it is not shown. */
  const segment = () =>
    page.evaluate(() => {
      const n = document.querySelector("#status .seg.link");
      if (!n || getComputedStyle(n).display === "none") return null;
      return { text: n.textContent, label: n.getAttribute("aria-label") };
    });
  const overlayCues = () => page.evaluate(() => ({ solid: document.querySelectorAll("#overlay .linkon").length, rose: document.querySelectorAll("#overlay .linkhover").length }));
  /** Hover a link and wait until the status line names its target. */
  const hoverLink = async (col, row, target) => {
    await hover(0, ROW.plain); // somewhere else first, so that the move is a change
    await hover(col, row);
    return poll(async () => {
      const seg = await segment();
      return seg?.label === `Link target ${target}` ? seg : undefined;
    }, `the LINK segment names ${target}`, 10_000);
  };
  /** Hover the link first (the segment shows), then `col, row`: the segment must go, which proves the page looked at the new cell. */
  const hoverNotALink = async (col, row, what) => {
    await hoverLink(8, ROW.equal, DOCS);
    await hover(col, row);
    await poll(async () => ((await segment()) === null ? true : undefined), `${what}: the LINK segment goes (the cell is not a link)`, 5_000);
    const cues = await overlayCues();
    check(cues.solid === 0 && cues.rose === 0, `${what}: no hover underline over the cell (${JSON.stringify(cues)})`);
  };
  const gestureClick = async (col, row, modifier) => {
    const p = point(col, row);
    await hover(col, row);
    await sleep(DOUBLE_CLICK_MS);
    await page.keyboard.down(modifier);
    await page.mouse.click(p.pageX, p.pageY);
    await page.keyboard.up(modifier);
  };
  /** The popup the last gesture opened: it has the address that was opened. Resolves with the page and the seconds it took. */
  const popupAfter = async (before, address, what) => {
    const t0 = Date.now();
    const popup = await poll(() => (popups.length > before ? popups[popups.length - 1] : undefined), `${what}: a tab opens`, 10_000);
    const took = (Date.now() - t0) / 1000;
    await poll(() => (popup.url().startsWith(TARGET_ORIGIN) ? true : undefined), `${what}: the tab navigates to the address`, 10_000);
    equal(popup.url(), new URL(address).href, `${what}: the address the tab opened`);
    await popup.waitForLoadState("domcontentloaded");
    // `window.opener` is the fact. (Playwright's `popup.opener()` names the page that started the tab even for a tab opened with `noopener`.)
    equal(await popup.evaluate(() => window.opener), null, `${what}: the tab has no opener`);
    return { popup, took };
  };
  const noPopup = async (before, what) => {
    await sleep(NEGATIVE_MS);
    equal(popups.length, before, `${what}: no tab opened within ${NEGATIVE_MS / 1000} s`);
  };
  const closePopups = async () => {
    for (const popup of popups.splice(0)) await popup.close().catch(() => {});
  };

  const report = { popups: [], gesture: [] };

  // ---- the control: plain text shows no segment; a link does --------------------------------------------------------------------------
  await hover(2, ROW.plain);
  await sleep(300);
  equal(await segment(), null, "hovering plain text shows no LINK segment");
  const seg = await hoverLink(8, ROW.equal, DOCS);
  check(seg.text.startsWith("LINK") && seg.text.includes("example.org"), `the segment says LINK and the target (${seg.text})`);
  const cues = await overlayCues();
  equal(cues.solid, 1, "the link under the pointer has its solid underline");

  // ---- a link whose text is its target: the gesture opens a tab ------------------------------------------------------------------------
  let before = popups.length;
  await hover(8, ROW.equal);
  await sleep(DOUBLE_CLICK_MS);
  await page.mouse.click(point(8, ROW.equal).pageX, point(8, ROW.equal).pageY);
  await noPopup(before, "a click without the gesture on a link whose text is its target");
  report.popups.push("click without the gesture: none");

  for (const modifier of ["Control", "Meta"]) {
    before = popups.length;
    await gestureClick(8, ROW.equal, modifier);
    try {
      const { popup, took } = await popupAfter(before, DOCS, `${modifier}+click on a link whose text is its target`);
      report.gesture.push(`${modifier}: accepted (tab after ${took.toFixed(2)} s)`);
      await popup.close();
      popups.pop();
    } catch (e) {
      if (modifier === "Control") throw e; // Ctrl is the gesture of every system but one; the other modifier is recorded, not required
      report.gesture.push(`${modifier}: not accepted (${String(e.message).split("\n")[0].slice(0, 80)})`);
    }
  }
  check(report.gesture.some((g) => g.startsWith("Control: accepted")), "Ctrl+click opens a tab");

  // ---- a link whose text differs: the question -------------------------------------------------------------------------------------------
  const dialog = () =>
    page.evaluate(() => {
      const d = document.querySelector(".dlg-link");
      if (!d) return null;
      return {
        title: d.querySelector(".dlg-title")?.textContent,
        labels: [...d.querySelectorAll(".dlg-lbl")].map((n) => n.textContent),
        shown: d.querySelector(".dlg-shown")?.textContent,
        target: d.querySelector(".dlg-target")?.textContent,
        buttons: [...d.querySelectorAll("button")].map((b) => b.textContent),
        foreign: d.querySelectorAll("img, a, script, [onerror]").length,
      };
    });
  const dialogShown = (shown) => poll(async () => ((await dialog() !== null) === shown ? true : undefined), `the question ${shown ? "is" : "is not"} on the screen`, 10_000);
  const screenHas = async (text) => (await sock.screen(pane)).includes(text);

  await hoverLink(8, ROW.differs, ASK);
  before = popups.length;
  await gestureClick(8, ROW.differs, "Control");
  await dialogShown(true);
  let asked = await dialog();
  equal(JSON.stringify([asked.title, asked.labels, asked.shown, asked.target, asked.buttons]), JSON.stringify(["Open this link?", ["The text says", "The link goes to"], "click here", ASK, ["Cancel", "Open"]]), "the question shows the text and the real target");
  await page.keyboard.type("qxqx");
  await noPopup(before, "the question is open (nothing opens before the answer)");
  // Cancel.
  await page.click(".dlg-cancel");
  await dialogShown(false);
  await noPopup(before, "Cancel");
  check(!(await screenHas("qxqx")), "the terminal received none of the keys typed while the question was open");

  // Escape.
  await gestureClick(8, ROW.differs, "Control");
  await dialogShown(true);
  await page.keyboard.press("Escape");
  await dialogShown(false);
  await noPopup(before, "Escape");
  report.popups.push("differing text: Cancel none, Escape none");

  // Open.
  await gestureClick(8, ROW.differs, "Control");
  await dialogShown(true);
  await page.click(".dlg-open");
  const confirmed = await popupAfter(before, ASK, "Open in the question");
  report.popups.push(`differing text: Open -> tab after ${confirmed.took.toFixed(2)} s, no opener`);
  await dialogShown(false);
  await confirmed.popup.close();
  popups.pop();

  // The control for the typed keys: with the question gone the same keys reach the shell.
  await page.locator("canvas#term").click();
  await page.keyboard.type("qxqx");
  await poll(async () => ((await screenHas("qxqx")) ? true : undefined), "keys typed with no question open reach the shell", 10_000);
  await page.keyboard.press("Control+U");

  // ---- addresses that are not links in the browser ---------------------------------------------------------------------------------
  const notLinks = [
    ["file:///etc/hosts", ROW.file],
    ["javascript:alert(1)", ROW.javascript],
    ["ssh://host/x", ROW.ssh],
    ["data:text/html,x", ROW.data],
    ["an address with a blank", ROW.blank],
  ];
  before = popups.length;
  for (const [what, row] of notLinks) {
    await hoverNotALink(6, row, what);
    await gestureClick(6, row, "Control");
    await gestureClick(6, row, "Meta");
  }
  await noPopup(before, "a gesture click on file, javascript, ssh, data and blank addresses");
  report.popups.push(`not links (file, javascript, ssh, data, blank): hover shows nothing, ${notLinks.length * 2} gesture clicks, no tab`);

  // The dotted underline at rest: with a link, the cells have dots; without one they do not. The file link is recorded, not required.
  const inkOf = async (row) => {
    const y = grid.y + (row + 1) * grid.cellH - 1.5; // the row of device pixels the dots are on (two above the cell's bottom edge)
    const pts = [];
    for (let x = grid.x + 4 * grid.cellW; x < grid.x + 11 * grid.cellW; x++) pts.push({ x, y });
    const colours = await readColours(page, pts);
    const background = colours[0];
    return colours.filter((c) => c !== background).length;
  };
  const dots = { link: await inkOf(ROW.ulink), file: await inkOf(ROW.ufile), plain: await inkOf(ROW.uplain) };
  check(dots.link > 0, `a link has dots under its cells (${JSON.stringify(dots)})`);
  equal(dots.plain, 0, "text with no link has no dots (the control)");
  report.popups.push(`dots at rest: link ${dots.link}, file link ${dots.file}, plain ${dots.plain} pixels`);

  // ---- markup in an address ----------------------------------------------------------------------------------------------------------------
  const hostile = await hoverLink(6, ROW.markup, MARKUP);
  check(hostile.text.includes("<img"), "the status line shows the address as text");
  const created = await page.evaluate(() => document.querySelectorAll("img, [onerror]").length);
  equal(created, 0, "no img element or onerror attribute exists in the page");
  before = popups.length;
  await gestureClick(6, ROW.markup, "Control");
  await dialogShown(true);
  asked = await dialog();
  equal(asked.target, MARKUP, "the question shows the address as text");
  equal(asked.shown, "markup link", "the question shows the text as text");
  equal(asked.foreign, 0, "the question holds no element made from the address");
  equal(await page.evaluate(() => document.querySelectorAll("img, [onerror]").length), 0, "no img element or onerror attribute exists with the question open");
  await page.keyboard.press("Escape");
  await dialogShown(false);
  equal(alerts.length, 0, "no script of the address ran");
  await noPopup(before, "Escape on the markup address");
  report.popups.push("markup address: shown as text in the status line and the question, no element, no script, nothing opened");

  // ---- a link that wraps is two links ---------------------------------------------------------------------------------------------
  const firstPart = grid.cols - 5;
  await hoverLink(firstPart, ROW.wrap, WRAPPED);
  const topCues = await overlayCues();
  await hoverLink(5, ROW.wrap + 1, WRAPPED);
  const bottomCues = await overlayCues();
  equal(topCues.solid, 1, "the first row's part is underlined");
  equal(bottomCues.solid, 1, "only the second row's part is underlined while the pointer is on it");
  // Each row's part is a link of its own with its own text: the text of the second part is not the whole address, so the page asks.
  before = popups.length;
  await gestureClick(5, ROW.wrap + 1, "Control");
  await dialogShown(true);
  asked = await dialog();
  equal(asked.target, WRAPPED, "the question names the whole address");
  check(asked.shown !== WRAPPED && WRAPPED.endsWith(asked.shown), `the question shows the text of the part that was clicked (${asked.shown})`);
  await noPopup(before, "the question about the wrapped link's second part");
  await page.click(".dlg-open");
  const wrapped = await popupAfter(before, WRAPPED, "Open for the second row of a wrapped link");
  report.popups.push(`wrapped link: hover on the second row names the target; click asks about the part ("${asked.shown}"), Open -> tab after ${wrapped.took.toFixed(2)} s`);
  await dialogShown(false);
  await wrapped.popup.close();
  popups.pop();

  if (SELF_CHECK) {
    // Two expectations that cannot hold. Each must fail; the runner counts the lines.
    await expectFailure("a plain text row shows a LINK segment", async () => {
      await hover(0, ROW.equal);
      await hover(2, ROW.plain);
      await poll(async () => ((await segment()) !== null ? true : undefined), "the LINK segment shows over plain text", 2_500);
    });
    await expectFailure("a gesture click on a file link opens a tab", async () => {
      const count = popups.length;
      await gestureClick(6, ROW.file, "Control");
      await poll(() => (popups.length > count ? true : undefined), "a tab opens for a file link", 2_500);
    });
  }

  await closePopups();
  owned.expectClean("the context of the links test");
  equal(alerts.length, 0, "no script ran");
  console.log(`links [${engine}]: renderer ${grid.renderer}; Gesture: ${report.gesture.join(", ")}; Popups: ${report.popups.join(" | ")}`);
});
