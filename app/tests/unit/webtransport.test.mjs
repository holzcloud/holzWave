// The web transport against a fake socket, a manual clock and a seeded random: validation of everything the server
// sends, the first connection, reconnect with backoff, no replay, the ended codes, silence, sizes, and the address.
// Runs on dist/ like the other unit tests; no real timer, no real socket.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  BACKOFF_FIRST_MS, BACKOFF_MAX_MS, CLOSE_REFUSED, CLOSE_SESSION_ENDED, FIRST_CONNECT_MS, JITTER, MAX_MESSAGE_BYTES,
  MAX_REQUEST_BYTES, SILENCE_MS, createTransport, socketUrl,
} from "../../dist/webtransport.js";

/** A manual clock: timers run in order of their due time when `advance` passes them. */
function clock() {
  let now = 1_000;
  let nextHandle = 1;
  const timers = new Map();
  return {
    now: () => now,
    setTimer(fn, ms) {
      const h = nextHandle++;
      timers.set(h, { at: now + ms, fn });
      return h;
    },
    clearTimer: (h) => void timers.delete(h),
    advance(ms) {
      const end = now + ms;
      for (;;) {
        let due = null;
        for (const [h, t] of timers) if (t.at <= end && (due === null || t.at < due[1].at)) due = [h, t];
        if (due === null) break;
        timers.delete(due[0]);
        now = Math.max(now, due[1].at);
        due[1].fn();
      }
      now = end;
    },
    pending: () => timers.size,
  };
}

class FakeSocket {
  constructor() {
    this.readyState = 0;
    this.binaryType = "blob";
    this.sent = [];
    this.closed = [];
    this.onopen = this.onmessage = this.onclose = this.onerror = null;
  }
  send(data) {
    this.sent.push(data);
  }
  close(code) {
    this.closed.push(code);
  }
  // test controls
  open() {
    this.readyState = 1;
    this.onopen?.({});
  }
  message(data) {
    this.onmessage?.({ data });
  }
  lose(code) {
    this.readyState = 3;
    this.onclose?.({ code });
  }
  frames() {
    return this.sent.map((s) => JSON.parse(s));
  }
}

/** A transport over a fake world: `sockets` lists every socket opened, `states` every connection report. */
function world({ random = () => 0.5, failOpen = false } = {}) {
  const c = clock();
  const sockets = [];
  const thrown = [];
  const deps = {
    open() {
      if (failOpen) throw new Error("no address");
      const s = new FakeSocket();
      sockets.push(s);
      return s;
    },
    now: c.now,
    setTimer: (fn, ms) =>
      c.setTimer(() => {
        try {
          fn();
        } catch (e) {
          thrown.push(e); // what the page's error reporting would see
        }
      }, ms),
    clearTimer: c.clearTimer,
    random,
  };
  const t = createTransport(deps);
  const states = [];
  t.onConnection((info) => states.push(info));
  return { c, t, sockets, states, thrown };
}

/** Open the first socket of `w` and let the transport see it. */
const connected = (w) => {
  w.t.listen("x", () => {});
  w.sockets[0].open();
  return w.sockets[0];
};

const settle = (p) => p.then((v) => ({ ok: v }), (e) => ({ err: e }));
const tick = () => new Promise((r) => setImmediate(r));

test("nothing is opened until the first call or listen", () => {
  const w = world();
  assert.equal(w.sockets.length, 0);
  w.t.listenerCount("x");
  w.t.onSession(() => {});
  assert.equal(w.sockets.length, 0);
  w.t.listen("x", () => {});
  assert.equal(w.sockets.length, 1);
  assert.deepEqual(w.states, [{ state: "connecting", attempt: 0, resumed: false, code: 0 }]);
  assert.equal(w.sockets[0].binaryType, "arraybuffer");
});

test("request ids rise from 1, the arguments default to an empty object, replies settle by id", async () => {
  const w = world();
  const a = settle(w.t.call("get_layout"));
  const b = settle(w.t.call("split", { pane: 3 }));
  const s = w.sockets[0];
  assert.deepEqual(s.sent, [], "a call before the first open waits");
  s.open();
  assert.deepEqual(s.frames(), [{ id: 1, cmd: "get_layout", args: {} }, { id: 2, cmd: "split", args: { pane: 3 } }]);
  s.message(JSON.stringify({ id: 2, ok: { view: 2 } }));
  s.message(JSON.stringify({ id: 1, ok: null }));
  assert.deepEqual(await b, { ok: { view: 2 } });
  assert.deepEqual(await a, { ok: null });
  const c = settle(w.t.call("x"));
  s.message(JSON.stringify({ id: 3, err: "no such pane" }));
  assert.deepEqual(await c, { err: "no such pane" });
  const d = settle(w.t.call("y"));
  s.message(JSON.stringify({ id: 4, err: { code: "refused", reason: "r", text: "t" } }));
  assert.deepEqual(await d, { err: { code: "refused", reason: "r", text: "t" } });
});

test("a reply settles a call once; ok with err, neither, an unknown id and a settled id are dropped", async () => {
  const w = world();
  const s = connected(w);
  const p = settle(w.t.call("a"));
  s.message(JSON.stringify({ id: 1, ok: 1, err: "x" }));
  s.message(JSON.stringify({ id: 1 }));
  s.message(JSON.stringify({ id: 99, ok: 1 }));
  s.message(JSON.stringify({ id: 1, ok: "first" }));
  s.message(JSON.stringify({ id: 1, ok: "second" }));
  s.message(JSON.stringify({ id: 1, err: "third" }));
  assert.deepEqual(await p, { ok: "first" });
  assert.deepEqual(w.thrown, []);
});

test("malformed frames are dropped without a throw, a callback or a state change", async () => {
  const w = world();
  const s = connected(w);
  const heard = [];
  w.t.listen("layout", (p) => heard.push(p));
  w.t.onSession((p) => heard.push(["session", p]));
  const p = settle(w.t.call("a"));
  const bad = [
    "", "not json", "{", "[]", "[1,2]", "null", "7", "true", '"text"',
    JSON.stringify({}), JSON.stringify({ id: "1", ok: 1 }), JSON.stringify({ id: 1.5, ok: 1 }), JSON.stringify({ id: -1, ok: 1 }),
    JSON.stringify({ id: 0, ok: 1 }), JSON.stringify({ id: 2 ** 60, ok: 1 }),
    JSON.stringify({ event: "", payload: 1 }), JSON.stringify({ event: 5, payload: 1 }), JSON.stringify({ event: "layout" }),
    JSON.stringify({ event: "x".repeat(65), payload: 1 }), JSON.stringify({ event: "é".repeat(33), payload: 1 }),
    JSON.stringify({ event: "layout", id: 1, payload: 1 }),
    new Uint8Array(3), new Blob(["x"]), undefined, null, 12,
  ];
  for (const frame of bad) s.message(frame);
  assert.deepEqual(heard, []);
  assert.equal(s.closed.length, 0);
  assert.deepEqual(w.thrown, []);
  assert.equal(w.states.length, 2, "connecting and open only");
  s.message(JSON.stringify({ id: 1, ok: "still pending" }));
  assert.deepEqual(await p, { ok: "still pending" });
});

test("events go to the listeners of their name in registration order; one that throws does not stop the others", () => {
  const w = world();
  const s = connected(w);
  const order = [];
  w.t.listen("layout", (p) => order.push(["a", p]));
  w.t.listen("layout", () => {
    throw new Error("boom");
  });
  w.t.listen("layout", (p) => order.push(["c", p]));
  w.t.listen("other", () => order.push("other"));
  s.message(JSON.stringify({ event: "layout", payload: { n: 1 } }));
  s.message(JSON.stringify({ event: "layout", payload: null }));
  assert.deepEqual(order, [["a", { n: 1 }], ["c", { n: 1 }], ["a", null], ["c", null]]);
  assert.equal(w.thrown.length, 0, "the error is rethrown from a timer, not at once");
  w.c.advance(0);
  assert.equal(w.thrown.length, 2);
  assert.equal(w.thrown[0].message, "boom");
});

test("event names like __proto__ and constructor are ordinary names", () => {
  const w = world();
  const s = connected(w);
  const heard = [];
  w.t.listen("__proto__", (p) => heard.push(["proto", p]));
  w.t.listen("constructor", (p) => heard.push(["ctor", p]));
  s.message('{"event":"__proto__","payload":1}');
  s.message('{"event":"constructor","payload":2}');
  s.message('{"event":"toString","payload":3}');
  s.message('{"__proto__":{"event":"layout"},"event":"constructor","payload":4}');
  assert.deepEqual(heard, [["proto", 1], ["ctor", 2], ["ctor", 4]]);
  assert.equal(w.t.listenerCount("__proto__"), 1);
  assert.equal(w.t.listenerCount("hasOwnProperty"), 0);
});

test("unlisten stops delivery and listenerCount follows listen and unlisten", () => {
  const w = world();
  const s = connected(w);
  let n = 0;
  const off1 = w.t.listen("e", () => n++);
  const off2 = w.t.listen("e", () => n++);
  assert.equal(w.t.listenerCount("e"), 2);
  s.message('{"event":"e","payload":0}');
  assert.equal(n, 2);
  off1();
  off1();
  assert.equal(w.t.listenerCount("e"), 1);
  s.message('{"event":"e","payload":0}');
  assert.equal(n, 3);
  off2();
  assert.equal(w.t.listenerCount("e"), 0);
  s.message('{"event":"e","payload":0}');
  assert.equal(n, 3);
});

test("hw-session reaches onSession with its payload and never a listen callback", () => {
  const w = world();
  const s = connected(w);
  const sessions = [];
  const listened = [];
  w.t.listen("hw-session", (p) => listened.push(p));
  const off = w.t.onSession((p) => sessions.push(p));
  s.message('{"event":"hw-session","payload":{"state":"ok","seconds":900}}');
  assert.deepEqual(sessions, [{ state: "ok", seconds: 900 }]);
  assert.deepEqual(listened, []);
  off();
  s.message('{"event":"hw-session","payload":{"state":"ok"}}');
  assert.equal(sessions.length, 1);
});

test("a binary frame of 9 bytes or more resolves the call of its id with the bytes; shorter or unknown ones are dropped", async () => {
  const w = world();
  const s = connected(w);
  const p = settle(w.t.call("file_range"));
  const frame = (id, bytes) => {
    const out = new Uint8Array(8 + bytes.length);
    new DataView(out.buffer).setBigUint64(0, BigInt(id), false);
    out.set(bytes, 8);
    return out.buffer;
  };
  s.message(frame(1, [])); // 8 bytes
  s.message(new ArrayBuffer(4));
  s.message(frame(7, [1]));
  const huge = new Uint8Array(9);
  huge.set([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
  s.message(huge.buffer);
  s.message(frame(0, [1]));
  s.message(frame(1, [9, 8, 7]));
  const got = await p;
  assert.ok(got.ok instanceof ArrayBuffer);
  assert.deepEqual([...new Uint8Array(got.ok)], [9, 8, 7]);
  s.message(frame(1, [1])); // already settled
  assert.deepEqual(w.thrown, []);
});

test("a message over 16 MiB closes the socket with 1009 and the page reconnects", () => {
  const w = world();
  const s = connected(w);
  s.message("x".repeat(MAX_MESSAGE_BYTES + 1));
  assert.deepEqual(s.closed, [1009]);
  assert.equal(w.states.at(-1).state, "reconnecting");
  const w2 = world();
  const s2 = connected(w2);
  s2.message("é".repeat(MAX_MESSAGE_BYTES / 2 + 1)); // fewer characters, more bytes
  assert.deepEqual(s2.closed, [1009]);
  const w3 = world();
  const s3 = connected(w3);
  s3.message(new ArrayBuffer(MAX_MESSAGE_BYTES + 1));
  assert.deepEqual(s3.closed, [1009]);
  const w4 = world();
  const s4 = connected(w4);
  s4.message(`{"event":"e","payload":"${"x".repeat(1024)}"}`);
  assert.deepEqual(s4.closed, []);
});

test("the calls of the start wait for the first open, in order, and give up after 15 s", async () => {
  const w = world();
  const first = settle(w.t.call("one"));
  const second = settle(w.t.call("two"));
  w.c.advance(FIRST_CONNECT_MS - 1);
  assert.equal(w.sockets[0].sent.length, 0);
  w.c.advance(1);
  assert.deepEqual(await first, { err: "cannot reach the server" });
  assert.deepEqual(await second, { err: "cannot reach the server" });
  assert.deepEqual(await settle(w.t.call("late")), { err: "not connected to the server" });
  assert.equal(w.sockets[0].sent.length, 0);
});

test("a first connection that fails is tried again and the waiting calls go out when it opens", async () => {
  const w = world();
  const p = settle(w.t.call("one"));
  w.sockets[0].lose(1006);
  assert.deepEqual(w.states.at(-1), { state: "reconnecting", attempt: 1, resumed: false, code: 0 });
  w.c.advance(BACKOFF_FIRST_MS);
  assert.equal(w.sockets.length, 2);
  w.sockets[1].open();
  assert.deepEqual(w.states.at(-1), { state: "open", attempt: 0, resumed: false, code: 0 });
  assert.deepEqual(w.sockets[1].frames(), [{ id: 1, cmd: "one", args: {} }]);
  w.sockets[1].message('{"id":1,"ok":true}');
  assert.deepEqual(await p, { ok: true });
});

test("a lost connection rejects pending calls, backs off 0.5 s doubling to 30 s, and resumes", async () => {
  const w = world();
  const s = connected(w);
  const p = settle(w.t.call("pending"));
  s.lose(1006);
  assert.deepEqual(await p, { err: "connection lost" });
  assert.deepEqual(w.states.at(-1), { state: "reconnecting", attempt: 1, resumed: false, code: 0 });
  assert.deepEqual(await settle(w.t.call("while down")), { err: "not connected to the server" });
  w.c.advance(BACKOFF_FIRST_MS - 1);
  assert.equal(w.sockets.length, 1);
  w.c.advance(1);
  assert.equal(w.sockets.length, 2);
  // the new socket is connecting: still not connected
  assert.deepEqual(await settle(w.t.call("while connecting")), { err: "not connected to the server" });
  w.sockets[1].open();
  assert.deepEqual(w.states.at(-1), { state: "open", attempt: 0, resumed: true, code: 0 });
  assert.deepEqual(w.sockets[1].sent, [], "nothing from before the loss is sent again");
  w.c.advance(60_000);
  assert.deepEqual(w.sockets[1].sent, []);
  // a loss after the reset starts again at the first delay
  w.sockets[1].lose(1006);
  assert.equal(w.states.at(-1).attempt, 1);
  w.c.advance(BACKOFF_FIRST_MS);
  assert.equal(w.sockets.length, 3);
});

test("the delays double up to the cap and stay inside the 20 percent band", () => {
  for (const [rand, factor] of [[0, 1 - JITTER], [0.5, 1], [0.999999, 1 + JITTER]]) {
    const w = world({ random: () => rand });
    const s = connected(w);
    s.lose(1006);
    let expected = BACKOFF_FIRST_MS;
    for (let attempt = 1; attempt <= 9; attempt++) {
      const base = Math.min(BACKOFF_MAX_MS, expected);
      const delay = base * factor;
      const before = w.sockets.length;
      w.c.advance(Math.floor(delay) - 1);
      assert.equal(w.sockets.length, before, `attempt ${attempt} came too early`);
      w.c.advance(2);
      assert.equal(w.sockets.length, before + 1, `attempt ${attempt} did not come in time`);
      assert.equal(w.states.at(-1).attempt, attempt);
      w.sockets.at(-1).lose(1006);
      expected *= 2;
    }
  }
});

test("a socket that cannot be made counts as a failed attempt", () => {
  const w = world({ failOpen: true });
  w.t.listen("x", () => {});
  assert.deepEqual(w.states.map((s) => s.state), ["connecting", "reconnecting"]);
  w.c.advance(BACKOFF_FIRST_MS);
  assert.equal(w.states.at(-1).attempt, 2);
});

test("a stale key press is not on any later socket", async () => {
  const w = world();
  const s = connected(w);
  s.lose(1006);
  const key = settle(w.t.call("key", { pane: 1, event: { key: "x" } }));
  assert.deepEqual(await key, { err: "not connected to the server" });
  w.c.advance(BACKOFF_FIRST_MS);
  w.sockets[1].open();
  w.c.advance(5 * 60_000);
  assert.deepEqual(w.sockets[1].sent, []);
  assert.deepEqual(s.sent, []);
});

test("the codes 4401 and 4403 end the connection without a retry", async () => {
  for (const [code, text] of [[CLOSE_SESSION_ENDED, "the session has ended"], [CLOSE_REFUSED, "the server refused this page"]]) {
    const w = world();
    const s = connected(w);
    const p = settle(w.t.call("pending"));
    s.lose(code);
    assert.deepEqual(await p, { err: "connection lost" });
    assert.deepEqual(w.states.at(-1), { state: "ended", attempt: 0, resumed: false, code });
    w.c.advance(10 * 60_000);
    assert.equal(w.sockets.length, 1, "no attempt");
    assert.deepEqual(await settle(w.t.call("later")), { err: text });
  }
});

test("ended during the first connection rejects the waiting calls", async () => {
  const w = world();
  const p = settle(w.t.call("one"));
  w.sockets[0].lose(CLOSE_REFUSED);
  assert.deepEqual(await p, { err: "the server refused this page" });
});

test("ninety seconds without a message close the socket and reconnect; any message restarts the timer", () => {
  const w = world();
  const s = connected(w);
  w.c.advance(SILENCE_MS - 1);
  s.message('{"event":"hw-session","payload":{"state":"ok"}}');
  w.c.advance(SILENCE_MS - 1);
  s.message("garbage");
  w.c.advance(SILENCE_MS - 1);
  s.message(new ArrayBuffer(2));
  w.c.advance(SILENCE_MS - 1);
  assert.deepEqual(s.closed, [], "alive as long as something arrives");
  w.c.advance(1);
  assert.equal(s.closed.length, 1, "the page closes the dead socket itself");
  assert.deepEqual(w.states.at(-1), { state: "reconnecting", attempt: 1, resumed: false, code: 0 });
  w.c.advance(BACKOFF_FIRST_MS);
  assert.equal(w.sockets.length, 2);
  w.sockets[1].open();
  assert.equal(w.states.at(-1).resumed, true);
  // the late close event of the dead socket changes nothing
  s.onclose?.({ code: 1006 });
  assert.equal(w.states.at(-1).state, "open");
});

test("a request over 8 MiB plus 4 KiB is refused before it is sent, one of exactly that size is sent", async () => {
  const w = world();
  const s = connected(w);
  const frameOverhead = JSON.stringify({ id: 1, cmd: "paste", args: { text: "" } }).length;
  const exact = MAX_REQUEST_BYTES - frameOverhead;
  const ok = settle(w.t.call("paste", { text: "x".repeat(exact) }));
  assert.equal(s.sent.length, 1);
  assert.equal(s.sent[0].length, MAX_REQUEST_BYTES);
  s.message('{"id":1,"ok":null}');
  assert.deepEqual(await ok, { ok: null });
  const over = await settle(w.t.call("paste", { text: "x".repeat(exact + 1) })); // one byte over
  assert.deepEqual(over, { err: "the request is too large" });
  assert.equal(s.sent.length, 1);
  assert.deepEqual(await settle(w.t.call("paste", { text: "é".repeat(MAX_REQUEST_BYTES / 2) })), { err: "the request is too large" });
  assert.equal(s.sent.length, 1);
  // a value that is not JSON is refused, not thrown
  assert.deepEqual((await settle(w.t.call("big", { n: 1n }))).err.length > 0, true);
});

test("a callback of onConnection that throws does not stop the others", () => {
  const w = world();
  const seen = [];
  w.t.onConnection(() => {
    throw new Error("bad callback");
  });
  w.t.onConnection((info) => seen.push(info.state));
  w.t.listen("x", () => {});
  assert.deepEqual(seen, ["connecting"]);
  w.c.advance(0);
  assert.equal(w.thrown.length, 1);
});

test("the address: wss for https, ws for http, the host and /ws, never a downgrade", () => {
  assert.equal(socketUrl("https:", "hw.example.test"), "wss://hw.example.test/ws");
  assert.equal(socketUrl("http:", "127.0.0.1:4000"), "ws://127.0.0.1:4000/ws");
  assert.equal(socketUrl("https:", "[::1]:8443"), "wss://[::1]:8443/ws");
  for (const [protocol, host] of [["file:", "x"], ["data:", "x"], ["ws:", "x"], ["wss:", "x"], ["", "x"], ["https:", ""], ["http:", ""],
    ["https:", "a/b"], ["https:", "user@host"], ["https:", "host with space"], ["https:", "h?x"], ["https:", "h\\x"]]) {
    assert.throws(() => socketUrl(protocol, host), Error, `${protocol}${host}`);
  }
});

test("the compiled files use no Web Storage, no indexedDB, no readable cookie and no console", () => {
  const strip = (src) => src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  for (const file of ["webtransport.js", "webhttp.js", "bridge.js"]) {
    const code = strip(readFileSync(new URL(`../../dist/${file}`, import.meta.url), "utf8"));
    for (const word of ["localStorage", "sessionStorage", "indexedDB", "document.cookie", "console.", "cookieStore", "caches."]) {
      assert.ok(!code.includes(word), `${file} uses ${word}`);
    }
  }
});

test("no timer is left over after the happy path settles", async () => {
  const w = world();
  const s = connected(w);
  const p = w.t.call("a");
  s.message('{"id":1,"ok":1}');
  await p;
  await tick();
  // only the silence timer of the open connection remains
  assert.equal(w.c.pending(), 1);
});
