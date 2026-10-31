// Files in the browser build (docs/design.md 6.30): a file dropped on a pane is sent to the server with the page's request function
// and its path, quoted for the shell, is typed into the pane like a paste; a drop never takes the page away from the person.
// The desktop has no such target: it does not listen for files at all. The server is scripted: `PUT /api/upload?name=` answers from
// a table, and the page's request function is wrapped so that the bytes of the body can be compared.
import assert from "node:assert/strict";
import { currentBackend } from "./lib.mjs";
import { leftover, openMain, sessionRoute } from "./websession-lib.mjs";

const FOLDER = "/home/p/uploads";

/** The answer of the server for a stored file: the path in the person's upload folder and the size. */
const stored = (name, size) => ({ status: 200, body: { path: `${FOLDER}/${name}`, size } });

/** Everything the page said in the notice segment of the status line, one entry for each change, in order. */
const notices = (page) => page.evaluate(() => window.__notices);
const noticeNow = (page) => page.evaluate(() => document.querySelector("#status .notice")?.textContent ?? "");
const pastes = (page) => page.evaluate(() => window.__MOCK__.callsOf("paste"));
const settle = (page) =>
  page.evaluate(async () => {
    for (let i = 0; i < 3; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  });

/**
 * Open the main page on the web backend as the browser build: the platform is `web`, `GET /api/session` is scripted, and the
 * upload route answers from `answer(request, n)`, which returns `{ status, body }`, `{ abort: true }` or `{ delay, status, body }`.
 * Returns the server recorder and the log of the upload requests: method, path with query, header names, the bytes and the times.
 */
async function open(page, url, { answer = () => stored("x", 0), os = "web", blocks = [] } = {}) {
  await page.addInitScript(
    ([o, b]) => {
      window.__MOCK__.on("app_info", () => ({ os: o, version: "t", hostname: "h" }));
      window.__MOCK__.on("pane_blocks", () => structuredClone(b));
      window.__MOCK__.on("files_list", ({ path }) => ({ path, parent: null, entries: [], truncated: false }));
      // The entries API of a real drop says which item is a folder; a made-up drop has none, so a file named `dir-...` plays one.
      DataTransferItem.prototype.webkitGetAsEntry = function () {
        const f = this.getAsFile();
        const directory = f !== null && f.name.startsWith("dir-");
        return { isDirectory: directory, isFile: !directory };
      };
      // Init scripts of earlier cases run again on every load: the recorders are made once, the answers above come from the newest script.
      if (window.__recording) return;
      window.__recording = true;
      // The page's request function is `fetch`: record the body of an upload before it goes on.
      window.__sent = [];
      const real = window.fetch.bind(window);
      window.fetch = async (input, init) => {
        const address = String(input);
        if (address.includes("/api/upload") && init && init.body instanceof Blob) {
          const entry = {
            method: init.method,
            address,
            headers: Object.keys(init.headers ?? {}),
            token: (init.headers ?? {})["X-CSRF-Token"] ?? null,
            type: init.body.type,
            bytes: Array.from(new Uint8Array(await init.body.arrayBuffer())),
            credentials: init.credentials,
            redirect: init.redirect,
            at: performance.now(),
          };
          window.__sent.push(entry);
        } else if (address.includes("/api/upload")) {
          window.__sent.push({ method: init?.method, address, headers: [], bytes: null, body: typeof init?.body });
        }
        return real(input, init);
      };
      window.__notices = [];
      const watch = () => {
        const seg = document.querySelector("#status .notice");
        if (!seg) return false;
        new MutationObserver(() => {
          const t = seg.textContent;
          if (t !== "" && window.__notices.at(-1) !== t) window.__notices.push(t);
        }).observe(seg, { childList: true, characterData: true, subtree: true });
        return true;
      };
      document.addEventListener("DOMContentLoaded", () => {
        if (!watch()) new MutationObserver((_, obs) => watch() && obs.disconnect()).observe(document.body, { childList: true, subtree: true });
      });
    },
    [os, blocks],
  );
  const server = await openMain(page, url, [sessionRoute()]);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  const log = [];
  await page.route("**/api/upload*", async (route) => {
    const request = route.request();
    const u = new URL(request.url());
    const entry = { method: request.method(), path: u.pathname + u.search, headers: request.headers(), requestedAt: Date.now() };
    const n = log.length;
    log.push(entry);
    const a = await answer(entry, n);
    if (a.delay) await new Promise((done) => setTimeout(done, a.delay));
    entry.answeredAt = Date.now();
    if (a.abort) return route.abort("failed");
    return route.fulfill({ status: a.status ?? 200, contentType: "application/json", body: JSON.stringify(a.body ?? {}) });
  });
  return { server, log };
}

/** The centre of a pane in window coordinates, and its box. */
const paneBox = (page, id) =>
  page.evaluate(async (pane) => {
    const { state } = await import(new URL("state.js", location.href).href);
    const found = state.view.panes.find(([p]) => p === pane);
    const c = document.getElementById("term").getBoundingClientRect();
    const [, r] = found;
    const x = c.left + r.x * state.metrics.width;
    const y = c.top + r.y * state.metrics.height;
    const w = r.w * state.metrics.width;
    const h = r.h * state.metrics.height;
    return { x: x + w / 2, y: y + h / 2, left: x, top: y, w, h };
  }, id);

/** A file for a drag: a name, bytes and a type. */
const file = (name, text = name, type = "text/plain") => ({ name, bytes: Array.from(new TextEncoder().encode(text)), type });

/**
 * Fire one drag event at a window point the way a browser does: a `DragEvent` on the element under the point, carrying a
 * `DataTransfer` with `files` (or with `text` / `uri` and no files). The same transfer is used for a whole drag (`key`).
 * Returns whether the page prevented the default, and the drop effect it chose.
 */
const drag = (page, kind, at, { files = [], text = null, uri = null, key = "dt", target = null } = {}) =>
  page.evaluate(
    ([k, p, spec]) => {
      let dt = window[spec.key];
      if (!dt || k === "dragenter") {
        dt = new DataTransfer();
        // A transfer made by a script is not a drag of the system and ignores `dropEffect`; this one keeps what the page set.
        let effect = "none";
        Object.defineProperty(dt, "dropEffect", { get: () => effect, set: (v) => void (effect = v) });
        for (const f of spec.files) dt.items.add(new File([new Uint8Array(f.bytes)], f.name, { type: f.type }));
        if (spec.text !== null) dt.setData("text/plain", spec.text);
        if (spec.uri !== null) dt.setData("text/uri-list", spec.uri);
        window[spec.key] = dt;
      }
      const under = spec.target ? document.querySelector(spec.target) : document.elementFromPoint(p.x, p.y);
      const before = location.href;
      const e = new DragEvent(k, { bubbles: true, cancelable: true, composed: true, dataTransfer: dt, clientX: p.x, clientY: p.y });
      (under ?? document.body).dispatchEvent(e);
      return { prevented: e.defaultPrevented, effect: dt.dropEffect, moved: location.href !== before };
    },
    [kind, at, { files, text, uri, key, target }],
  );

/** Drag `files` over a window point and drop them there. */
async function dropAt(page, at, files, options = {}) {
  await drag(page, "dragenter", at, { files, ...options });
  await drag(page, "dragover", at, { files, ...options });
  return drag(page, "drop", at, { files, ...options });
}

/** The drop target as drawn: its box, its words, its border and whether it can be hit. */
const targetOf = (page) =>
  page.evaluate(() => {
    const nodes = [...document.querySelectorAll(".web-drop")];
    return nodes.map((n) => {
      const r = n.getBoundingClientRect();
      const cs = getComputedStyle(n);
      const folder = n.querySelector(".web-drop-folder");
      return {
        x: r.left, y: r.top, w: r.width, h: r.height,
        label: n.querySelector(".web-drop-label")?.textContent ?? null,
        folder: folder ? folder.textContent : null,
        folderFont: folder ? getComputedStyle(folder).fontFamily : null,
        style: cs.borderTopStyle,
        width: cs.borderTopWidth,
        pointer: cs.pointerEvents,
        role: n.getAttribute("role"),
      };
    });
  });

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  if (currentBackend() === "tauri") {
    await desktopCase(page, url);
  } else {
    await platformCase(page, url);
    await dragCase(page, url);
    await uploadCase(page, url);
    await queueCase(page, url);
    await refusalCase(page, url);
    await quoteCase(page, url);
    await outsideCase(page, url);
    await endedCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The desktop page does not listen for files: a drag over it is the webview's own business, and nothing is requested. */
async function desktopCase(page, url) {
  const requested = [];
  page.on("request", (r) => requested.push(r.url()));
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  const at = await paneBox(page, 1);
  const f = [file("a.txt")];
  assert.equal((await drag(page, "dragenter", at, { files: f })).prevented, false, "the desktop page does not take a drag of files");
  assert.equal((await drag(page, "dragover", at, { files: f })).prevented, false);
  assert.equal((await drag(page, "drop", at, { files: f })).prevented, false);
  assert.deepEqual(await targetOf(page), [], "no drop target is drawn");
  assert.deepEqual(await pastes(page), []);
  assert.ok(!requested.some((u) => /\/webfiles\.js|\/api\/upload/.test(u)), "the module of the browser build is not requested");
}

/** A page that runs against a server but whose platform is not `web` (the tests that play a desktop over the socket) keeps the desktop's way. */
async function platformCase(page, url) {
  await open(page, url, { os: "macos" });
  const at = await paneBox(page, 1);
  const r = await drag(page, "dragover", at, { files: [file("a.txt")] });
  assert.equal(r.prevented, false, "a platform other than web registers no drop listener");
  assert.deepEqual(await targetOf(page), []);
}

/** While a file is dragged over a pane that pane shows the dashed edge and the label; leaving takes them away. */
async function dragCase(page, url) {
  const { log } = await open(page, url, { answer: (_, n) => stored(`f${n}.txt`, 1) });
  const one = await paneBox(page, 1);
  const two = await paneBox(page, 2);
  const f = [file("a.txt")];

  assert.deepEqual(await targetOf(page), [], "nothing before the drag");
  const enter = await drag(page, "dragenter", one, { files: f });
  assert.equal(enter.prevented, true, "the page takes the drag");
  const over = await drag(page, "dragover", one, { files: f });
  assert.equal(over.prevented, true);
  assert.equal(over.effect, "copy", "the cursor says a copy");
  const shown = await targetOf(page);
  assert.equal(shown.length, 1, "one target");
  const t = shown[0];
  assert.ok(Math.abs(t.x - (one.left + 2)) <= 1 && Math.abs(t.y - (one.top + 2)) <= 1 && Math.abs(t.w - (one.w - 4)) <= 1 && Math.abs(t.h - (one.h - 4)) <= 1, `the edge is the pane's box, 2 px inside ${JSON.stringify([t, one])}`);
  assert.equal(t.label, "Drop to upload");
  assert.equal(t.folder, null, "no folder is named before the page has seen one");
  assert.equal(t.style, "dashed", "a dashed edge");
  assert.equal(t.width, "1px", "a hairline");
  assert.equal(t.pointer, "none", "the edge never takes a press");
  assert.equal(t.role, "status");

  // Moving over the other pane moves the edge there; there is never more than one.
  await drag(page, "dragover", two, { files: f });
  const moved = await targetOf(page);
  assert.equal(moved.length, 1);
  assert.ok(Math.abs(moved[0].x - (two.left + 2)) <= 1 && Math.abs(moved[0].w - (two.w - 4)) <= 1, "the edge follows the pane under the pointer");

  // Over nothing that is a pane (the title bar): no edge, no copy.
  const bar = { x: 40, y: 4 };
  const off = await drag(page, "dragover", bar, { files: f });
  assert.equal(off.prevented, true, "the drag is still the page's, so the browser cannot take it");
  assert.equal(off.effect, "none");
  assert.deepEqual(await targetOf(page), []);

  // Back on a pane, then out of the window: the edge goes.
  await drag(page, "dragover", one, { files: f });
  assert.equal((await targetOf(page)).length, 1);
  await page.evaluate(() => {
    const e = new DragEvent("dragleave", { bubbles: true, cancelable: true, relatedTarget: null, clientX: -1, clientY: -1, dataTransfer: window.dt });
    document.documentElement.dispatchEvent(e);
  });
  assert.deepEqual(await targetOf(page), [], "leaving the window takes the edge away");

  // A drag of text shows nothing and is not taken.
  const text = await drag(page, "dragover", one, { text: "hello", key: "dt2" });
  assert.equal(text.prevented, false);
  assert.deepEqual(await targetOf(page), []);
  assert.deepEqual(log, [], "no request is made for a drag that is not dropped");
  assert.deepEqual(await pastes(page), []);
}

/** One file end to end: the request, the words of the status line and the quoted path typed into the pane under the drop. */
async function uploadCase(page, url) {
  const { server, log } = await open(page, url, { answer: (e) => stored(decodeURIComponent(new URL(e.path, "http://x").searchParams.get("name")), 7) });
  const two = await paneBox(page, 2);
  const f = file("a b.txt", "bytes é\n", "text/plain");

  // The second drop names the folder on the label, because the first answer told it.
  const result = await dropAt(page, two, [f]);
  assert.equal(result.prevented, true, "the drop is the page's");
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 1);
  assert.equal(log.length, 1, "one request");
  assert.equal(log[0].method, "PUT");
  assert.equal(log[0].path, "/api/upload?name=a%20b.txt", "the name is in the query, encoded");
  const sent = await page.evaluate(() => window.__sent);
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].bytes, f.bytes, "the body is the file's bytes");
  assert.equal(sent[0].type, "text/plain", "the file's own content type");
  assert.deepEqual([...sent[0].headers].sort(), ["Accept", "X-CSRF-Token"], "the request function adds the token and nothing of the person's data");
  assert.equal(sent[0].token, "csrf-token-1");
  assert.equal(sent[0].credentials, "same-origin");
  assert.equal(sent[0].redirect, "error");
  assert.equal(log[0].headers["x-csrf-token"], "csrf-token-1");

  assert.deepEqual(await pastes(page), [{ pane: 2, text: `'${FOLDER}/a b.txt'`, allow_unsafe: false }], "the quoted path, typed through the paste path of the pane under the drop");
  await page.waitForFunction(() => window.__notices.includes("Uploaded"));
  const said = await notices(page);
  assert.deepEqual(said, ["Uploading a b.txt", "Uploaded"]);
  assert.equal(await noticeNow(page), "Uploaded");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("key")), [], "nothing was typed after the path: no Enter");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the pane keeps the keyboard");

  await drag(page, "dragenter", two, { files: [f], key: "dt3" });
  const second = await targetOf(page);
  assert.equal(second.length, 1);
  assert.equal(second[0].folder, FOLDER, "the folder the server named");
  assert.match(second[0].folderFont, /mono/i, "in mono");
  assert.equal(second[0].label, "Drop to upload");
  await drag(page, "dragleave", { x: -1, y: -1 }, { key: "dt3", files: [f] });
  const l = leftover(server);
  assert.deepEqual(l.foreign, []);
  assert.deepEqual(l.unscripted, []);
  assert.equal(await page.evaluate(() => Object.keys(localStorage).concat(Object.keys(sessionStorage)).length), 0, "no Web Storage use");
}

/** Several files go one after the other, 16 at most, each path typed; a second drop waits for the first. */
async function queueCase(page, url) {
  const { log } = await open(page, url, {
    answer: (e, n) => ({ delay: n === 0 ? 250 : 0, ...stored(decodeURIComponent(new URL(e.path, "http://x").searchParams.get("name")), n + 1) }),
  });
  const one = await paneBox(page, 1);
  const names = ["a.txt", "b.txt", "c d.txt"];
  await dropAt(page, one, names.map((n) => file(n, `data ${n}`)));
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 3);
  assert.deepEqual(log.map((e) => `${e.method} ${e.path}`), ["PUT /api/upload?name=a.txt", "PUT /api/upload?name=b.txt", "PUT /api/upload?name=c%20d.txt"]);
  assert.ok(log[1].requestedAt >= log[0].answeredAt, "the second request starts after the first was answered");
  assert.ok(log[2].requestedAt >= log[1].answeredAt, "and the third after the second");
  const sent = await page.evaluate(() => window.__sent.map((s) => [s.method, s.headers.slice().sort(), s.bytes.length]));
  assert.deepEqual(sent, [["PUT", ["Accept", "X-CSRF-Token"], 10], ["PUT", ["Accept", "X-CSRF-Token"], 10], ["PUT", ["Accept", "X-CSRF-Token"], 12]]);
  assert.deepEqual(
    (await pastes(page)).map((p) => [p.pane, p.text]),
    [[1, `'${FOLDER}/a.txt'`], [1, ` '${FOLDER}/b.txt'`], [1, ` '${FOLDER}/c d.txt'`]],
    "three quoted paths for the pane of the drop, separated by a space, none followed by Enter",
  );
  await page.waitForFunction(() => window.__notices.at(-1) === "Uploaded 3 files");
  assert.deepEqual(await notices(page), ["Uploading a.txt", "Uploading b.txt", "Uploading c d.txt", "Uploaded 3 files"]);

  // 17 files: 16 are sent and the page says why not the last.
  const before = log.length;
  const many = Array.from({ length: 17 }, (_, i) => file(`m${i}.txt`, "x"));
  await dropAt(page, one, many);
  await page.waitForFunction(() => window.__notices.at(-1)?.includes("Only 16 files at a time."));
  assert.equal(log.length - before, 16, "16 uploads");
  assert.equal(log.at(-1).path, "/api/upload?name=m15.txt");
  assert.ok(!log.some((e) => e.path.includes("m16")), "the 17th is not sent");
  assert.equal(await noticeNow(page), "Uploaded 16 files. Only 16 files at a time.");

  // Folders are left out, and said; a drop of folders only sends nothing.
  const n2 = log.length;
  await dropAt(page, one, [file("dir-x"), file("ok.txt", "x")]);
  await page.waitForFunction(() => window.__notices.at(-1) === "Uploaded. Folders are not uploaded.");
  assert.equal(log.length - n2, 1, "the file went, the folder did not");
  assert.equal(log.at(-1).path, "/api/upload?name=ok.txt");
  const n3 = log.length;
  const only = await dropAt(page, one, [file("dir-y")]);
  assert.equal(only.prevented, true);
  await page.waitForFunction(() => window.__notices.at(-1) === "Folders are not uploaded.");
  assert.equal(log.length, n3, "nothing is sent for a folder");
}

/** Every refusal says why, in words, and types nothing; a network failure says the plain one. */
async function refusalCase(page, url) {
  const table = [
    [{ status: 400, body: { code: "bad_request", field: "size" } }, "too large"],
    [{ status: 400, body: { code: "bad_request", field: "name" } }, "name not allowed"],
    [{ status: 413, body: { code: "too_large" } }, "too large"],
    [{ status: 423, body: { code: "locked" } }, "secrets are locked"],
    [{ status: 429, body: { code: "rate" } }, "try again"],
    [{ status: 503, body: { code: "busy" } }, "try again"],
    [{ status: 401, body: { code: "unauthorized" } }, "sign in again"],
    [{ status: 403, body: { code: "forbidden" } }, "sign in again"],
    [{ status: 500, body: { code: "internal" } }, "upload failed"],
    [{ abort: true }, "upload failed"],
    [{ status: 200, body: { path: "relative", size: 1 } }, "upload failed"],
  ];
  const { log } = await open(page, url, { answer: (_, n) => table[n][0] });
  const one = await paneBox(page, 1);
  for (const [i, [, words]] of table.entries()) {
    await dropAt(page, one, [file("r.txt")]);
    await page.waitForFunction((n) => window.__notices.filter((t) => t.startsWith("Not uploaded")).length === n, i + 1);
    assert.equal(await noticeNow(page), `Not uploaded: ${words}.`, `request ${i + 1}`);
    await settle(page);
  }
  assert.equal(log.length, table.length);
  assert.deepEqual(await pastes(page), [], "a refusal types nothing");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the keyboard stayed with the terminal");
}

/** The returned path reaches the pane quoted; a newline in it goes through the paste confirmation of the page. */
async function quoteCase(page, url) {
  const paths = ["/home/p/it's a -x.txt", "/home/p/-rf", "/home/p/$(touch pwn).txt"];
  const { log } = await open(page, url, { answer: (_, n) => ({ status: 200, body: { path: paths[n] ?? "/home/p/a\nb.txt", size: 1 } }) });
  await page.evaluate(() => window.__MOCK__.on("paste", (a) => (a.allow_unsafe || !a.text.includes("\n") ? "written" : "rejected")));
  const one = await paneBox(page, 1);
  await dropAt(page, one, paths.map((_, i) => file(`q${i}`)));
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 3);
  assert.deepEqual((await pastes(page)).map((p) => p.text), [`'/home/p/it'\\''s a -x.txt'`, ` '/home/p/-rf'`, ` '/home/p/$(touch pwn).txt'`]);

  await dropAt(page, one, [file("nl")]);
  await page.locator('[role="dialog"]').waitFor({ state: "visible" });
  const asked = await pastes(page);
  assert.deepEqual(asked.at(-1), { pane: 1, text: "'/home/p/a\nb.txt'", allow_unsafe: false }, "quoted, and the core was asked first");
  assert.equal(asked.length, 4, "nothing more was sent before the person answered");
  await page.keyboard.press("Escape");
  await page.locator('[role="dialog"]').waitFor({ state: "detached" });
  assert.equal((await pastes(page)).length, 4, "cancelled: no second paste");
  assert.equal(log.length, 4);

  await dropAt(page, one, [file("nl2")]);
  await page.locator('[role="dialog"]').waitFor({ state: "visible" });
  await page.getByRole("button", { name: "Paste" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 6);
  assert.deepEqual((await pastes(page)).at(-1), { pane: 1, text: "'/home/p/a\nb.txt'", allow_unsafe: true });
  await page.waitForFunction(() => document.activeElement?.id === "ime");
}

/** A drop outside every pane, of text, or on a pane that runs no shell uploads nothing, and the browser never navigates. */
async function outsideCase(page, url) {
  const { log } = await open(page, url, {
    blocks: [{ pane: 1, kind: "files", source: "local", path: "/home/p", source_label: "This computer" }],
  });
  const before = await page.evaluate(() => location.href);
  const f = [file("a.txt")];
  const bar = { x: 40, y: 4 };
  const outside = await dropAt(page, bar, f);
  assert.equal(outside.prevented, true, "a drop on the title bar is taken, so the browser does not open the file");
  assert.equal(await page.evaluate(() => location.href), before, "the address is the same");

  const status = await dropAt(page, { x: 40, y: 640 - 8 }, f, { key: "dt4" });
  assert.equal(status.prevented, true, "and on the status line");

  // A pane that is a file browser takes no upload: no edge, no copy cursor, nothing sent. The browser still does not navigate.
  const block = await paneBox(page, 1);
  await drag(page, "dragenter", block, { files: f, key: "dt5" });
  const over = await drag(page, "dragover", block, { files: f, key: "dt5" });
  assert.equal(over.prevented, true);
  assert.equal(over.effect, "none");
  assert.deepEqual(await targetOf(page), [], "no edge on a pane that runs no shell");
  const drop = await drag(page, "drop", block, { files: f, key: "dt5" });
  assert.equal(drop.prevented, true);
  assert.equal(await page.evaluate(() => location.href), before);

  // Text, and a link from another page: the text is left to the page's fields; a link is not allowed to take the page away.
  const shell = await paneBox(page, 2);
  const text = await dropAt(page, shell, [], { text: "hello", key: "dt6" });
  assert.equal(text.prevented, false, "text is no file; fields of the page keep their own drop");
  const link = await dropAt(page, shell, [], { uri: "https://example.org/", key: "dt7" });
  assert.equal(link.prevented, true, "a dropped link does not navigate the page");
  assert.equal(await page.evaluate(() => location.href), before);

  assert.deepEqual(log, [], "nothing was uploaded");
  assert.deepEqual(await pastes(page), []);
  assert.deepEqual(await page.evaluate(() => window.__sent), []);
}

/** When the session is over the page uploads nothing, and still does not let the browser leave. */
async function endedCase(page, url) {
  const { log } = await open(page, url);
  await page.evaluate(() => window.__MOCK__.emit("hw-session", { state: "ended" }));
  await page.locator(".web-ended").waitFor({ state: "visible" });
  const before = await page.evaluate(() => location.href);
  const shell = await paneBox(page, 2);
  const r = await dropAt(page, shell, [file("a.txt")]);
  assert.equal(r.prevented, true, "the drop is still the page's");
  await settle(page);
  assert.equal(await page.evaluate(() => location.href), before);
  assert.deepEqual(log, [], "no request after the session ended");
  assert.deepEqual(await pastes(page), []);
  assert.deepEqual(await targetOf(page), []);
}
