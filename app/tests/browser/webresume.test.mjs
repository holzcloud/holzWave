// A browser tab that outlives its worker: the socket opens again after a loss, and the page has to treat whatever worker
// answers as one it has never met. The scripted core plays the worker. Before the restart it is the old one: four panes at
// view number 40, pane 2 watching a session of the server. A blip (close 1006) reaches the same worker again; a restart
// (close 1001) reaches a new one, which numbers its views from 1, has started no pane until the page sends `start_pane`, and
// lists the restored remote pane as ended once it is started, with the host's last words on its screen. A reload is the
// reference: the open tab has to end up where a reload would. Nothing else of the old worker may block the new one: a pane that
// asked for older lines asks again, and a pane that waited for a full frame takes the new worker's frames from sequence 1.
import assert from "node:assert/strict";

export const backends = ["ws"];
export const backendNote = "the subject is the resumed socket; the desktop never loses its core";

const head = (page, pane) => page.locator(`.panehead[data-pane="${pane}"]`);
const callsOf = (page, cmd) => page.evaluate((c) => window.__MOCK__.callsOf(c), cmd);
const sentCount = (page) => page.evaluate(() => window.__WS_MOCK__.sent.length);
const panesOf = (calls) => calls.map((a) => a.pane).sort((a, b) => a - b);
/** The commands the page sent since `from` frames, as names, in order. */
const sentSince = (page, from) => page.evaluate((n) => window.__WS_MOCK__.sent.slice(n).map((s) => JSON.parse(s).cmd), from);
const emitFrame = (page, frame) => page.evaluate((f) => window.__MOCK__.emit("frame", f), frame);
/** Wait until `fn(arg)` holds in the page; a timeout names what was awaited. */
async function until(page, what, fn, arg) {
  try {
    await page.waitForFunction(fn, arg);
  } catch (e) {
    throw new Error(`${what}: ${e.message.split("\n")[0]}`);
  }
}
/** Wait for `ms` of real time, so a call that should not come has had the time to come. */
const quiet = (page, ms) => page.waitForTimeout(ms);

/** A frame of `pane`: `text` on its first row, scrolled to `offset` of `total` history rows. */
const frameOf = (pane, seq, { full = true, text = "", offset = 0, total = 5 } = {}) => ({
  pane, seq, full, cols: 40, rows: 5, fg: 0xf6efe6, bg: 0x101010, cursor: null, scrollbar: [offset, total, 5], selection: [], matches: [],
  search: null, copy_cursor: null, modes: { app_cursor: false, app_keypad: false, bracketed_paste: false, mouse: false },
  changed: text === "" ? [] : [{ y: 0, runs: [[text.padEnd(40), 40, 0xf6efe6, 0x101010, 0]] }],
});

/** A layout of one tab with `ids` side by side, numbered `rev`. */
const viewOf = (rev, title, ids, focus) => ({
  rev,
  workspaces: [{ name: "main", active: true, panes: ids.length, pane_ids: ids }],
  tabs: [{ title, active: true, focus, panes: ids.length, pane_ids: ids }],
  focus,
  panes: ids.map((id, i) => [id, { x: i / ids.length, y: 0, w: 1 / ids.length, h: 1 }]),
  dividers: [],
  pane_names: [],
  windows: 1,
});

const LAST_WORDS = "[holzWave] exited with status 255.";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await page.addInitScript(
    ([first, lastWords]) => {
      const M = window.__MOCK__;
      // The worker: its generation (2 after the restart), the panes it started, and whether the session has older lines.
      const W = (window.__WORKER__ = { gen: 1, started: new Set(), more: true });
      const conn = (pane, state) => ({
        pane, profile: "", name: "This server", state, attempt: 0, of: 0, secs: 0,
        remote: { writer: "watching", session: 7, cols: 80, rows: 24, more: W.more, phase: "" },
      });
      M.setView(first);
      M.on("start_pane", (a) => {
        W.started.add(a.pane);
        return null;
      });
      // The new worker answers a full frame from sequence 1; the ended remote pane holds the host's last words.
      M.on("request_full", (a) => {
        if (W.gen !== 2) return null;
        const text = a.pane === 2 ? lastWords : "";
        void M.emit("frame", {
          pane: a.pane, seq: 1, full: true, cols: 40, rows: 5, fg: 0xf6efe6, bg: 0x101010, cursor: null, scrollbar: [0, 5, 5],
          selection: [], matches: [], search: null, copy_cursor: null,
          modes: { app_cursor: false, app_keypad: false, bracketed_paste: false, mouse: false },
          changed: text === "" ? [] : [{ y: 0, runs: [[text.padEnd(40), 40, 0xf6efe6, 0x101010, 0]] }],
        });
        return null;
      });
      // The old worker runs the session in pane 2; the new one lists the pane only once it was started, and then as ended.
      M.on("pane_connections", () => (W.gen === 1 ? [conn(2, "connected")] : W.started.has(2) ? [conn(2, "closed")] : []));
      M.on("pane_load_older", () => null);
    },
    [viewOf(40, "before the restart", [1, 2, 3, 4], 2), LAST_WORDS],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await until(page, "the first start of the four panes", () => window.__MOCK__.callsOf("start_pane").length >= 4);
  await page.evaluate(async () => {
    window.__state = (await import(new URL("state.js", location.href).href)).state;
  });
  await until(page, "pane 2 watching", () => document.querySelector('.panehead[data-pane="2"] .ph-writer')?.textContent === "WATCHING");
  assert.ok(await page.evaluate(() => document.body.textContent.includes("before the restart")));
  for (const pane of [1, 2, 3, 4]) await emitFrame(page, frameOf(pane, 1, { text: `old screen ${pane}` }));
  await until(page, "the old screens", () => [1, 2, 3, 4].every((p) => window.__state.frames.get(p)?.seq === 1));

  await aBlipReachesTheSameWorker(page);
  await aRestartReachesANewWorker(page);
  assert.deepEqual(errors, [], "the page reported errors");
}

/**
 * The same worker after a lost connection: one more `start_pane` and `request_full` per pane, which the core takes as nothing new
 * for a running pane, and nothing else again. The headers stay as they were.
 */
async function aBlipReachesTheSameWorker(page) {
  // Pane 2 is scrolled to the top of a session with older lines: it asks once, and a held scroll does not ask again. The answer
  // is lost with the connection, so the core never shows a phase that would open the latch.
  await emitFrame(page, frameOf(2, 2, { full: false, total: 100 }));
  await emitFrame(page, frameOf(2, 3, { full: false, total: 100 }));
  await until(page, "the frames of pane 2 at the top", () => window.__state.frames.get(2)?.seq === 3);
  assert.equal((await callsOf(page, "pane_load_older")).length, 1, "the pane at the top asks for older lines once");

  const starts = (await callsOf(page, "start_pane")).length;
  const fulls = (await callsOf(page, "request_full")).length;
  const layouts = (await callsOf(page, "get_layout")).length;
  await page.evaluate((v) => window.__MOCK__.setView(v), viewOf(41, "before the restart", [1, 2, 3, 4], 2));
  const from = await sentCount(page);
  await page.evaluate(() => window.__WS_MOCK__.drop(1006));
  await until(page, "request_full again for the four panes after a blip", (n) => window.__MOCK__.callsOf("request_full").length >= n + 4, fulls);
  await quiet(page, 1200);
  assert.equal((await callsOf(page, "get_layout")).length, layouts + 1, "the layout is asked for once");
  assert.deepEqual(panesOf((await callsOf(page, "start_pane")).slice(starts)), [1, 2, 3, 4], "start_pane once more per pane");
  assert.deepEqual(panesOf((await callsOf(page, "request_full")).slice(fulls)), [1, 2, 3, 4], "request_full once more per pane");
  // Nothing else is sent again: what follows the reconnect is the layout, the starts, the full frames, the polls that run anyway
  // and the animation setting of pictures, which the picture store tells every socket that opens again on its own.
  const again = new Set(await sentSince(page, from));
  for (const cmd of ["get_layout", "start_pane", "request_full", "pane_connections", "integration_context", "set_image_animation"]) {
    again.delete(cmd);
  }
  assert.deepEqual([...again], [], "nothing else is sent again after a blip");
  for (const pane of [1, 2, 3, 4]) assert.equal(await head(page, pane).count(), 1, `pane ${pane} has one header`);
  assert.equal(await head(page, 2).locator(".ph-writer").textContent(), "WATCHING", "the header is as before");
  assert.deepEqual(await page.evaluate(() => [1, 2, 3, 4].map((p) => window.__state.frames.get(p)?.seq)), [1, 3, 1, 1], "the screens stay");

  // After the reconnect the pane at the top of a session with older lines asks again, once; a held scroll still asks once.
  await emitFrame(page, frameOf(2, 4, { full: false, total: 100 }));
  await until(page, "older lines asked for again", () => window.__MOCK__.callsOf("pane_load_older").length === 2);
  await emitFrame(page, frameOf(2, 5, { full: false, total: 100 }));
  await until(page, "the next frame of pane 2", () => window.__state.frames.get(2)?.seq === 5);
  assert.equal((await callsOf(page, "pane_load_older")).length, 2, "a held scroll asks once");
}

/**
 * A new worker after a restart of the server: its view has a lower number and is shown, every pane of it is started there (also
 * one the old worker had reported as exited), a pane that exited and is not in the view stays forgotten, the remote pane says
 * that its session ended, and a key reaches the new worker.
 */
async function aRestartReachesANewWorker(page) {
  // The old worker ends two panes and drops one of them from its layout; pane 4 stays in the layout it saved.
  await page.evaluate(() => {
    window.__MOCK__.emit("pane-exit", { pane: 3, code: 0 });
    window.__MOCK__.emit("pane-exit", { pane: 4, code: 0 });
  });
  await until(page, "the exits of panes 3 and 4", () => window.__state.exited.has(3) && window.__state.exited.has(4));
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), viewOf(42, "before the restart", [1, 2], 2));
  await until(page, "the old worker's view 42", () => window.__state.view?.rev === 42);

  // Pane 1 sees a gap and waits for a full frame that the old worker never sends.
  const asked = (await callsOf(page, "request_full")).length;
  await emitFrame(page, frameOf(1, 9, { full: false }));
  await until(page, "the full frame asked for after the gap", (n) => window.__MOCK__.callsOf("request_full").length === n + 1, asked);

  const starts = (await callsOf(page, "start_pane")).length;
  const fulls = (await callsOf(page, "request_full")).length;
  await page.evaluate((v) => {
    window.__WS_MOCK__.drop(1001);
    window.__WORKER__.gen = 2;
    window.__WORKER__.started.clear();
    window.__MOCK__.setView(v);
  }, viewOf(2, "after the restart", [1, 2, 4], 1));
  await until(page, "the new worker's view number 2 drawn", () => document.body.textContent.includes("after the restart"));
  assert.equal(await page.evaluate(() => window.__state.view?.rev), 2, "the new worker's view is shown although its number is lower");
  await until(page, "request_full for the three panes of the new worker", (n) => window.__MOCK__.callsOf("request_full").length >= n + 3, fulls);
  await quiet(page, 300);
  assert.deepEqual(panesOf((await callsOf(page, "start_pane")).slice(starts)), [1, 2, 4], "every pane of the new view is started, the exited pane 3 is not");
  assert.deepEqual(panesOf((await callsOf(page, "request_full")).slice(fulls)), [1, 2, 4]);

  // The remote pane says its session ended, with the mark and the underline, and shows the host's last words.
  await until(page, "pane 2 says Session ended", () => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Session ended");
  const look = await head(page, 2).evaluate((h) => ({
    tone: h.querySelector(".ph-ctx").dataset.tone ?? null,
    underline: getComputedStyle(h.querySelector(".ph-ctx")).textDecorationLine,
    mark: getComputedStyle(h.querySelector(".ph-ctx > span"), "::before").content,
    dot: h.querySelector(".ph-dot").dataset.state,
    writer: h.querySelector(".ph-writer")?.textContent ?? null,
  }));
  assert.equal(look.tone, "warn");
  assert.match(look.underline, /underline/);
  assert.match(look.mark, /!/);
  assert.equal(look.dot, "failed");
  assert.equal(look.writer, null, "no writer label after the end");
  await until(
    page,
    "the host's last words in pane 2",
    (words) => window.__state.frames.get(2)?.seq === 1 && window.__state.frames.get(2).rows_[0]?.runs[0][0].startsWith(words),
    LAST_WORDS,
  );
  assert.equal(await page.evaluate(() => window.__state.frames.get(1)?.seq), 1, "the new worker's full frame replaced the old screen");
  // The new worker counts its frames from 1: the pane that waited takes the next one, it does not wait for ever.
  await emitFrame(page, frameOf(1, 2, { full: false, text: "new worker" }));
  await until(page, "the new worker's next frame of pane 1", () => window.__state.frames.get(1)?.seq === 2);
  assert.equal(await head(page, 3).count(), 0, "the exited pane stays forgotten");

  // A key in the focused plain pane reaches the new worker.
  await page.locator("#term").focus();
  await page.keyboard.press("x");
  await until(page, "a key for pane 1", () => window.__MOCK__.callsOf("key").some((a) => a.pane === 1 && a.key === "x" && !a.release));
}
