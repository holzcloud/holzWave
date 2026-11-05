// The status line names the address that is under the pointer (docs/design.md 6.31) even when the program rewrites it under a pointer
// that does not move: a frame that replaces the hovered row makes the page look again (a changed address, a link that goes, a link
// that appears), a frame that leaves the row as it was costs nothing, and a frame of a pane the pointer is not over costs nothing.
// The looks are counted through the page's one read of the canvas box that every look makes.
import assert from "node:assert/strict";
import { emit } from "./lib.mjs";
import { DOCS, boxOf, frame, frames, linkSeg, open, rowOf, step, waitSeg } from "./osclinks.test.mjs";

const OTHER = "https://example.org/other";

/** Count the looks the page takes at the row under the pointer: each one reads the box of the canvas. */
const countLooks = (page) =>
  page.evaluate(() => {
    window.__looks = 0;
    const original = Element.prototype.getBoundingClientRect;
    Element.prototype.getBoundingClientRect = function (...args) {
      if (this.id === "term") window.__looks += 1;
      return original.apply(this, args);
    };
  });
const looks = (page) => page.evaluate(() => window.__looks);
/** Mark the element of the solid underline, so a rebuilt box shows as another one. */
const markBox = (page) => page.evaluate(() => { document.querySelector("#overlay .linkon").dataset.mark = "kept"; });
const marked = (page) => page.evaluate(() => document.querySelector("#overlay .linkon")?.dataset.mark ?? null);

/** The `emit` of a frame that sends `rows` for `pane`; the sequence number goes up by one each time. */
function sender(page) {
  const seq = { 1: 1, 2: 1 };
  return (pane, rows) => emit(page, "frame", frame(pane, ++seq[pane], rows, { cols: 50, rows: 8, full: false }));
}

const waitTarget = (page, address) =>
  page.waitForFunction((want) => document.querySelector("#status .seg.link")?.getAttribute("aria-label") === `Link target ${want}`, address);

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await step("the address follows", () => followCase(page, url));
  await step("the gesture", () => gestureCase(page, url));
  await step("nothing to look at", () => quietCase(page, url));
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A changed address, a link that goes and a link that appears, with the pointer still. */
async function followCase(page, url) {
  const g = await open(page, url);
  const send = sender(page);
  await page.mouse.move(...g.pt(1, 5, 1));
  await waitSeg(page, true);
  assert.equal((await linkSeg(page)).label, `Link target ${DOCS}`);

  // The program changes the address under the same cells.
  await send(1, [rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, OTHER]])]);
  await waitTarget(page, OTHER);
  const seg = await linkSeg(page);
  assert.equal(seg.text, `LINK${OTHER}`, "the segment shows the new address");
  assert.deepEqual(seg.kids.map((k) => k[1]), ["LINK", "https://example.org", "/other"]);
  assert.notEqual(await boxOf(page, "#overlay .linkon"), null, "the solid underline stays under the pointer");

  // The link goes: the segment and the underline with it.
  await send(1, [rowOf(1, ["see docs here"])]);
  await waitSeg(page, false);
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") === null);

  // A link appears under a pointer that rests on plain text.
  await send(1, [rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, DOCS]])]);
  await waitSeg(page, true);
  assert.equal((await linkSeg(page)).label, `Link target ${DOCS}`);
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") !== null);
}

/** With the open gesture held the rose underline and the address follow as well. */
async function gestureCase(page, url) {
  const g = await open(page, url);
  const send = sender(page);
  await page.mouse.move(...g.pt(1, 5, 1));
  await page.keyboard.down("Meta");
  await page.waitForFunction(() => document.querySelector("#overlay .linkhover") !== null);
  await send(1, [rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, OTHER]])]);
  await waitTarget(page, OTHER);
  assert.notEqual(await boxOf(page, "#overlay .linkhover"), null, "the rose underline stays while the gesture is held");
  await send(1, [rowOf(1, ["see docs here"])]);
  await waitSeg(page, false);
  await page.waitForFunction(() => document.querySelector("#overlay .linkhover") === null);
  await page.keyboard.up("Meta");
}

/** A frame that leaves the row as it was, a frame of the other pane and a pointer outside the canvas cost nothing. */
async function quietCase(page, url) {
  const g = await open(page, url);
  const send = sender(page);
  await page.mouse.move(...g.pt(1, 5, 1));
  await waitSeg(page, true);
  await page.waitForFunction(() => document.querySelector("#overlay .linkon") !== null);
  await markBox(page);
  await frames(page, 2);
  await countLooks(page);

  // The control: a frame that replaces the hovered row is looked at.
  await send(1, [rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, OTHER]])]);
  await waitTarget(page, OTHER);
  assert.ok((await looks(page)) >= 1, "the control: the page looked at the replaced row");
  await send(1, [rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, DOCS]])]);
  await waitTarget(page, DOCS);
  await markBox(page);

  // Twenty frames that change other rows: no look, and the box is the one that was there.
  await page.evaluate(() => { window.__looks = 0; });
  for (let i = 0; i < 20; i++) await send(1, [rowOf(0, [`plain text row ${i}`])]);
  await frames(page, 3);
  assert.equal(await looks(page), 0, "twenty frames that change other rows cost no look");
  assert.equal(await marked(page), "kept", "the underline was not rebuilt");
  assert.equal((await linkSeg(page)).label, `Link target ${DOCS}`);

  // Frames of the pane the pointer is not over, even for the same row.
  for (let i = 0; i < 5; i++) await send(2, [rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, `${OTHER}/${i}`]])]);
  await frames(page, 3);
  assert.equal(await looks(page), 0, "a frame of another pane costs no look");
  assert.equal((await linkSeg(page)).label, `Link target ${DOCS}`, "the address is the one under the pointer");

  // The pointer has left the canvas: a frame gives no hover.
  await page.mouse.move(500, 635);
  await waitSeg(page, false);
  await page.evaluate(() => { window.__looks = 0; });
  await send(1, [rowOf(1, ["see ", { link: "docs" }, " here"], [[4, 7, OTHER]])]);
  await frames(page, 3);
  assert.equal(await looks(page), 0, "no look while the pointer is outside the canvas");
  assert.equal(await linkSeg(page), null, "no segment for a pointer that is not over a link");
  assert.equal(await boxOf(page, "#overlay .linkon"), null);
}
