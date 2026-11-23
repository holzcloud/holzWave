// The lasting state `Not sent` in the header of a remote pane (docs/design.md 6.34): alone and after the word of another state, as a
// watcher, when it goes, and its second cue (the mark ! and the underline) under forced colours and higher contrast on the desktop, the
// tablet and the phone. The stub plays the host: its `remoteSessions` hook puts the remote member, with `not_sent`, into the answer of
// `pane_connections`. A file of its own beside serverpane-states, which holds the other states, so that each stays inside the time
// a test file has.
import assert from "node:assert/strict";
import { currentBackend } from "./lib.mjs";
import { open as openPhone } from "./touch-lib.mjs";

// Both builds show a session in a pane with the same states. The phone and the tablet are the Android app's layouts (the browser build
// has neither), so those cases run on the desktop backend only.
export const backends = ["tauri", "ws"];

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await notSentCase(page, url);
  if (currentBackend() === "tauri") {
    await phoneCase(page, url);
    await tabletCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

const head = (page, pane) => page.locator(`.panehead[data-pane="${pane}"]`);

async function open(page, url, panes) {
  await page.addInitScript((p) => {
    const sessions = window.__MOCK__.remoteSessions;
    sessions.reset();
    for (const [pane, partial] of Object.entries(p)) sessions.setPane(Number(pane), partial);
  }, panes);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

const setPane = (page, pane, partial) => page.evaluate(([p, x]) => window.__MOCK__.remoteSessions.setPane(p, x), [pane, partial]);
const ctxIs = (page, pane, text) =>
  page.waitForFunction(([p, t]) => document.querySelector(`.panehead[data-pane="${p}"] .ph-ctx`)?.textContent === t, [pane, text], { timeout: 4000 });

/** The size of the tile of `pane` in cells, as the page told the core. */
const tile = (page, pane) =>
  page.evaluate(async (id) => (await import(new URL("state.js", location.href).href)).state.sizes.get(id), pane);

/** What the header of `pane` shows: the context word, its tone, its mark and underline, the writer label and the button after it. */
const lookAt = (page, pane) =>
  head(page, pane).evaluate((h) => ({
    ctx: h.querySelector(".ph-ctx").textContent,
    tone: h.querySelector(".ph-ctx").dataset.tone ?? null,
    label: h.querySelector(".ph-writer")?.textContent ?? null,
    take: h.querySelector(".ph-take")?.textContent ?? null,
    mark: getComputedStyle(h.querySelector(".ph-ctx > span"), "::before").content,
    underline: getComputedStyle(h.querySelector(".ph-ctx")).textDecorationLine,
    dot: h.querySelector(".ph-dot").dataset.state,
  }));

/** A pane that could not send a key says `Not sent` in the header, alone and after the word of another state (docs/design.md 6.34). */
async function notSentCase(page, url) {
  await open(page, url, { 2: { remote: { writer: "writing", cols: 80, rows: 24 } } });
  const sizes = await tile(page, 2);
  await setPane(page, 2, { remote: { cols: sizes.cols, rows: sizes.rows } });
  await ctxIs(page, 2, "sync.example.test");
  // The mock's default member says false; a member without the key reads as false too.
  assert.equal((await lookAt(page, 2)).tone, null, "no word before a key was dropped");

  // A writer that could not send: the word alone, warn, with the mark and the underline; the label stays.
  await setPane(page, 2, { remote: { not_sent: true } });
  await ctxIs(page, 2, "Not sent");
  let got = await lookAt(page, 2);
  assert.equal(got.tone, "warn");
  assert.equal(got.label, "WRITING", "the writer label stays");
  assert.match(got.mark, /!/, "the mark !");
  assert.equal(got.underline, "underline", "the underline is the second cue");
  assert.notEqual(got.dot, "wait", "the dot is as before");
  assert.equal(await head(page, 2).locator(".ph-ctx").getAttribute("title"), "Not sent");

  // After the word of another state: that word, then ` · Not sent`, warn, with that state's label rule.
  const after = [
    [{ state: "reconnecting", attempt: 1, of: 5, secs: 2, remote: { phase: "" } }, "Reconnecting… · Not sent", null, "wait"],
    [{ state: "failed", remote: { phase: "" } }, "Server unreachable · Not sent", null, "failed"],
    [{ state: "connected", remote: { phase: "attaching" } }, "Attaching… · Not sent", null, "wait"],
    [{ state: "connected", remote: { phase: "input_dropped" } }, "Input dropped · Not sent", "WRITING", null],
    [{ state: "connected", remote: { phase: "" } }, "Not sent", "WRITING", null],
  ];
  for (const [partial, word, label, dot] of after) {
    await setPane(page, 2, partial);
    await ctxIs(page, 2, word);
    got = await lookAt(page, 2);
    assert.equal(got.tone, "warn", `${word}: warn`);
    assert.equal(got.label, label, `${word}: the label rule of the state`);
    assert.match(got.mark, /!/, `${word}: the mark`);
    assert.equal(got.underline, "underline", `${word}: the underline`);
    if (dot === "wait" || dot === "failed") assert.equal(got.dot, dot, `${word}: the dot of the state`);
  }

  // A watcher that typed: WATCHING, Take over and the word.
  await setPane(page, 2, { remote: { writer: "watching", not_sent: true } });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-writer')?.textContent === "WATCHING");
  await ctxIs(page, 2, "Not sent");
  got = await lookAt(page, 2);
  assert.equal(got.label, "WATCHING");
  assert.equal(got.take, "Take over");
  assert.equal(got.tone, "warn");

  // The word goes when the view no longer says it, and a member without it reads as false.
  await setPane(page, 2, { remote: { not_sent: false } });
  await ctxIs(page, 2, "sync.example.test");
  assert.equal((await lookAt(page, 2)).tone, null, "the warn tone goes with the word");
  await setPane(page, 2, { remote: { not_sent: true } });
  await ctxIs(page, 2, "Not sent");
  await setPane(page, 2, { remote: { not_sent: undefined } });
  await ctxIs(page, 2, "sync.example.test");
  await setPane(page, 2, { remote: { not_sent: "yes" } });
  await ctxIs(page, 2, "sync.example.test");
  assert.equal((await lookAt(page, 2)).tone, null, "a member that is not a boolean reads as false");

  // Forced colours and higher contrast keep the second cue: the mark ! and the underline stay, alone and after another word.
  for (const [partial, word] of [
    [{ state: "connected", remote: { phase: "", not_sent: true } }, "Not sent"],
    [{ state: "reconnecting", remote: { phase: "", not_sent: true } }, "Reconnecting… · Not sent"],
  ]) {
    await setPane(page, 2, partial);
    await ctxIs(page, 2, word);
    for (const media of [{ forcedColors: "active" }, { contrast: "more" }, { forcedColors: "active", contrast: "more" }, { reducedMotion: "reduce" }]) {
      await page.emulateMedia(media);
      got = await lookAt(page, 2);
      assert.equal(got.ctx, word, `${word} under ${JSON.stringify(media)}`);
      assert.match(got.mark, /!/, `${word}: the mark ! under ${JSON.stringify(media)}`);
      assert.equal(got.underline, "underline", `${word}: the underline under ${JSON.stringify(media)}`);
    }
    await page.emulateMedia({ forcedColors: "none", contrast: "no-preference", reducedMotion: "no-preference" });
  }
}


/** `Not sent` in the header of `pane`: said in the context slot, warn with the mark and the underline, whole inside the header, nothing overflows. */
async function wordFits(page, pane, where) {
  await setPane(page, pane, { remote: { not_sent: true } });
  await ctxIs(page, pane, "Not sent");
  const word = await head(page, pane).evaluate((h) => {
    const ctx = h.querySelector(".ph-ctx");
    const box = ctx.getBoundingClientRect();
    const root = h.getBoundingClientRect();
    return {
      tone: ctx.dataset.tone,
      mark: getComputedStyle(ctx.firstElementChild, "::before").content,
      underline: getComputedStyle(ctx).textDecorationLine,
      shown: box.width > 0 && box.height > 0 && box.left >= root.left - 0.5 && box.right <= root.right + 0.5 && ctx.scrollWidth <= ctx.clientWidth + 1,
      overflow: document.scrollingElement.scrollWidth > innerWidth,
    };
  });
  assert.equal(word.tone, "warn", where);
  assert.match(word.mark, /!/, where);
  assert.equal(word.underline, "underline", where);
  assert.ok(word.shown && !word.overflow, `the word fits the header of the pane on ${where} ${JSON.stringify(word)}`);
  for (const media of [{ forcedColors: "active" }, { contrast: "more" }]) {
    await page.emulateMedia(media);
    const got = await lookAt(page, pane);
    assert.equal(got.ctx, "Not sent", `${where} under ${JSON.stringify(media)}`);
    assert.match(got.mark, /!/, `${where}: the mark under ${JSON.stringify(media)}`);
    assert.equal(got.underline, "underline", `${where}: the underline under ${JSON.stringify(media)}`);
  }
  await page.emulateMedia({ forcedColors: "none", contrast: "no-preference" });
}

/** The tablet: the headers of all panes stay (44 px), and the word is said in the focused one. */
async function tabletCase(page, url) {
  await openPhone(page, url, { width: 1280, height: 800, screen: [1280, 800], shown: 3 });
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains("layout-tablet")), true);
  const focus = await page.evaluate(() => window.__MOCK__.view().focus);
  await setPane(page, focus, { remote: { writer: "watching" } });
  await page.waitForFunction(() => document.querySelector(".ph-writer")?.textContent === "WATCHING");
  await wordFits(page, focus, "a tablet");
}

async function phoneCase(page, url) {
  await openPhone(page, url, { width: 360, height: 760, screen: [360, 760] });
  const focus = await page.evaluate(() => window.__MOCK__.view().focus);
  await setPane(page, focus, { remote: { writer: "watching" } });
  await page.waitForFunction(() => document.querySelector("#phone-chip .ph-writer")?.textContent === "WATCHING");
  // The word is where the other states' words are on a phone: the context of the pane's header.
  await wordFits(page, focus, "a phone");
  // The word goes with the member; the slot says the server again (with the session's size, which differs from this tile).
  await setPane(page, focus, { remote: { not_sent: false } });
  await page.waitForFunction((p) => document.querySelector(`.panehead[data-pane="${p}"] .ph-ctx`)?.textContent.startsWith("sync.example.test"), focus);
  assert.equal((await lookAt(page, focus)).tone, null, "the warn tone goes with the word");
}
