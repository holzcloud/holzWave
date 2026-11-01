// Files in the browser build (docs/design.md 6.30): a file dropped on a pane is sent to the server with the page's request function
// and its path, quoted for the shell, is typed into the pane like a paste; a drop never takes the page away from the person.
// The desktop has no such target: it does not listen for files at all. The server is scripted: `PUT /api/upload?name=` answers from
// a table, and the page's request function is wrapped so that the bytes of the body can be compared.
import assert from "node:assert/strict";
import { currentBackend } from "./lib.mjs";
import { leftover, openMain, sessionRoute } from "./websession-lib.mjs";

const FOLDER = "/home/p/uploads";

const ENTRIES = [
  { name: "docs", kind: "dir", link: false, size: 0, modified: 1760000000, hidden: false },
  { name: "mockup.png", kind: "file", link: false, size: 412000, modified: 1760000000, hidden: false },
  { name: "notes.md", kind: "file", link: false, size: 1536, modified: 1760000000, hidden: false },
];
const VIEW = {
  workspaces: [{ name: "main", active: true, panes: 2 }],
  tabs: [{ title: "files", active: true, focus: 1, panes: 2 }],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
  pane_names: [],
  windows: 1,
};
const FILES = { pane: 1, kind: "files", source: "local", path: "/home/holz", source_label: "This computer" };
const PREVIEW = { pane: 2, kind: "preview", source: "local", path: "/home/holz/mockup.png", source_label: "This computer" };
const SENTENCE = "Download works for images, video, audio and PDF files.";

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
async function open(page, url, { answer = () => stored("x", 0), os = "web", blocks = [], focus = 1 } = {}) {
  await page.addInitScript(
    ([o, b, f, entries, view]) => {
      window.__MOCK__.on("app_info", () => ({ os: o, version: "t", hostname: "h" }));
      window.__MOCK__.on("pane_blocks", () => structuredClone(b));
      // `window.__EXTRA[path]` are the files that "arrived" in a folder since the page first listed it.
      window.__MOCK__.on("files_list", ({ path }) => ({ path, parent: "/home", entries: [...entries, ...(window.__EXTRA?.[path] ?? [])], truncated: false }));
      window.__MOCK__.on("file_open", ({ path }) => ({
        content: "binary", name: path.split("/").pop(), mime: "application/octet-stream", size: 3, text: null, html: null, language: null, modified: 1, digest: null,
      }));
      // Grants are `tok-1`, `tok-2`, ...; `window.__REFUSE` makes the core refuse, as it does for a kind it does not grant.
      let granted = 0;
      window.__MOCK__.on("file_grant", () => {
        if (window.__REFUSE) throw window.__REFUSE;
        granted++;
        return { url: `/files/tok-${granted}`, token: `tok-${granted}`, mime: "image/png", size: 1 };
      });
      window.__MOCK__.on("file_revoke", () => null);
      // Why the core refused: `window.__WHY` is the stable code (a kind it does not grant by default), `window.__WHY_RAW` an answer of any
      // shape, `window.__WHY_FAIL` makes the question itself fail.
      window.__MOCK__.on("file_grant_why", () => {
        if (window.__WHY_FAIL) throw window.__WHY_FAIL;
        return window.__WHY_RAW !== undefined ? window.__WHY_RAW : { code: window.__WHY ?? "kind" };
      });
      if (b.length > 0) window.__MOCK__.setView({ ...view, focus: f });
      // The entries API of a real drop says which item is a folder; a made-up drop has none, so a file named `dir-...` plays one.
      DataTransferItem.prototype.webkitGetAsEntry = function () {
        const f = this.getAsFile();
        const directory = f !== null && f.name.startsWith("dir-");
        return { isDirectory: directory, isFile: !directory };
      };
      // Init scripts of earlier cases run again on every load: the recorders are made once, the answers above come from the newest script.
      if (window.__recording) return;
      window.__recording = true;
      // A download is a click on a link with the `download` attribute: record it and stop the browser from following it.
      window.__links = [];
      document.addEventListener(
        "click",
        (e) => {
          const a = e.target instanceof Element ? e.target.closest("a") : null;
          if (a === null || !a.hasAttribute("download")) return;
          window.__links.push({ href: a.href, download: a.getAttribute("download"), rel: a.getAttribute("rel"), target: a.getAttribute("target"), inDocument: a.isConnected });
          e.preventDefault();
        },
        true,
      );
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
            contentType: (init.headers ?? {})["Content-Type"] ?? null,
            type: init.body.type,
            bytes: Array.from(new Uint8Array(await init.body.arrayBuffer())),
            credentials: init.credentials,
            redirect: init.redirect,
            mode: init.mode,
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
    [os, blocks, focus, ENTRIES, VIEW],
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
    await takesNothingCase(page, url);
    await chatDropCase(page, url);
    await browserDropCase(page, url);
    await endedCase(page, url);
    await menuCase(page, url);
    await keyboardMenuCase(page, url);
    await closeCase(page, url);
    await refusedGrantCase(page, url);
    await paletteCase(page, url);
    await revokeCase(page, url);
    await platformMenuCase(page, url);
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

  // The file browser of the desktop has no row menu, and a context-menu gesture does not request the module.
  await page.addInitScript(([entries, view]) => {
    window.__MOCK__.on("pane_blocks", () => [{ pane: 1, kind: "files", source: "local", path: "/home/holz", source_label: "This computer" }]);
    window.__MOCK__.on("files_list", ({ path }) => ({ path, parent: "/home", entries, truncated: false }));
    window.__MOCK__.setView(view);
  }, [ENTRIES, VIEW]);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.locator(".block-files .bk-row").first().waitFor();
  await rowOf(page, "mockup.png").click({ button: "right" });
  await settle(page);
  assert.equal(await menus(page).count(), 0, "no row menu on the desktop");
  await page.keyboard.press("Shift+F10");
  await settle(page);
  assert.equal(await menus(page).count(), 0);
  assert.equal(await registryHas(page, "files.download"), false, "no Download action");
  assert.ok(!(await paletteTitles(page)).includes("Download"), "the palette does not list Download");
  assert.ok(!requested.some((u) => /\/webfiles\.js/.test(u)), "no context-menu gesture requested the web module");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("file_grant")), []);
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
  // The server takes an upload only as a byte stream, so the request names that type itself and the file's own type stays off the wire.
  assert.deepEqual([...sent[0].headers].sort(), ["Accept", "Content-Type", "X-CSRF-Token"], "the request function adds the token and the byte-stream type and nothing of the person's data");
  assert.equal(sent[0].contentType, "application/octet-stream", "the upload is sent as a byte stream whatever type the file has");
  assert.equal(sent[0].token, "csrf-token-1");
  assert.equal(sent[0].credentials, "same-origin");
  assert.equal(sent[0].redirect, "error");
  assert.equal(sent[0].mode, "cors", "the mode that reaches fetch: every engine then sends the real Origin header");
  assert.equal(log[0].headers["x-csrf-token"], "csrf-token-1");
  // What the page puts in the options is not what the server sees: the engine decides. The route has the headers the browser really
  // sent, and an explicit type must beat the Blob's own `text/plain` there too, or the server answers 400 to the upload.
  assert.equal(log[0].headers["content-type"], "application/octet-stream", "the browser sent a byte stream, not the file's type");

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
  const headers = ["Accept", "Content-Type", "X-CSRF-Token"];
  assert.deepEqual(sent, [["PUT", headers, 10], ["PUT", headers, 10], ["PUT", headers, 12]]);
  assert.deepEqual(log.map((e) => e.headers["content-type"]), ["application/octet-stream", "application/octet-stream", "application/octet-stream"], "the browser sent a byte stream for each file");
  assert.deepEqual(await page.evaluate(() => window.__sent.map((s) => s.mode)), ["cors", "cors", "cors"]);
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

/** A drop outside every pane, of text, or on a pane that takes nothing (a preview, the monitor, a tool view) uploads nothing, and the browser never navigates. */
async function outsideCase(page, url) {
  const { log } = await open(page, url, {
    blocks: [{ pane: 1, kind: "preview", source: "local", path: "/home/p/mockup.png", source_label: "This computer" }],
  });
  const before = await page.evaluate(() => location.href);
  const f = [file("a.txt")];
  const bar = { x: 40, y: 4 };
  const outside = await dropAt(page, bar, f);
  assert.equal(outside.prevented, true, "a drop on the title bar is taken, so the browser does not open the file");
  assert.equal(await page.evaluate(() => location.href), before, "the address is the same");

  const status = await dropAt(page, { x: 40, y: 640 - 8 }, f, { key: "dt4" });
  assert.equal(status.prevented, true, "and on the status line");

  // A pane that is a preview takes no upload: no edge, no copy cursor, nothing sent. The browser still does not navigate.
  const block = await paneBox(page, 1);
  await drag(page, "dragenter", block, { files: f, key: "dt5" });
  const over = await drag(page, "dragover", block, { files: f, key: "dt5" });
  assert.equal(over.prevented, true);
  assert.equal(over.effect, "none");
  assert.deepEqual(await targetOf(page), [], "no edge on a pane that takes nothing");
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

const CHAT = { pane: 1, kind: "chat", source: "local", path: "chat-mock-1", source_label: "This computer" };

/** A monitor and a tool view take nothing either: no edge, no copy cursor, nothing sent, and the drop is still the page's. */
async function takesNothingCase(page, url) {
  const blocks = [
    { pane: 1, kind: "monitor", source: "local", path: "", source_label: "This computer" },
    { pane: 1, kind: "integration", source: "kube", path: "get_pods", source_label: "kube" },
  ];
  for (const block of blocks) {
    const { log } = await open(page, url, { blocks: [block] });
    const before = await page.evaluate(() => location.href);
    const at = await paneBox(page, 1);
    const f = [file("a.txt")];
    await drag(page, "dragenter", at, { files: f, key: "dtn" });
    const over = await drag(page, "dragover", at, { files: f, key: "dtn" });
    assert.equal(over.prevented, true, block.kind);
    assert.equal(over.effect, "none", `${block.kind}: the cursor says nothing is taken`);
    assert.deepEqual(await targetOf(page), [], `${block.kind}: no edge`);
    const drop = await drag(page, "drop", at, { files: f, key: "dtn" });
    assert.equal(drop.prevented, true, `${block.kind}: the drop is the page's, so the browser cannot open the file`);
    assert.equal(drop.moved, false);
    assert.equal(await page.evaluate(() => location.href), before);
    await settle(page);
    assert.deepEqual(log, [], `${block.kind}: nothing was uploaded`);
    assert.deepEqual(await pastes(page), []);
  }
}

/** The composer of the chat pane. */
const composer = (page) => page.locator("textarea.chat-input");

/**
 * A file dropped on a chat pane is uploaded and its quoted path is added to the message being written, at the caret, with a space
 * between it and the words before it; nothing is pasted to a shell and nothing is sent.
 */
async function chatDropCase(page, url) {
  // The server stores a file under the name it was sent, except that `big.bin` is too large.
  const named = (entry) => new URL(entry.path, "http://x").searchParams.get("name");
  const { log } = await open(page, url, { blocks: [CHAT], focus: 1, answer: (entry) => (named(entry) === "big.bin" ? { status: 413, body: {} } : stored(named(entry), 3)) });
  await composer(page).waitFor();
  const at = await paneBox(page, 1);
  const f = [file("a.txt")];

  // Over the chat: the edge, the label and the copy cursor.
  await drag(page, "dragenter", at, { files: f, key: "dtc" });
  const over = await drag(page, "dragover", at, { files: f, key: "dtc" });
  assert.equal(over.effect, "copy", "a chat takes files");
  const shown = await targetOf(page);
  assert.equal(shown.length, 1);
  assert.equal(shown[0].label, "Drop to upload");
  assert.equal(shown[0].style, "dashed");
  assert.equal(shown[0].pointer, "none");
  await drag(page, "drop", at, { files: f, key: "dtc" });

  await page.waitForFunction(() => document.querySelector("textarea.chat-input")?.value.includes("a.txt"));
  assert.equal(await composer(page).inputValue(), "'/home/p/uploads/a.txt'", "the quoted path, alone in an empty composer");
  assert.equal(await noticeNow(page), "Uploaded");
  assert.deepEqual(await pastes(page), [], "a chat is no shell: nothing is pasted");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("ai_send")), [], "it is never sent");
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-input", "the keyboard is in the composer");
  assert.deepEqual(await targetOf(page), [], "the edge is gone after the drop");

  // Words before the caret get one space; a path with a quote and a space is quoted as for the shell; the draft counts it.
  await composer(page).fill("look at");
  await dropAt(page, at, [file("b c.txt")], { key: "dtc2" });
  await page.waitForFunction(() => document.querySelector("textarea.chat-input").value.includes("b c.txt"));
  assert.equal(await composer(page).inputValue(), "look at '/home/p/uploads/b c.txt'");
  await composer(page).fill("look at ");
  await dropAt(page, at, [file("it's.txt")], { key: "dtc3" });
  await page.waitForFunction(() => document.querySelector("textarea.chat-input").value.includes("it"));
  assert.equal(await composer(page).inputValue(), "look at '/home/p/uploads/it'\\''s.txt'", "after a space there is no second one");

  // Several files of one drop are separated by a space; the caret in the middle of the text splits it.
  await composer(page).fill("");
  const second = await dropAt(page, at, [file("p.txt"), file("q.txt")], { key: "dtc4" });
  assert.equal(second.prevented, true);
  await page.waitForFunction(() => (document.querySelector("textarea.chat-input").value.match(/'\/home/g) ?? []).length === 2);
  assert.equal(await composer(page).inputValue(), "'/home/p/uploads/p.txt' '/home/p/uploads/q.txt'");
  assert.equal(await noticeNow(page), "Uploaded 2 files");
  assert.deepEqual(await pastes(page), []);
  assert.equal(log.length, 5);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("ai_send")), []);

  // A refusal adds nothing to the message.
  await composer(page).fill("keep this");
  await dropAt(page, at, [file("big.bin")], { key: "dtc5" });
  await page.waitForFunction(() => document.querySelector("#status .notice")?.textContent.startsWith("Not uploaded"));
  assert.equal(await noticeNow(page), "Not uploaded: too large.");
  assert.equal(await composer(page).inputValue(), "keep this");
}

/** `FILES` shows /home/holz, so a file stored in `FOLDER` is elsewhere unless the browser is pointed at that folder. */
/**
 * A file dropped on a file browser is uploaded and then shown there. When the browser lists the folder the file went to, the list is
 * read again and the new row is the selected one; when it lists another folder (or the files of a saved connection) it does not move,
 * and the status line says where the file went. Nothing is typed anywhere.
 */
async function browserDropCase(page, url) {
  // 1. The browser shows the upload folder.
  let { log } = await open(page, url, { blocks: [{ ...FILES, path: FOLDER }], answer: () => stored("new.txt", 3) });
  await page.locator(".block-files .bk-row").first().waitFor();
  assert.equal(await selectedName(page), "docs");
  const at = await paneBox(page, 1);
  const f = [file("new.txt")];
  await drag(page, "dragenter", at, { files: f, key: "dtf" });
  const over = await drag(page, "dragover", at, { files: f, key: "dtf" });
  assert.equal(over.effect, "copy", "a file browser takes files");
  const shown = await targetOf(page);
  assert.equal(shown.length, 1, "the edge is drawn on a file browser");
  assert.equal(shown[0].label, "Drop to upload");
  assert.equal(shown[0].style, "dashed");
  assert.ok(Math.abs(shown[0].x - (at.left + 2)) <= 1 && Math.abs(shown[0].w - (at.w - 4)) <= 1, "the edge is the pane's box, 2 px inside");
  await page.evaluate(() => (window.__EXTRA = { "/home/p/uploads": [{ name: "new.txt", kind: "file", link: false, size: 3, modified: 1760000100, hidden: false }] }));
  const listed = () => page.evaluate(() => window.__MOCK__.callsOf("files_list").length);
  const before = await listed();
  await drag(page, "drop", at, { files: f, key: "dtf" });
  await page.waitForFunction(() => [...document.querySelectorAll(".block-files .bk-row")].some((r) => r.textContent.includes("new.txt")));
  assert.equal(await selectedName(page), "new.txt", "the new file is the selected row");
  assert.equal(await listed(), before + 1, "the list was read again, once");
  assert.equal(await noticeNow(page), "Uploaded");
  assert.deepEqual(await pastes(page), [], "a file browser types nothing");
  assert.equal(log.length, 1);
  assert.deepEqual(await targetOf(page), []);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the keyboard stays in the page's input");
  // The row is in view and has the selection cue (the attribute) in a scroll box that holds it.
  assert.ok(await page.locator('.block-files .bk-row[aria-selected="true"]').evaluate((row) => {
    const box = row.parentElement.getBoundingClientRect();
    const r = row.getBoundingClientRect();
    return r.top >= box.top - 1 && r.bottom <= box.bottom + 1;
  }), "the selected row is visible");

  // 2. The browser shows another folder: it does not move, and the status line names the folder.
  ({ log } = await open(page, url, { blocks: [FILES], answer: () => stored("new.txt", 3) }));
  await page.locator(".block-files .bk-row").first().waitFor();
  const first = await listed();
  const there = await paneBox(page, 1);
  await dropAt(page, there, [file("new.txt")], { key: "dtf2" });
  await page.waitForFunction(() => document.querySelector("#status .notice")?.textContent.startsWith("Uploaded"));
  assert.equal(await noticeNow(page), "Uploaded to /home/p/uploads");
  assert.equal(await selectedName(page), "docs", "the selection did not move");
  assert.equal(await listed(), first, "the list was not read again");
  assert.equal(await page.locator(".block-files .bk-path").textContent(), "/home/holz", "the browser still shows its own folder");
  assert.deepEqual(await pastes(page), []);
  assert.equal(log.length, 1);

  // Two files: the sentence counts them and names the folder once.
  ({ log } = await open(page, url, { blocks: [FILES], answer: (_, n) => stored(`n${n}.txt`, 3) }));
  await page.locator(".block-files .bk-row").first().waitFor();
  await dropAt(page, await paneBox(page, 1), [file("n0.txt"), file("n1.txt")], { key: "dtf3" });
  await page.waitForFunction(() => document.querySelector("#status .notice")?.textContent.startsWith("Uploaded 2"));
  assert.equal(await noticeNow(page), "Uploaded 2 files to /home/p/uploads");

  // 3. A browser of a saved connection never shows the folder of this server.
  ({ log } = await open(page, url, { blocks: [{ ...FILES, source: "talos-1", path: FOLDER, source_label: "talos-1" }], answer: () => stored("new.txt", 3) }));
  await page.locator(".block-files .bk-row").first().waitFor();
  const remoteLists = await listed();
  await dropAt(page, await paneBox(page, 1), [file("new.txt")], { key: "dtf4" });
  await page.waitForFunction(() => document.querySelector("#status .notice")?.textContent.startsWith("Uploaded"));
  assert.equal(await noticeNow(page), "Uploaded to /home/p/uploads", "the same folder name on another computer is not this folder");
  assert.equal(await listed(), remoteLists);

  // 4. A refusal types and shows nothing; the browser stays as it was.
  ({ log } = await open(page, url, { blocks: [{ ...FILES, path: FOLDER }], answer: () => ({ status: 413, body: {} }) }));
  await page.locator(".block-files .bk-row").first().waitFor();
  const kept = await listed();
  await dropAt(page, await paneBox(page, 1), [file("big.bin")], { key: "dtf5" });
  await page.waitForFunction(() => document.querySelector("#status .notice")?.textContent.startsWith("Not uploaded"));
  assert.equal(await noticeNow(page), "Not uploaded: too large.");
  assert.equal(await listed(), kept);
  assert.equal(await selectedName(page), "docs");
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

// ---- download ---------------------------------------------------------------------------------------------------------

const menus = (page) => page.locator('[role="menu"]');
const rowOf = (page, name) => page.locator(".block-files .bk-row", { hasText: name });
const links = (page) => page.evaluate(() => window.__links);
const active = (page) => page.evaluate(() => document.activeElement?.id || document.activeElement?.className || "");
const grants = (page) => page.evaluate(() => window.__MOCK__.callsOf("file_grant"));
const revokes = (page) => page.evaluate(() => window.__MOCK__.callsOf("file_revoke").map((c) => c.token));
const selectedName = (page) => page.locator('.block-files .bk-row[aria-selected="true"] .bk-name').textContent();
const registryHas = (page, id) =>
  page.evaluate(async (i) => (await import(new URL("actions.js", location.href).href)).listActions().some((a) => a.id === i), id);
const paletteTitles = async (page) => {
  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("palette.open"));
  await page.waitForSelector(".palette-row, .palette-empty:not([hidden])");
  const titles = await page.evaluate(() => [...document.querySelectorAll(".palette-row .palette-title")].map((n) => n.textContent));
  return titles;
};
const closePalette = async (page) => {
  await page.keyboard.press("Escape");
  await page.locator(".palette-backdrop.open").waitFor({ state: "detached" }).catch(() => {});
  await page.waitForFunction(() => document.activeElement?.id === "ime");
};

/** Open the browser of pane 1 and wait for its rows. */
async function openBrowser(page, url, options = {}) {
  await open(page, url, { blocks: [FILES], ...options });
  await page.locator(".block-files .bk-row").first().waitFor();
}

/** The context-menu gesture on a file row opens a menu with one entry, selects the row and grants nothing; a folder row opens none. */
async function menuCase(page, url) {
  await openBrowser(page, url);
  await rowOf(page, "docs").click({ button: "right" });
  await settle(page);
  assert.equal(await menus(page).count(), 0, "a folder row opens no menu");

  await rowOf(page, "mockup.png").click({ button: "right" });
  await menus(page).waitFor({ state: "visible" });
  assert.equal(await menus(page).count(), 1);
  assert.equal(await selectedName(page), "mockup.png", "the row became the selected one");
  const items = await menus(page).locator('[role="menuitem"]').allTextContents();
  assert.deepEqual(items, ["Download"], "one entry");
  assert.equal(await menus(page).getAttribute("aria-label"), "Actions for mockup.png");
  assert.equal(await page.evaluate(() => document.activeElement?.getAttribute("role")), "menuitem", "the entry has the keyboard");
  const box = await menus(page).evaluate((m) => {
    const r = m.getBoundingClientRect();
    const item = m.querySelector('[role="menuitem"]').getBoundingClientRect();
    const cs = getComputedStyle(m);
    return { r: r.toJSON(), itemHeight: item.height, shadow: cs.boxShadow, radius: cs.borderTopLeftRadius, border: cs.borderTopWidth, fixed: cs.position };
  });
  assert.ok(box.itemHeight >= 28, `the entry is at least 28 px high (${box.itemHeight})`);
  assert.notEqual(box.shadow, "none", "a floating layer has the one shadow");
  assert.equal(box.border, "1px");
  assert.equal(box.fixed, "fixed");
  assert.ok(box.r.left >= 0 && box.r.top >= 0 && box.r.right <= 1000 && box.r.bottom <= 640, "inside the window");
  assert.deepEqual(await grants(page), [], "opening the menu grants nothing");
  assert.deepEqual(await links(page), []);

  // ArrowDown and ArrowUp keep the one entry focused and do not move the selection of the list.
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("End");
  assert.equal(await page.evaluate(() => document.activeElement?.getAttribute("role")), "menuitem");
  assert.equal(await selectedName(page), "mockup.png", "the list did not move under the menu");

  // Enter runs it: one grant for that source and path, then a click on a same-origin link that downloads under the file's name.
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__links.length === 1);
  assert.deepEqual(await grants(page), [{ source: "local", path: "/home/holz/mockup.png" }]);
  const [link] = await links(page);
  assert.equal(link.href, `${new URL(url).origin}/files/tok-1`, "the address of the grant, on the page's own origin");
  assert.equal(link.download, "mockup.png");
  assert.equal(link.target, null, "no new tab");
  assert.equal(await menus(page).count(), 0, "the menu is gone");
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("key")), [], "no key reached the terminal");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("block_open")), [], "Enter did not open a preview");

  // A click on the entry does the same.
  await rowOf(page, "notes.md").click({ button: "right" });
  await menus(page).waitFor({ state: "visible" });
  await menus(page).getByRole("menuitem", { name: "Download" }).click();
  await page.waitForFunction(() => window.__links.length === 2);
  assert.equal((await links(page))[1].download, "notes.md");
  assert.equal((await grants(page))[1].path, "/home/holz/notes.md");
  await page.waitForFunction(() => document.activeElement?.id === "ime");
}

/** The context-menu key and Shift+F10 open it at the selected file row, and Escape closes it and gives the keyboard back. */
async function keyboardMenuCase(page, url) {
  await openBrowser(page, url);
  await page.waitForFunction(() => document.activeElement?.id === "ime" || document.getElementById("ime") !== null);
  await page.evaluate(() => document.getElementById("ime").focus());
  // The first row is a folder: no menu.
  await page.keyboard.press("Shift+F10");
  await settle(page);
  assert.equal(await menus(page).count(), 0, "a folder is selected");
  await page.keyboard.press("ArrowDown");
  assert.equal(await selectedName(page), "mockup.png");
  const row = await rowOf(page, "mockup.png").boundingBox();
  await page.keyboard.press("Shift+F10");
  await menus(page).waitFor({ state: "visible" });
  const at = await menus(page).boundingBox();
  assert.ok(at.y >= row.y - 2 && at.y <= row.y + row.height + 2, `the menu stands at the row ${JSON.stringify([at, row])}`);
  assert.ok(at.x >= row.x && at.x <= row.x + row.width, "and inside its width");
  const ring = await page.evaluate(() => {
    const cs = getComputedStyle(document.activeElement);
    return [cs.outlineStyle, cs.outlineWidth];
  });
  assert.deepEqual(ring, ["solid", "2px"], "the keyboard focus ring is 2 px");
  await page.keyboard.press("Escape");
  await menus(page).waitFor({ state: "detached" });
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  assert.deepEqual(await grants(page), [], "nothing was granted");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("key")), [], "Escape did not reach the terminal");

  await page.keyboard.press("ContextMenu");
  await menus(page).waitFor({ state: "visible" });
  assert.equal(await menus(page).locator('[role="menuitem"]').count(), 1);
  await page.keyboard.press("Escape");
  await menus(page).waitFor({ state: "detached" });
  assert.equal(await selectedName(page), "mockup.png");
}

/** A click elsewhere, a blur and a layout change close it, and the keyboard is back in the terminal each time. */
async function closeCase(page, url) {
  await openBrowser(page, url);
  const open = async () => {
    await rowOf(page, "mockup.png").click({ button: "right" });
    await menus(page).waitFor({ state: "visible" });
  };
  await open();
  const two = await paneBox(page, 2);
  await page.mouse.click(two.x, two.y);
  await menus(page).waitFor({ state: "detached" });
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  await open();
  await page.evaluate(() => document.activeElement.blur());
  await menus(page).waitFor({ state: "detached" });
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  // The focus moving to another pane is no layout change; the panes changing their size is.
  await open();
  const move = (to) =>
    page.evaluate(async (t) => {
      const { state } = await import(new URL("state.js", location.href).href);
      const { renderOverlay } = await import(new URL("overlay.js", location.href).href);
      state.view = t === "focus"
        ? { ...state.view, focus: 2 }
        : { ...state.view, panes: state.view.panes.map(([id, r]) => (id === 1 ? [id, { ...r, w: 0.4 }] : [id, { ...r, x: 0.4, w: 0.6 }])) };
      renderOverlay();
    }, to);
  await move("focus");
  await settle(page);
  assert.equal(await menus(page).count(), 1, "the focus moving does not close it");
  await move("panes");
  await menus(page).waitFor({ state: "detached" });
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  await open();
  await page.setViewportSize({ width: 900, height: 600 });
  await menus(page).waitFor({ state: "detached" });
  await page.setViewportSize({ width: 1000, height: 640 });
  assert.deepEqual(await grants(page), [], "closing never grants");
  assert.deepEqual(await links(page), []);
  assert.equal(await menus(page).count(), 0);
}

/** The words of every reason a grant is refused, said in the status line; the page makes no download and the question is asked after a refusal only. */
async function refusedGrantCase(page, url) {
  await openBrowser(page, url);
  const asked = () => page.evaluate(() => window.__MOCK__.callsOf("file_grant_why"));
  /** Make the core refuse with `refusal` and the question answer `why` (`raw` replaces the whole answer), press Download on notes.md and return what the status line then says. */
  const refuse = async ({ code = "kind", raw, fail = null }) => {
    await page.evaluate(
      ([c, r, f]) => {
        window.__REFUSE = "zq-secret core text of the refusal";
        window.__WHY = c;
        window.__WHY_RAW = r;
        window.__WHY_FAIL = f;
        document.querySelector("#status .notice").textContent = "";
      },
      [code, raw, fail],
    );
    await rowOf(page, "notes.md").click({ button: "right" });
    await menus(page).getByRole("menuitem", { name: "Download" }).click();
    await page.waitForFunction(() => document.querySelector("#status .notice")?.textContent !== "");
    await page.waitForFunction(() => document.activeElement?.id === "ime");
    return noticeNow(page);
  };

  // A download that works never asks why.
  await rowOf(page, "mockup.png").click({ button: "right" });
  await menus(page).getByRole("menuitem", { name: "Download" }).click();
  await page.waitForFunction(() => window.__links.length === 1);
  assert.deepEqual(await asked(), [], "no refusal, no question");

  // Each stable code has its sentence; the kind keeps the sentence it always had.
  const reasons = [
    ["kind", SENTENCE],
    ["locked", "Not downloaded: secrets are locked."],
    ["not_found", "Not downloaded: file not found."],
    ["denied", "Not downloaded: not allowed to read it."],
    ["other", "Not downloaded: download failed."],
  ];
  for (const [code, sentence] of reasons) assert.equal(await refuse({ code }), sentence, code);
  assert.equal((await asked()).length, reasons.length, "one question for each refusal");
  assert.deepEqual((await asked()).at(-1), { source: "local", path: "/home/holz/notes.md" }, "the question names the same file and nothing else");

  // The question can fail, or be answered with something that is not a code: the plain sentence, never a guess and never the core's text.
  const plain = "Not downloaded: download failed.";
  assert.equal(await refuse({ fail: "zq-secret no such file" }), plain, "a failing question");
  assert.equal(await refuse({ raw: "locked" }), plain, "an answer that is a string");
  assert.equal(await refuse({ raw: { code: "too_large", text: "zq-secret" } }), plain, "a code the page does not know");
  assert.equal(await refuse({ raw: null }), plain, "no answer");
  assert.ok(!(await noticeNow(page)).includes("zq-secret"), "the core's text never reaches the status line");

  assert.equal((await links(page)).length, 1, "only the first download made a link; a refusal makes none");
  assert.equal((await grants(page)).length, 1 + reasons.length + 4, "every press asked for a grant once");
  assert.deepEqual(await revokes(page), [], "there was no grant to end");
  await page.evaluate(() => {
    window.__REFUSE = undefined;
    window.__WHY_FAIL = null;
    window.__WHY_RAW = undefined;
    window.__WHY = undefined;
  });
}

/** The palette lists `Download` only while the focused pane is a file browser with a file selected, or a preview. */
async function paletteCase(page, url) {
  await openBrowser(page, url);
  assert.equal(await registryHas(page, "files.download"), true, "the entry exists once a file pane does");
  assert.equal(await selectedName(page), "docs");
  assert.ok(!(await paletteTitles(page)).includes("Download"), "a folder is selected: not listed");
  await closePalette(page);
  await page.keyboard.press("ArrowDown");
  assert.equal(await selectedName(page), "mockup.png");
  assert.ok((await paletteTitles(page)).includes("Download"), "a file is selected: listed");
  assert.deepEqual(await grants(page), [], "listing it grants nothing");
  await page.locator(".palette-row", { hasText: "Download" }).first().click();
  await page.waitForFunction(() => window.__links.length === 1);
  assert.deepEqual(await grants(page), [{ source: "local", path: "/home/holz/mockup.png" }]);
  assert.equal((await links(page))[0].download, "mockup.png");
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  // A preview that has the focus: listed, and it downloads its own file.
  await open(page, url, { blocks: [FILES, PREVIEW], focus: 2 });
  await page.locator(".block-preview").waitFor();
  await settle(page);
  assert.ok((await paletteTitles(page)).includes("Download"), "a preview is focused: listed");
  await page.locator(".palette-input").fill("Download");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__links.length === 1);
  assert.deepEqual(await grants(page), [{ source: "local", path: "/home/holz/mockup.png" }]);
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  // A shell pane has the focus: the entry is not listed (it is registered, as every action is).
  await open(page, url, { blocks: [FILES], focus: 2 });
  await page.locator(".block-files .bk-row").first().waitFor();
  assert.ok(!(await paletteTitles(page)).includes("Download"), "a shell is focused: not listed");
  await closePalette(page);
}

/** At most 16 grants stay: the 17th ends the first, and the page going away ends all that are left. */
async function revokeCase(page, url) {
  await openBrowser(page, url);
  await page.keyboard.press("ArrowDown");
  const run = () => page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("files.download"));
  for (let i = 1; i <= 16; i++) {
    await run();
    await page.waitForFunction((n) => window.__links.length === n, i);
  }
  assert.deepEqual(await revokes(page), [], "16 grants stay");
  await run();
  await page.waitForFunction(() => window.__links.length === 17);
  assert.deepEqual(await revokes(page), ["tok-1"], "the 17th ends the oldest");
  assert.equal((await grants(page)).length, 17);
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await page.waitForFunction(() => window.__MOCK__.callsOf("file_revoke").length === 17);
  assert.deepEqual(await revokes(page), Array.from({ length: 17 }, (_, i) => `tok-${i + 1}`), "pagehide ends every one that was left, oldest first");
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await settle(page);
  assert.equal((await revokes(page)).length, 17, "a second pagehide has nothing to end");
}

/** A page that runs against a server but whose platform is not `web` gets no menu and no entry. */
async function platformMenuCase(page, url) {
  const requested = [];
  page.on("request", (r) => requested.push(r.url()));
  await openBrowser(page, url, { os: "macos" });
  await rowOf(page, "mockup.png").click({ button: "right" });
  await settle(page);
  assert.equal(await menus(page).count(), 0, "no row menu");
  assert.equal(await registryHas(page, "files.download"), false);
  await page.keyboard.press("Shift+F10");
  await settle(page);
  assert.equal(await menus(page).count(), 0);
  assert.ok(!requested.some((u) => /\/webfiles\.js/.test(u)), "the web module is not requested");
  assert.deepEqual(await grants(page), []);
}
