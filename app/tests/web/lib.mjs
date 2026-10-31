// Helpers of the real-server browser tests: a browser context that accepts the harness's own certificate (and only here),
// the recorders that make a test fail on a request to another host, a page error or a policy violation, bounded polls, and
// the oracle: a second socket of the signed-in person, opened inside the page, that reads the terminal through the server's
// own protocol (the pane is drawn on a canvas, so no document text can be read).
//
// The page tests import the engines and the launcher from ../browser/lib.mjs; nothing in that library is changed, and the
// certificate exception lives in `openContext` of this file only.
import { launch, ENGINES } from "../browser/lib.mjs";
import { startHarness } from "./harness.mjs";

export { ENGINES, launch };

/** The exit status of a test file that skipped itself (the runner reads the reason from the last `SKIP:` line). */
export const SKIPPED = 77;

/** Print the reason and end the process as skipped. */
export function skip(reason) {
  console.log(`SKIP: ${reason}`);
  process.exit(SKIPPED);
}

export const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

/** The text of a state for a failure message: short, one line, never a secret (callers pass states, not values). */
const show = (value) => {
  let text;
  try {
    text = typeof value === "string" ? value : JSON.stringify(value);
  } catch {
    text = String(value);
  }
  text = String(text ?? "undefined").replace(/\s+/g, " ");
  return text.length > 300 ? `${text.slice(0, 300)}...` : text;
};

/**
 * Poll `read` every 100 ms until it returns something other than `undefined`, `null`, `false` or `""`, and return that. After
 * `ms` the error says what was waited for and the last state seen: `timeout: <what> (last: <state>)`.
 */
export async function poll(read, what, ms = 10_000) {
  const t0 = Date.now();
  let last;
  for (;;) {
    try {
      last = await read();
    } catch (e) {
      last = `threw: ${e?.message ?? e}`;
      if (/Target (page|context|browser).* closed|has been closed/i.test(String(e?.message))) throw e;
    }
    if (last !== undefined && last !== null && last !== false && last !== "") return last;
    if (Date.now() - t0 >= ms) throw new Error(`timeout: ${what} (last: ${show(last)})`);
    await sleep(100);
  }
}

/** Fail with a message that names the step and the state (never a value that is secret). */
export function check(condition, message) {
  if (!condition) throw new Error(message);
}

/** `check` for equality: the message names the step, and shows both sides, which the caller keeps free of secrets. */
export function equal(actual, expected, message) {
  if (actual !== expected) throw new Error(`${message} (got ${show(actual)}, expected ${show(expected)})`);
}

/**
 * A browser context for one person (or one stranger) on the harness. The certificate of the harness is accepted for this
 * context only. The context records every request and every socket (`recorder`), page errors, content-security-policy
 * violations and console errors; `expectClean` throws when a recorder holds anything it should not.
 *
 * `allow` lists further origins a test may reach on purpose (the identity provider on another origin, the fake hosts of the
 * attacker pages). A request to a host that is neither the harness host, 127.0.0.1 with the port of an allowed origin, nor
 * one of those is a violation.
 */
export async function openContext(browser, harness, { allow = [], ...contextOptions } = {}) {
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1100, height: 700 }, ...contextOptions });
  const allowed = new Set([harness.origin, ...allow]);
  const recorder = { hosts: new Set(), requests: 0, violations: [], sockets: [] };
  const pageErrors = [];
  const policy = [];
  const consoleErrors = [];

  const classify = (raw) => {
    let url;
    try {
      url = new URL(raw);
    } catch {
      recorder.violations.push(`an address that is not a URL`);
      return;
    }
    if (["data:", "blob:", "about:"].includes(url.protocol)) return;
    recorder.requests++;
    recorder.hosts.add(url.host);
    if (!allowed.has(url.origin)) recorder.violations.push(`a request to ${url.origin}`);
  };
  context.on("request", (request) => classify(request.url()));
  const watch = (page) => {
    page.on("pageerror", (error) => pageErrors.push(`page error: ${error.message}`));
    page.on("console", (message) => {
      if (message.type() === "error") consoleErrors.push(message.text().slice(0, 200));
    });
    page.on("websocket", (socket) => {
      recorder.sockets.push(socket.url());
      classify(socket.url().replace(/^ws/, "http"));
    });
  };
  context.on("page", watch);
  await context.exposeFunction("__hwPolicy", (what) => policy.push(what));
  await context.addInitScript(() => {
    window.addEventListener(
      "securitypolicyviolation",
      (e) => window.__hwPolicy(`${e.violatedDirective} blocked ${e.blockedURI || "inline"}`),
      true,
    );
  });

  return {
    context,
    recorder,
    pageErrors,
    policy,
    consoleErrors,
    /** Throw when the context saw a request to another host, a page error or a policy violation. */
    expectClean(what = "the context") {
      const found = [
        ...recorder.violations.map((v) => `request: ${v}`),
        ...pageErrors,
        ...policy.map((p) => `policy violation: ${p}`),
      ];
      if (found.length > 0) throw new Error(`${what} was not clean: ${[...new Set(found)].slice(0, 6).join("; ")}`);
    },
    async close() {
      await context.close().catch(() => {});
    },
  };
}

/** Open a page of the harness's application at `path` in a context made by `openContext`. */
export async function openPage(opened, harness, path = "/") {
  const page = await opened.context.newPage();
  await page.goto(`${harness.origin}${path}`);
  return page;
}

// ---- the oracle ---------------------------------------------------------------------------------------------------------

/** The text of one `frame` event payload: one line per row of the screen, runs joined, the cells padded to the width. */
export function frameText(payload) {
  const rows = new Map();
  for (const row of payload.changed ?? []) {
    // One code point per cell; the rest of a grapheme cluster travels in `extras` as [column, tail].
    const cells = [];
    for (const [chunk] of row.runs) cells.push(...chunk);
    for (const [column, tail] of row.extras ?? []) if (column < cells.length) cells[column] += tail;
    rows.set(row.y, cells.join("").replaceAll("\0", "").replace(/ +$/, ""));
  }
  const lines = [];
  for (let y = 0; y < (payload.rows ?? 0); y++) lines.push(rows.get(y) ?? "");
  return lines.join("\n");
}

let oracleCount = 0;

/**
 * A WebSocket opened inside `page` on `wss://<host>/ws`. The browser sends the session cookie and the page's origin, so the
 * server treats it as one more socket of the signed-in person. Pass `options.origin = false` to open it from a page whose
 * origin is another one (an attacker page): the test then looks at how the server answers.
 */
export async function oracle(page) {
  const key = `__hwOracle${++oracleCount}`;
  await page.evaluate((name) => {
    const state = { messages: [], answers: {}, closed: null, opened: false, extensions: "", next: 1, seen: 0 };
    const socket = new WebSocket(`wss://${location.host}/ws`);
    socket.onopen = () => {
      state.opened = true;
      state.extensions = socket.extensions;
    };
    socket.onmessage = (e) => {
      const message = JSON.parse(e.data);
      state.seen++;
      if (message.id !== undefined && message.event === undefined) state.answers[message.id] = message;
      else state.messages.push(message);
    };
    socket.onclose = (e) => {
      state.closed = { code: e.code, reason: e.reason };
    };
    state.socket = socket;
    window[name] = state;
  }, key);

  const read = (fn, arg) => page.evaluate(fn, { key, ...arg });
  const self = {
    /** Wait until the socket is open (the server accepted the upgrade) or closed. */
    async open() {
      const state = await poll(
        () => read(({ key }) => (window[key].opened ? "open" : window[key].closed ? `closed ${window[key].closed.code}` : undefined)),
        "the oracle socket opens",
      );
      check(state === "open", `the oracle socket did not open (${state})`);
      return self;
    },
    /** Send one command and resolve with the whole answer (`{id, ok}` or `{id, error}`). */
    async call(cmd, args = {}, ms = 10_000) {
      const id = await read(({ key, cmd, args }) => {
        const state = window[key];
        const id = state.next++;
        state.socket.send(JSON.stringify({ id, cmd, args }));
        return id;
      }, { cmd, args });
      return poll(() => read(({ key, id }) => window[key].answers[id], { id }), `the answer to ${cmd}`, ms);
    },
    /** `call` that must succeed; resolves with the value of `ok`. */
    async ok(cmd, args = {}) {
      const answer = await self.call(cmd, args);
      check(answer.ok !== undefined, `${cmd} failed: ${show(answer.error ?? answer)}`);
      return answer.ok;
    },
    /** Every event received so far, oldest first (the answers are kept apart). */
    events: () => read(({ key }) => window[key].messages.slice()),
    layout: () => self.ok("get_layout"),
    /** The extension string the browser reports for the oracle's socket. */
    extensions: () => read(({ key }) => window[key].extensions),
    /** `null` while open, else `{ code, reason }`. */
    closed: () => read(({ key }) => window[key].closed),
    /** The text of the pane's screen now: asks for a full frame and reads the one that comes back. */
    async screen(pane, ms = 10_000) {
      const before = await read(({ key }) => window[key].messages.length);
      await self.ok("request_full", { pane });
      const frame = await poll(
        async () => {
          const messages = await read(({ key, before }) => window[key].messages.slice(before), { before });
          return messages.find((m) => m.event === "frame" && m.payload?.pane === pane && m.payload.full === true);
        },
        `a full frame of pane ${pane}`,
        ms,
      );
      return frameText(frame.payload);
    },
    async close() {
      await read(({ key }) => window[key].socket.close(1000, "done"));
    },
  };
  return self.open();
}

/** The id of the pane that has the focus, from `get_layout`. */
export async function focusedPane(oracleSocket) {
  const view = await oracleSocket.layout();
  return view.focus;
}

// ---- a test file ---------------------------------------------------------------------------------------------------------

const FILE_LIMIT_MS = 240_000;

/**
 * Run the body of a test file for the engine in `HW_ENGINE`: start one server through the harness, launch the engine, call
 * `body({ engine, browser, harness, open(options) })`, and clean up whatever happens. Exit status 0 is a pass, 1 a failure
 * (the message names the step and the state), 77 a skip (see `skip`).
 */
export async function runTest(name, harnessOptions, body) {
  const engine = process.env.HW_ENGINE;
  if (!ENGINES.includes(engine)) {
    console.error(`HW_ENGINE must be one of ${ENGINES.join(", ")}`);
    process.exit(2);
  }
  const watchdog = setTimeout(() => {
    console.error(`FAIL ${name} [${engine}]: timeout: the file did not finish within ${FILE_LIMIT_MS / 1000} s`);
    process.exit(1);
  }, FILE_LIMIT_MS);
  let harness;
  let browser;
  const contexts = [];
  let failure = null;
  try {
    harness = await startHarness(harnessOptions);
    browser = await launch(engine);
    const open = async (options) => {
      const opened = await openContext(browser, harness, options);
      contexts.push(opened);
      return opened;
    };
    await body({ engine, browser, harness, open, contexts });
  } catch (e) {
    failure = e;
  } finally {
    clearTimeout(watchdog);
    await browser?.close().catch(() => {});
    try {
      await harness?.stop();
    } catch (e) {
      failure ??= e;
    }
  }
  if (failure) {
    console.error(`FAIL ${name} [${engine}]: ${String(failure.message ?? failure).split("\n").slice(0, 12).join("\n")}`);
    process.exit(1);
  }
  console.log(`ok ${name} [${engine}]`);
  process.exit(0);
}
