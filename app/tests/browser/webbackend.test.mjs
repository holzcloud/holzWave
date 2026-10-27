// The web backend of the bridge: the page has no Tauri global and talks to the server over one socket. This file runs
// on the ws backend only (the fake socket of ws-mock.js); the rest of the suite runs on both backends. The clock of the
// page is Playwright's, paused after the start, so the backoff and the 90 s of silence are played in a moment and a call
// made "while the socket is down" cannot race the first retry.
import assert from "node:assert/strict";

export const backends = ["ws"];
export const backendNote = "the subject is the socket backend itself";

/** What a call through the bridge the page itself uses comes to: "sent" when it resolved, else the rejection. */
const bridgeCall = (page, cmd, args) =>
  page.evaluate(async ([c, a]) => {
    const bridge = await import(new URL("bridge.js", location.href).href);
    return bridge.call(c, a).then(() => "sent", (e) => e);
  }, [cmd, args]);

const layoutCalls = (page) => page.evaluate(() => window.__MOCK__.callsOf("get_layout").length);
const reportOf = (page) => page.evaluate(() => window.__states.map((s) => `${s.state}${s.state === "reconnecting" ? s.attempt : ""}${s.resumed ? "+" : ""}`));
const socketCount = (page) => page.evaluate(() => window.__WS_MOCK__.urls.length);
/** Let the promises the last clock step started run to their end. */
const settled = (page) => page.evaluate(() => new Promise((done) => queueMicrotask(() => queueMicrotask(done))));

export default async function ({ page, url, errors }) {
  await page.clock.install();
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);

  assert.equal(await page.evaluate(() => typeof window.__TAURI__), "undefined", "the page must run without the Tauri global");
  assert.equal(await page.evaluate(() => typeof window.__TAURI_INTERNALS__), "undefined");

  // One socket, on the page's own host, path /ws, plain ws for the http test page.
  const urls = await page.evaluate(() => window.__WS_MOCK__.urls);
  assert.deepEqual(urls, [`ws://${new URL(url).host}/ws`]);

  // The first frame is a request with id 1 and an argument object; the layout is asked for with a later id.
  const sent = (await page.evaluate(() => window.__WS_MOCK__.sent)).map((s) => JSON.parse(s));
  assert.equal(sent[0].id, 1);
  assert.deepEqual(sent[0].args, {});
  assert.ok(sent.some((f) => f.cmd === "get_layout"));
  console.log(`web backend: first frame ${JSON.stringify(sent[0])}, address ${urls[0]}`);

  // An event of the core reaches the page as an event frame and redraws it.
  const view = await page.evaluate(() => window.__MOCK__.view());
  view.tabs[0].title = "from the wire";
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), view);
  await page.waitForFunction(() => document.body.textContent.includes("from the wire"));

  // The session event reaches onSession and never a listener.
  await page.evaluate(async () => {
    const bridge = await import(new URL("bridge.js", location.href).href);
    window.__sessions = [];
    window.__listened = [];
    window.__states = [];
    bridge.onSession((s) => window.__sessions.push(s));
    bridge.onConnection((s) => window.__states.push(s));
    void bridge.listen("hw-session", (p) => window.__listened.push(p));
  });
  await page.evaluate(() => window.__MOCK__.emit("hw-session", { state: "ok", seconds: 900 }));
  await settled(page);
  assert.deepEqual(await page.evaluate(() => window.__sessions), [{ state: "ok", seconds: 900 }]);
  assert.deepEqual(await page.evaluate(() => window.__listened), []);

  // From here on time is the test's: the clock stops one second ahead.
  const now = await page.evaluate(() => Date.now());
  await page.clock.pauseAt(new Date(now + 1000));

  // A lost connection: the page reports it, rejects a command at once, and after the retry asks for the layout once more.
  const before = await layoutCalls(page);
  await page.evaluate(() => window.__WS_MOCK__.drop(1006));
  assert.deepEqual(await reportOf(page), ["reconnecting1"]);
  const refused = await bridgeCall(page, "key", { pane: 1, event: { key: "x", while: "down" } });
  assert.equal(refused, "not connected to the server");
  await page.clock.runFor(700);
  await settled(page);
  assert.deepEqual(await reportOf(page), ["reconnecting1", "open+"]);
  assert.equal(await socketCount(page), 2);
  assert.equal(await layoutCalls(page), before + 1, "get_layout once more after the reconnect, not twice");
  await page.clock.runFor(20_000);
  await settled(page);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("key")), [], "a key pressed while down never runs");
  const frames = (await page.evaluate(() => window.__WS_MOCK__.sent)).map((s) => JSON.parse(s));
  assert.ok(!frames.some((f) => JSON.stringify(f).includes("while")), "nothing sent before the loss was sent again");
  const ids = frames.map((f) => f.id);
  assert.deepEqual(ids, [...ids].sort((a, b) => a - b), "ids only rise");
  assert.equal(new Set(ids).size, ids.length);

  // Two refused attempts: the third opens.
  await page.evaluate(() => {
    window.__states.length = 0;
    window.__WS_MOCK__.refuseOpen(2);
    window.__WS_MOCK__.drop(1006);
  });
  await page.clock.runFor(4500);
  await settled(page);
  assert.deepEqual(await reportOf(page), ["reconnecting1", "reconnecting2", "reconnecting3", "open+"]);
  assert.equal(await socketCount(page), 5);

  // A silent server: 90 s without a message and the page closes the socket itself and connects again.
  await page.evaluate(() => {
    window.__states.length = 0;
    window.__MOCK__.emit("hw-session", { state: "ok", seconds: 900 }); // the last sign of life
    window.__WS_MOCK__.silence();
  });
  await page.clock.runFor(89_000);
  assert.deepEqual(await reportOf(page), [], "alive before the 90 s are over");
  await page.clock.runFor(1100); // the 90 s end; the retry comes 0.4 to 0.6 s later
  assert.deepEqual(await reportOf(page), ["reconnecting1"]);
  await page.evaluate(() => window.__WS_MOCK__.silence(false));
  await page.clock.runFor(700);
  await settled(page);
  assert.deepEqual(await reportOf(page), ["reconnecting1", "open+"]);
  assert.equal(await socketCount(page), 6);
  assert.notEqual(await page.evaluate(() => window.__WS_MOCK__.sockets[4].readyState), 1, "the dead socket was closed");

  // The code 4401 ends the connection: no further attempt, and a command is refused with the reason.
  await page.evaluate(() => {
    window.__states.length = 0;
    window.__WS_MOCK__.drop(4401);
  });
  await page.clock.runFor(120_000);
  await settled(page);
  assert.deepEqual(await page.evaluate(() => window.__states.map((s) => [s.state, s.code])), [["ended", 4401]]);
  assert.equal(await socketCount(page), 6, "no attempt after the session ended");
  assert.equal(await bridgeCall(page, "get_layout"), "the session has ended");
  assert.deepEqual(errors, [], "the page reported errors");
}
