// Helpers of the real-server browser tests: a browser context that accepts the harness's own certificate (and only here),
// the recorders that make a test fail on a request to another host, a page error or a policy violation, bounded polls, and
// the oracle: a second socket of the signed-in person, opened inside the page, that reads the terminal through the server's
// own protocol (the pane is drawn on a canvas, so no document text can be read).
//
// The page tests import the engines and the launcher from ../browser/lib.mjs; nothing in that library is changed, and the
// certificate exception lives in `openContext` of this file only.
import https from "node:https";
import { launch, loadPlaywright, ENGINES } from "../browser/lib.mjs";
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
  const recorder = { hosts: new Set(), requests: 0, violations: [], sockets: [], urls: [] };
  const pageErrors = [];
  const policy = [];
  const consoleErrors = [];
  // Pages that belong to a hostile site: what goes wrong in them is theirs, not the application's.
  const foreign = new WeakSet();

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
    recorder.urls.push(raw);
    recorder.hosts.add(url.host);
    if (!allowed.has(url.origin)) recorder.violations.push(`a request to ${url.origin}`);
  };
  context.on("request", (request) => classify(request.url()));
  const watch = (page) => {
    page.on("pageerror", (error) => {
      if (!foreign.has(page)) pageErrors.push(`page error: ${error.message}`);
    });
    page.on("console", (message) => {
      if (message.type() === "error") consoleErrors.push(message.text().slice(0, 200));
    });
    page.on("websocket", (socket) => {
      recorder.sockets.push(socket.url());
      classify(socket.url().replace(/^ws/, "http"));
    });
  };
  context.on("page", watch);
  await context.exposeBinding("__hwPolicy", (source, what) => {
    if (!foreign.has(source.page)) policy.push(`${what} (on ${new URL(source.page.url()).host}${new URL(source.page.url()).pathname})`);
  });
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
    /** Mark a page as one of a hostile site: its page errors and policy violations are not counted against the application. */
    foreign: (page) => foreign.add(page),
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
      check(answer.ok !== undefined, `${cmd} failed: ${show(answer.err ?? answer)}`);
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
export async function runTest(name, harnessOptions, body, { chromiumArgs = [] } = {}) {
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
    // `chromiumArgs` is for a file that must reach the server from a hostile page: Chromium's own protection of loopback addresses
    // (Local Network Access) would stop that page before the server saw anything, so such a file turns that one check off and
    // tests the server's own defences; everything else is the launcher of the page tests.
    browser = engine === "chromium" && chromiumArgs.length > 0 ? await loadPlaywright().chromium.launch({ args: chromiumArgs }) : await launch(engine);
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
    // The line of the test file that was running when it failed.
    const where = String(failure.stack ?? "").split("\n").find((line) => /\.test\.mjs:\d+/.test(line));
    if (where) console.error(`  ${where.trim()}`);
    process.exit(1);
  }
  console.log(`ok ${name} [${engine}]`);
  process.exit(0);
}

// ---- the sign-in screens -------------------------------------------------------------------------------------------------

/** Sign a person (from `harness.account`) in through the sign-in page: password, then the code. Ends on the application. `at` is the address to open first (a path with a fragment, for a link that must survive the sign-in). */
export async function signIn(page, harness, person, at = "/") {
  await page.goto(`${harness.origin}${at}`);
  await page.waitForSelector(".lg-box");
  await page.getByLabel("Username").fill(person.username);
  await page.getByLabel("Password", { exact: true }).fill(person.password);
  await page.getByRole("button", { name: "Continue" }).click();
  await page.getByLabel("Code").waitFor();
  await page.getByLabel("Code").fill(await person.authenticator.next());
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.waitForSelector("canvas#term");
}

/** The one-line sentence under the sign-in form (`.lg-line`), whatever its role. */
export const lineText = (page) => page.evaluate(() => (document.querySelector(".lg-line")?.textContent ?? "").trim());

/** The text of the first visible `role=alert` element, or the empty string. */
export const alertText = (page) =>
  page.evaluate(() => {
    for (const el of document.querySelectorAll('[role="alert"]')) {
      if (el.hidden || el.closest("[hidden]") || el.getClientRects().length === 0) continue;
      const text = el.textContent.trim();
      if (text !== "") return text;
    }
    return "";
  });

/** Wait until the sign-in box is idle again and the line under the form has a sentence; resolves with the sentence. */
export const answered = (page, what = "the sign-in line has a sentence") =>
  poll(async () => {
    const idle = await page.evaluate(() => document.querySelector(".lg-box")?.getAttribute("aria-busy") === "false");
    const line = await lineText(page);
    return idle && line !== "" ? line : undefined;
  }, what);

/** Open the command palette with the chord of the web build, type `query` and return the titles it lists. */
export async function openPalette(page) {
  // The page learns its key table a moment after it is drawn, so the chord is repeated until the palette answers.
  await poll(async () => {
    await page.keyboard.press("Control+Shift+P");
    if ((await page.getByRole("combobox").count()) > 0) return true;
    await page.waitForTimeout(300);
    return (await page.getByRole("combobox").count()) > 0 ? true : undefined;
  }, "the palette opens on its chord", 15_000);
}

export async function paletteTitles(page, query) {
  await openPalette(page);
  if (query) await page.keyboard.type(query);
  return poll(async () => {
    const titles = await page.getByRole("option").allTextContents();
    return titles.length > 0 ? titles.map((t) => t.trim()) : undefined;
  }, `the palette lists entries for "${query ?? ""}"`);
}

/**
 * Run a palette entry by its title, as a person would: the chord, the first letters, Enter. The page registers some entries
 * (People, Account, Sign out) a moment after it is drawn, when it has learned who the person is, so the palette is asked
 * again until the entry is listed.
 */
export async function runEntry(page, title, query = title.replace(/…$/, "")) {
  const wanted = title.replace(/…$/, "");
  const pattern = new RegExp(`^\\s*${wanted.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`);
  await poll(async () => {
    await openPalette(page);
    await page.keyboard.type(query);
    // Some entries are registered a moment after the page is drawn: until then the query matches nothing and the palette is
    // closed and asked again.
    await page.getByRole("option").first().waitFor({ timeout: 1000 }).catch(() => {});
    const match = page.getByRole("option").filter({ hasText: pattern }).first();
    if ((await match.count()) > 0) {
      await match.click();
      return true;
    }
    await page.keyboard.press("Escape");
    await page.waitForTimeout(300);
    return undefined;
  }, `the palette lists "${title}"`, 20_000);
}

/** Whether any address a context requested holds one of the secrets. The secrets are never shown. */
export function addressesHold(opened, secrets) {
  return opened.recorder.urls.some((url) => secrets.some((s) => url.includes(s)));
}

// ---- the people dialog ---------------------------------------------------------------------------------------------------

/** The dialog of the people list, opened from the palette. */
export async function openPeople(page) {
  await page.locator("canvas#term").click();
  await runEntry(page, "People…");
  const dialog = page.getByRole("dialog", { name: "People", exact: true });
  await dialog.waitFor();
  await page.waitForSelector(".people tbody tr");
  return dialog;
}

/**
 * An invitation made the way an administrator makes it: the people dialog, the form, the step-up layer when the sign-in is old
 * (the password of `admin` answers it), and the link read from the layer that shows it once. The dialog is closed again.
 * Resolves with the link.
 */
export async function inviteLink(page, admin, { role = "Member" } = {}) {
  const dialog = await openPeople(page);
  await page.getByRole("button", { name: "Invite", exact: true }).click();
  await page.getByRole("dialog", { name: "Invite", exact: true }).waitFor();
  if (role !== "Member") await page.getByLabel(role, { exact: true }).check();
  await page.getByRole("button", { name: "Create link" }).click();
  const layer = page.getByRole("dialog", { name: "Confirm it is you" });
  const shown = page.getByRole("dialog", { name: "Invitation link" });
  // After a recent sign-in the link comes at once; after an old one the layer comes first.
  const first = await poll(async () => ((await layer.count()) > 0 ? "step-up" : (await shown.count()) > 0 ? "link" : undefined), "the step-up layer or the link");
  if (first === "step-up") {
    await page.locator("#web-step-field").fill(admin.password);
    await page.getByRole("button", { name: "Confirm" }).click();
    await shown.waitFor();
  }
  const link = (await page.locator(".ppl-link-text").textContent()).trim();
  const note = (await shown.innerText()).replace(/\s+/g, " ");
  await page.getByRole("button", { name: "Done" }).click();
  await dialog.getByRole("button", { name: "Close" }).click();
  return { link, note, stepUp: first === "step-up" };
}

/** The row of the people list that belongs to the person with this username. */
export const personRow = (page, username) =>
  page.locator(".people tbody tr", { has: page.locator(".ppl-user", { hasText: new RegExp(`^${username.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`) }) });

/** What the list says about a person: the role and the state word. */
export async function personFacts(page, username) {
  const row = personRow(page, username);
  return { role: (await row.locator(".ppl-role").innerText()).trim(), state: (await row.locator(".ppl-state .ppl-word").innerText()).trim() };
}

/** Open the menu of a person's row and resolve with the entries (label, whether it is held back). */
export async function openMenu(page, username) {
  await personRow(page, username).locator("button.ppl-more").click();
  const menu = page.getByRole("menu");
  await menu.waitFor();
  const items = menu.getByRole("menuitem");
  const out = [];
  for (const item of await items.all()) out.push({ label: (await item.innerText()).trim(), held: (await item.getAttribute("aria-disabled")) === "true" });
  return { menu, out };
}

/** The step-up layer, answered with the password. Resolves when it is gone. */
export async function confirmItsYou(page, password) {
  const layer = page.getByRole("dialog", { name: "Confirm it is you" });
  await layer.waitFor();
  await page.locator("#web-step-field").fill(password);
  await page.getByRole("button", { name: "Confirm" }).click();
  await layer.waitFor({ state: "detached" });
}

/** Make `provoked` runs of wrong passwords for a person in a context of its own until the server says to wait. */
export async function provokeLockout(open, harness, username) {
  const context = await open();
  const page = await openPage(context, harness, "/");
  await page.waitForSelector(".lg-box");
  for (let attempt = 1; attempt <= 7; attempt++) {
    await page.getByLabel("Username").fill(username);
    await page.getByLabel("Password", { exact: true }).fill("this is not the password");
    await page.getByRole("button", { name: "Continue" }).click();
    const sentence = await answered(page);
    if (sentence.startsWith("Too many attempts")) {
      await page.close();
      return attempt;
    }
  }
  throw new Error("the server did not ask to wait after seven wrong passwords");
}

/**
 * Open one WebSocket to the server from the page, as a script of the page would, and report how it ended: `{ opened, code }`.
 * The code is the one the server closed with (4401 for a session that is over), or 1006 when the upgrade was refused.
 */
export function socketOutcome(page, ms = 10_000) {
  return page.evaluate(
    (limit) =>
      new Promise((resolve) => {
        const socket = new WebSocket(`wss://${location.host}/ws`);
        let opened = false;
        const timer = setTimeout(() => resolve({ opened, code: null }), limit);
        socket.onopen = () => {
          opened = true;
        };
        socket.onclose = (e) => {
          clearTimeout(timer);
          resolve({ opened, code: e.code });
        };
      }),
    ms,
  );
}

// ---- pictures and links in the page --------------------------------------------------------------------------------------

/** The commands the page's own socket sent and the binary answers it received, from the browser's side (no code of the page is touched). */
export function watchSocket(page) {
  const sent = [];
  const answers = [];
  const generations = new Set();
  page.on("websocket", (socket) => {
    socket.on("framesent", ({ payload }) => {
      if (typeof payload !== "string") return;
      try {
        const message = JSON.parse(payload);
        if (typeof message.cmd === "string") sent.push({ cmd: message.cmd, args: message.args ?? {} });
      } catch {
        // not a command
      }
    });
    socket.on("framereceived", ({ payload }) => {
      if (typeof payload !== "string") return answers.push(payload.length);
      // The generations of every picture that frames told about, to see a picture move or stand still.
      if (!payload.includes('"images"')) return;
      try {
        const message = JSON.parse(payload);
        for (const placement of message.event === "frame" ? (message.payload.images ?? []) : []) generations.add(`${placement[0]}:${placement[1]}`);
      } catch {
        // not a frame
      }
    });
  });
  return {
    /** How many different generations of the picture `id` the frames told about so far. */
    generations: (id) => [...generations].filter((g) => g.startsWith(`${id}:`)).length,
    /** The arguments of every command with this name, oldest first. */
    sent: (cmd) => sent.filter((m) => m.cmd === cmd).map((m) => m.args),
    /** The lengths of the binary messages received (8 bytes of call id and the answer). */
    binaryAnswers: () => answers.slice(),
  };
}

/** The page's module `name` (`state`, `scene`, ...): the instance the page itself runs. */
const moduleUrl = (name) => `${name}.js`;

/**
 * Where the grid of the focused pane is, from the page's own state and geometry: the origin and size in CSS pixels relative to
 * the canvas, the cell size, the size in cells that the core was told, the canvas's place in the window, the renderer in use.
 * Resolves with `undefined` until the page has laid out a pane.
 */
export function gridOf(page) {
  return page.evaluate(async (urls) => {
    const { state } = await import(new URL(urls.state, location.href).href);
    const { gridRect } = await import(new URL(urls.geometry, location.href).href);
    const view = state.view;
    if (!view || !(state.metrics.cellW > 1)) return undefined;
    const entry = view.panes.find(([id]) => id === view.focus) ?? view.panes[0];
    if (!entry) return undefined;
    const g = gridRect(state.metrics, entry[1], state.headerPx);
    const size = state.sizes.get(entry[0]);
    if (!size) return undefined;
    const box = document.getElementById("term").getBoundingClientRect();
    return {
      pane: entry[0], x: g.x, y: g.y, w: g.w, h: g.h, cellW: state.metrics.cellW, cellH: state.metrics.cellH, dpr: state.metrics.dpr,
      cols: size.cols, rows: size.rows, left: box.left, top: box.top, renderer: state.renderer, os: state.os,
    };
  }, { state: moduleUrl("state"), geometry: moduleUrl("geometry") });
}

/** The centre of a cell in CSS pixels relative to the canvas (what `readColours` takes) and in the window (what the mouse takes). */
export const cellCentre = (grid, col, row) => ({
  x: grid.x + (col + 0.5) * grid.cellW,
  y: grid.y + (row + 0.5) * grid.cellH,
  pageX: grid.left + grid.x + (col + 0.5) * grid.cellW,
  pageY: grid.top + grid.y + (row + 0.5) * grid.cellH,
});

/**
 * The colours (0xRRGGBB) of the canvas at `points` (`{x, y}` in CSS pixels relative to the canvas), after the next draw, for either
 * renderer. A WebGL canvas is cleared once the browser has shown a frame, so the page is asked to draw again first: that draw is
 * queued before this read in the same animation frame and the read sees the pixels it just drew (the way the page tests read it).
 */
export function readColours(page, points) {
  return page.evaluate(
    async ({ pts, scene }) => {
      const { schedule } = await import(new URL(scene, location.href).href);
      schedule();
      await new Promise((done) => requestAnimationFrame(() => done()));
      const c = document.getElementById("term");
      const k = c.width / c.clientWidth;
      const gl = c.getContext("webgl2");
      if (gl) {
        const px = new Uint8Array(4);
        return pts.map(({ x, y }) => {
          gl.readPixels(Math.floor(x * k), c.height - 1 - Math.floor(y * k), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
          return (px[0] << 16) | (px[1] << 8) | px[2];
        });
      }
      const ctx = c.getContext("2d");
      return pts.map(({ x, y }) => {
        const [r, g, b] = ctx.getImageData(Math.floor(x * k), Math.floor(y * k), 1, 1).data;
        return (r << 16) | (g << 8) | b;
      });
    },
    { pts: points.map(({ x, y }) => ({ x, y })), scene: moduleUrl("scene") },
  );
}

export const RED = 0xff0000;
export const GREEN = 0x00ff00;
export const BLUE = 0x0000ff;
export const WHITE = 0xffffff;

/** `0xRRGGBB` as `#rrggbb` for a message. */
export const hex = (n) => `#${n.toString(16).padStart(6, "0")}`;

/** Whether a colour is the wanted one, each channel within `tolerance` (a picture is scaled and sampled bilinearly: the centre of a cell can be a few percent off its colour, red reads #ef1000, while the other colours are far away). */
export const isColour = (got, want, tolerance = 40) =>
  [16, 8, 0].every((shift) => Math.abs(((got >> shift) & 255) - ((want >> shift) & 255)) <= tolerance);

/** Type text into a pane through the oracle socket (the `text` command, the same input path as the keyboard's text). */
export const typeInto = (socket, pane, text) => socket.ok("text", { pane, text });

// The shell texts. Every escape byte is written as an octal escape of `printf`, so nothing but printable text is ever typed.

/** Delete every picture and clear the screen. */
export const RESET = "printf '\\033_Ga=d,d=A,q=2\\033\\\\\\033[2J\\033[H'";

/** Move the cursor to the cell (`col`, `row`), both counted from 0. */
export const cursorTo = (col, row) => `printf '\\033[${row + 1};${col + 1}H'`;

/** A word printed from its two halves, so that the typed command never holds the word itself (a marker the screen can show only once the program ran). */
export const marker = (word) => `printf 'hw-%s\\n' ${word}`;

/**
 * The shell text that prints a Kitty picture: transmit and display (`a=T`) 32 bit straight RGBA (`f=32`) of `width` by `height`
 * pixels as image `id`, over `cols` by `rows` cells, with no reply to the program (`q=2`). The base64 of the bytes goes in chunks of
 * 4096 characters, `m=1` on all but the last, as the protocol asks. `extra` is added to the first control data (`z=-1`, ...).
 */
export function kittyPicture({ id, width, height, rgba, cols, rows, extra = "" }) {
  const bytes = Buffer.from(rgba);
  if (bytes.length !== width * height * 4) throw new Error("the pixel bytes do not fit the size");
  const text = bytes.toString("base64");
  const pieces = [];
  for (let at = 0; at < text.length; at += 4096) pieces.push(text.slice(at, at + 4096));
  const format = pieces
    .map((piece, i) => {
      const more = i + 1 < pieces.length ? 1 : 0;
      const keys = i === 0 ? `a=T,f=32,s=${width},v=${height},i=${id},c=${cols},r=${rows},q=2${extra ? `,${extra}` : ""},m=${more}` : `m=${more}`;
      return `\\033_G${keys};${piece}\\033\\\\`;
    })
    .join("");
  return `printf '${format}'`;
}

/**
 * A picture of opaque white, `width` by `height` pixels, printed by the shell itself in chunks of 4096 characters (so the typed line
 * stays short): all bytes are 0xFF, so a chunk of 3072 bytes is 4096 times "/" in base64.
 */
export function kittyWhite({ id, width, height, cols, rows }) {
  const total = width * height * 4;
  const full = Math.floor(total / 3072);
  const rest = total - full * 3072;
  const keys = `a=T,f=32,s=${width},v=${height},i=${id},c=${cols},r=${rows},q=2`;
  const middle = rest === 0 ? full - 1 : full;
  const last = rest === 0 ? '"$c"' : `'${Buffer.alloc(rest, 255).toString("base64")}'`;
  return (
    `c=$(head -c 4096 /dev/zero | tr '\\0' '/'); printf '\\033_G${keys},m=1;%s\\033\\\\' "$c"; n=1; ` +
    `while [ $n -lt ${middle} ]; do printf '\\033_Gm=1;%s\\033\\\\' "$c"; n=$((n+1)); done; ` +
    `printf '\\033_Gm=0;%s\\033\\\\' ${last}`
  );
}

/** The Kitty animation of a picture of two one-pixel frames: `first` and `second` (RGBA, four bytes each), `gap` milliseconds apart, looping. */
export function kittyAnimation({ id, cols, rows, first, second, gap }) {
  const a = Buffer.from(first).toString("base64");
  const b = Buffer.from(second).toString("base64");
  return [
    // a=T: the root frame, shown at the cursor.
    `printf '\\033_Ga=T,f=32,s=1,v=1,i=${id},c=${cols},r=${rows},q=2;${a}\\033\\\\'`,
    // a=f: a second frame, z is its gap in milliseconds.
    `printf '\\033_Ga=f,i=${id},f=32,s=1,v=1,z=${gap},q=2;${b}\\033\\\\'`,
    // a=a with r=1: the gap of the root frame.
    `printf '\\033_Ga=a,i=${id},r=1,z=${gap},q=2\\033\\\\'`,
    // a=a with s=3: run, looping (s=1 would stop, s=2 wait for frames).
    `printf '\\033_Ga=a,i=${id},s=3,q=2\\033\\\\'`,
  ].join("; ");
}

/** The shell text that prints `text` as a hyperlink to `uri` (OSC 8, closed again after the text); `uri` is printf text. */
export const osc8 = (uri, text) => `printf '\\033]8;;${uri}\\033\\\\${text}\\033]8;;\\033\\\\'`;

/** Several shell texts as one line for the shell, run with Enter. */
export const line = (...parts) => `${parts.join("; ")}\r`;

// ---- the device features -------------------------------------------------------------------------------------------------------

const PEM = (der) => `-----BEGIN CERTIFICATE-----\n${der.match(/.{1,64}/g).join("\n")}\n-----END CERTIFICATE-----\n`;

/**
 * A native client of the server, as an app is: no cookie, no page, JSON over the harness's own certificate (trusted for these
 * requests only). `call(method, path, { body, token })` resolves with `{ status, body }`; `start` asks for a grant and gives back the
 * codes; `poll` asks once whether it was decided; `me` asks as the device with its token.
 */
export function nativeClient(harness) {
  const agent = new https.Agent({ ca: PEM(harness.certificate), keepAlive: false });
  const call = (method, path, { body, token } = {}) =>
    new Promise((resolve, reject) => {
      const payload = body === undefined ? undefined : JSON.stringify(body);
      const headers = { accept: "application/json" };
      if (payload !== undefined) {
        headers["content-type"] = "application/json";
        headers["content-length"] = String(Buffer.byteLength(payload));
      }
      if (token !== undefined) headers.authorization = `Bearer ${token}`;
      const request = https.request(`${harness.origin}${path}`, { method, agent, headers, servername: harness.hostname }, (response) => {
        const chunks = [];
        response.on("data", (c) => chunks.push(c));
        response.on("end", () => {
          const text = Buffer.concat(chunks).toString("utf8");
          let parsed;
          try {
            parsed = text === "" ? undefined : JSON.parse(text);
          } catch {
            parsed = undefined;
          }
          resolve({ status: response.statusCode, body: parsed });
        });
      });
      request.on("error", reject);
      request.end(payload);
    });
  return {
    call,
    /** The start of a grant: `{ status, userCode, deviceCode, uri }`; the codes are undefined when the server did not make one. */
    async start(name = "Test laptop", platform = "macos") {
      const r = await call("POST", "/api/device/code", { body: { name, platform } });
      return { status: r.status, userCode: r.body?.user_code, deviceCode: r.body?.device_code, uri: r.body?.verification_uri_complete, body: r.body };
    },
    /** One poll: `{ status, state, token, deviceId }`. */
    async poll(deviceCode) {
      const r = await call("POST", "/api/device/token", { body: { device_code: deviceCode } });
      return { status: r.status, state: r.body?.state, token: r.body?.token, deviceId: r.body?.device_id, body: r.body };
    },
    /** `GET /api/device/me` with the token of a device. */
    me: (token) => call("GET", "/api/device/me", { token }),
  };
}

/** Turn the two device switches from the page of a signed-in administrator, as its own requests would (the page's request function adds the token). */
export function setSwitches(page, change) {
  return page.evaluate(async (body) => {
    const { http } = await import(new URL("bridge.js", location.href).href);
    return http("POST", "/api/admin/device-access", body);
  }, change);
}

/** Whether the page keeps `text` in its address, its storage, its cookies or its title. The text is never shown. */
export function pageHolds(page, text) {
  return page.evaluate((needle) => {
    const where = { address: location.href, title: document.title, cookie: document.cookie, storage: JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)]) };
    return Object.entries(where).filter(([, v]) => v.includes(needle)).map(([k]) => k);
  }, text);
}
