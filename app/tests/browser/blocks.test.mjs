// File browsers and previews (design.md sections 6.3 and 6.5): panes that show files instead of a terminal. The list is
// keyboard driven, a file opens in a preview beside it, Markdown is copied into the page through an allow-list and never
// as innerHTML, images and video are shown from the granted URL of the `holzfile` scheme and the grant ends with the
// preview. The browser stub plays the core.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await actionsCase(page, url);
  await browserCase(page, url);
  await previewCase(page, url);
  await pdfCase(page, url);
  await safeHtmlCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const ENTRIES = [
  { name: "docs", kind: "dir", link: false, size: 0, modified: 1760000000, hidden: false },
  { name: ".git", kind: "dir", link: false, size: 0, modified: 1760000000, hidden: true },
  { name: "mockup.png", kind: "file", link: false, size: 412000, modified: 1760000000, hidden: false },
  { name: "notes.md", kind: "file", link: false, size: 1536, modified: 1760000000, hidden: false },
  { name: "latest", kind: "dir", link: true, size: 0, modified: 1760000000, hidden: false },
];
const listing = (path, parent = "/home") => ({ path, parent, entries: ENTRIES, truncated: false });

const TALOS = {
  id: "talos", name: "Talos", kind: "ssh", host: "h", port: 22, user: "u", auth: "key", key_path: "k", jump: null,
  startup: "", reconnect_attempts: 5, reconnect_delay_secs: 3, shell: "", has_secret: false,
};

const VIEW = {
  workspaces: [{ name: "main", active: true, panes: 2 }],
  tabs: [{ title: "files", active: true, focus: 1, panes: 2 }],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
  pane_names: [],
  windows: 1,
};

/** Open the page with the blocks `blocks` and the answers `answers` (command name to a function source or value). */
async function open(page, url, blocks, data = {}) {
  await page.addInitScript(
    ([b, view, entries, talos, extra]) => {
      Object.assign(window, extra);
      window.__BLOCKS = b;
      window.__MOCK__.on("file_open", () => { if (window.__OPEN instanceof Object && window.__OPEN.__throw) throw window.__OPEN.__throw; return structuredClone(window.__OPEN ?? null); });
      window.__MOCK__.on("file_save", () => { if (window.__SAVE_ERR) throw window.__SAVE_ERR; return 1760000099; });
      window.__MOCK__.on("file_grant", () => structuredClone(window.__GRANT ?? null));
      window.__MOCK__.setView(view);
      window.__MOCK__.on("connections_list", () => [talos]);
      window.__MOCK__.on("pane_blocks", () => structuredClone(window.__BLOCKS));
      window.__MOCK__.on("files_list", ({ path }) => ({ path, parent: path === "/" ? null : "/home", entries, truncated: false }));
    },
    [blocks, VIEW, ENTRIES, TALOS, data],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
}

const FILES = { pane: 1, kind: "files", source: "local", path: "/home/holz", source_label: "This computer" };
const PREVIEW = (path, source = "local") => ({ pane: 2, kind: "preview", source, path, source_label: "This computer" });

async function actionsCase(page, url) {
  await open(page, url, []);
  const titles = await page.evaluate(() => window.__registry.listActions().map((a) => a.title));
  assert.ok(titles.includes("Files: This computer"));
  assert.ok(titles.includes("Files: Talos (sftp)"), "each SSH connection can be browsed");
  await page.evaluate(() => window.__MOCK__.on("block_open", () => window.__MOCK__.view()));
  await page.evaluate(() => window.__registry.runAction("files.open"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 1);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("block_open")[0]), { kind: "files", source: "local", path: "", placement: "tab" });
  await page.evaluate(() => window.__registry.runAction("files.talos"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 2);
  assert.equal(await page.evaluate(() => window.__MOCK__.callsOf("block_open")[1].source), "talos");
}

async function browserCase(page, url) {
  await open(page, url, [FILES]);
  const block = page.locator('.block[data-pane="1"]');
  await block.waitFor();
  // The header is titled by what the pane shows.
  assert.equal(await page.locator('.panehead[data-pane="1"] .ph-title').textContent(), "files");
  assert.equal(await page.locator('.panehead[data-pane="1"] .ph-ctx').textContent(), "This computer · /home/holz");
  // Folders first as the core sorted them, hidden entries off, a link says so, sizes in words.
  await page.waitForSelector(".bk-row");
  const rows = await block.locator(".bk-row").evaluateAll((rs) => rs.map((r) => [r.querySelector(".bk-icon").textContent, r.querySelector(".bk-name").textContent, r.querySelector(".bk-size").textContent]));
  assert.deepEqual(rows, [["▸", "docs", ""], ["▣", "mockup.png", "402 KB"], ["▤", "notes.md", "1.5 KB"], ["▸", "latest →", ""]]);
  assert.equal(await block.locator(".bk-toggle").textContent(), "hidden: off");
  await block.locator(".bk-toggle").click();
  assert.equal(await block.locator(".bk-row").count(), 5, "hidden entries are shown on request");
  assert.equal(await block.locator(".bk-toggle").getAttribute("aria-pressed"), "true");
  await block.locator(".bk-toggle").click();

  // The keyboard drives the list: the highlight has a bar and a wash, `aria-selected` says it.
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  const selected = () => block.locator('.bk-row[aria-selected="true"] .bk-name').textContent();
  assert.equal(await selected(), "docs");
  await page.keyboard.press("ArrowDown");
  assert.equal(await selected(), "mockup.png");
  await page.keyboard.press("End");
  assert.equal(await selected(), "latest →");
  await page.keyboard.press("Home");
  assert.equal(await selected(), "docs");
  // None of those keys went to the terminal behind the list.
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("key"))).length, 0, "the list took the keys");

  // Enter on a folder goes into it; the core is told where the pane is now; Backspace goes up.
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("files_list").some((c) => c.path === "/home/holz/docs"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_update").some((c) => c.path === "/home/holz/docs"));
  assert.ok((await page.evaluate(() => window.__MOCK__.callsOf("block_update"))).every((c) => c.pane === 1), "the update names the pane");
  await page.keyboard.press("Backspace");
  await page.waitForFunction(() => window.__MOCK__.callsOf("files_list").filter((c) => c.path === "/home").length >= 1);

  // Enter on a file opens a preview beside the browser.
  await page.evaluate(() => window.__MOCK__.on("block_open", () => window.__MOCK__.view()));
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 1);
  const asked = await page.evaluate(() => window.__MOCK__.callsOf("block_open")[0]);
  assert.equal(asked.kind, "preview");
  assert.equal(asked.placement, "right");
  assert.match(asked.path, /mockup\.png$/);

  // A key with a modifier is not the list's: it reaches the shortcuts.
  const before = (await page.evaluate(() => window.__MOCK__.callsOf("files_list"))).length;
  await page.keyboard.press("Meta+ArrowDown");
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("files_list"))).length, before);
}

/** Typing marks the pane unsaved, the save chord writes with the time it was read, a change on disk is said once. */
async function editorCase(page, url, opened) {
  const file = opened({ name: "main.rs", size: 3, language: "rust", text: "abc", modified: 1760000000 });
  await open(page, url, [PREVIEW("/home/holz/main.rs")], { __OPEN: file });
  await page.waitForSelector(".bk-editor .cm-content");
  const status = page.locator('.block[data-pane="2"] .bk-status');
  // Focus goes to the editor only when the person puts it there.
  assert.equal(await page.locator(".bk-editor").evaluate((n) => document.activeElement === n), false);
  await page.locator(".cm-content").click();
  await page.keyboard.press("End");
  await page.keyboard.type("def");
  assert.deepEqual(await page.locator(".cm-line").evaluateAll((n) => n.map((l) => l.textContent)), ["abcdef"], "keys reach the editor, not the terminal");
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("key"))).length, 0);
  assert.match(await status.textContent(), /^● unsaved/);
  assert.equal(await status.getAttribute("data-dirty"), "true");

  // Closing a pane with changes asks and says so.
  await page.locator('.panehead[data-pane="2"] .ph-close').click();
  assert.match(await page.locator("[role=alertdialog]").first().innerText(), /not saved/);
  await page.keyboard.press("Escape");
  await page.locator("[role=alertdialog]").waitFor({ state: "detached" });
  await page.locator(".cm-content").click();
  await page.keyboard.press("End");

  await page.keyboard.press("Control+s");
  await page.waitForFunction(() => window.__MOCK__.callsOf("file_save").length === 1);
  const sent = await page.evaluate(() => window.__MOCK__.callsOf("file_save")[0]);
  assert.deepEqual(sent, { source: "local", path: "/home/holz/main.rs", text: "abcdef", expected_modified: 1760000000 });
  await page.waitForFunction(() => document.querySelector('.block[data-pane="2"] .bk-status').dataset.dirty === "false");
  assert.match(await status.textContent(), /^saved/);

  // The next save carries the time the last one returned.
  await page.keyboard.type("!");
  await page.keyboard.press("Control+s");
  await page.waitForFunction(() => window.__MOCK__.callsOf("file_save").length === 2);
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("file_save")[1])).expected_modified, 1760000099);

  // A change on disk is shown; the save after it overwrites on purpose.
  await page.evaluate(() => { window.__SAVE_ERR = "the file changed on disk since it was opened; save again to overwrite it"; });
  await page.keyboard.type("?");
  await page.keyboard.press("Control+s");
  await page.waitForFunction(() => document.querySelector('.block[data-pane="2"] .bk-status').dataset.failed === "true");
  assert.match(await status.textContent(), /changed on disk/);
  await page.evaluate(() => { window.__SAVE_ERR = null; });
  await page.keyboard.press("Control+s");
  await page.waitForFunction(() => window.__MOCK__.callsOf("file_save").length === 4);
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("file_save")[3])).expected_modified, null);

  // Escape gives the keyboard back to the terminal.
  await page.keyboard.press("Escape");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");
}

async function previewCase(page, url) {
  const opened = (over) => ({ name: "x", size: 10, content: "text", mime: "text/plain", language: null, html: null, text: "", ...over });
  const GRANT = { url: "holzfile://localhost/0123456789abcdef0123456789abcdef", token: "0123456789abcdef0123456789abcdef", mime: "image/png", size: 5 };

  // Source text has line numbers and a status line with what it is.
  await open(page, url, [PREVIEW("/home/holz/notes.md")], { __OPEN: opened({ name: "main.rs", size: 22, language: "rust", text: "fn main() {\n    run();\n}" }) });
  await page.waitForSelector(".bk-editor .cm-content");
  assert.deepEqual(await page.locator(".cm-line").evaluateAll((n) => n.map((l) => l.textContent)), ["fn main() {", "    run();", "}"]);
  assert.equal(await page.locator(".cm-lineNumbers .cm-gutterElement").count() >= 3, true, "line numbers");
  assert.match(await page.locator('.block[data-pane="2"] .bk-status').textContent(), /text\/plain · 22 B · rust/);
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-title').textContent(), "notes.md");
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-ctx').textContent(), "This computer · preview");

  await editorCase(page, url, opened);

  // Markdown is shown as a document (through the allow-list), not as text.
  await open(page, url, [PREVIEW("/home/holz/notes.md")], { __OPEN: opened({ name: "notes.md", content: "markdown", mime: "text/markdown", html: "<h1>Phase 2</h1><p>Key <strong>encoder</strong></p>", text: "# Phase 2" }) });
  await page.waitForSelector(".md h1");
  assert.equal(await page.locator(".md h1").textContent(), "Phase 2");
  assert.equal(await page.locator(".md strong").textContent(), "encoder");
  assert.equal(await page.locator(".md").evaluate((n) => getComputedStyle(n).fontFamily.includes("Manrope")), true, "documents are set in Manrope");

  // `e` turns the page into its source in the editor; Escape without changes goes back to the page.
  await page.evaluate(() => {
    window.__MOCK__.setView({ ...window.__MOCK__.view(), focus: 2 });
    window.__MOCK__.emit("layout", window.__MOCK__.view());
  });
  await page.waitForFunction(() => document.querySelector('.block[data-pane="2"]')?.classList.contains("focused"));
  await page.keyboard.press("e");
  await page.waitForSelector(".bk-editor .cm-line");
  assert.deepEqual(await page.locator(".cm-line").evaluateAll((n) => n.map((l) => l.textContent)), ["# Phase 2"]);
  await page.locator(".cm-content").click();
  await page.keyboard.press("Escape");
  await page.waitForSelector(".md h1");

  // An image comes from the granted URL, is revoked with the pane, and the file is never read into the page.
  await open(page, url, [PREVIEW("/home/holz/a.png")], { __OPEN: opened({ name: "a.png", content: "image", mime: "image/png", size: 5, text: null }), __GRANT: GRANT });
  const img = page.locator(".bk-image");
  await img.waitFor();
  assert.equal(await img.getAttribute("src"), GRANT.url);
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("file_grant"))).length, 1);
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("file_read"))).length, 0, "no command reads a file into the page");

  // The pane goes away: the grant ends.
  await page.evaluate(() => {
    window.__BLOCKS = [];
    window.__MOCK__.setView({ ...window.__MOCK__.view(), panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]], dividers: [], focus: 1 });
  });
  await page.evaluate(() => window.__MOCK__.emit("layout", window.__MOCK__.view()));
  await page.waitForFunction(() => window.__MOCK__.callsOf("file_revoke").length === 1);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("file_revoke")[0]), { token: GRANT.token });

  // A kind that has no preview says so, with what it is.
  await open(page, url, [PREVIEW("/home/holz/a.bin")], { __OPEN: opened({ name: "a.bin", content: "binary", mime: "application/octet-stream", size: 2048, text: null }) });
  await page.waitForSelector(".bk-message");
  assert.match(await page.locator(".bk-message").textContent(), /No preview for this kind of file yet \(application\/octet-stream, 2 KB\)/);

  // A failure is shown in words.
  await open(page, url, [PREVIEW("/home/holz/a.txt")], { __OPEN: { __throw: "sftp: the request timed out" } });
  await page.waitForFunction(() => document.querySelector(".bk-status")?.textContent.includes("timed out"));
  assert.equal(await page.locator(".bk-status").getAttribute("data-failed"), "true");
}

async function safeHtmlCase(page, url) {
  await open(page, url, []);
  const result = await page.evaluate(async () => {
    const { appendSafe, safeHref } = await import(new URL("safehtml.js", location.href).href);
    const host = document.createElement("div");
    document.body.append(host);
    const evil = [
      "<h1>Title</h1>",
      "<script>window.__pwned = 1</script>",
      "<p onclick=\"window.__pwned = 2\">click <b>bold</b> <strong>strong</strong></p>",
      "<img src=x onerror=\"window.__pwned = 3\">",
      "<a href=\"javascript:window.__pwned=4\">js</a> <a href=\"https://example.org/x\">ok</a> <a href=\"data:text/html,hi\">data</a>",
      "<iframe src=\"https://evil\"></iframe><style>body{display:none}</style>",
      "<div style=\"position:fixed\" class=\"x\">styled</div>",
      "<code class=\"language-rust evil\">a</code><code class=\"language-rust\">b</code>",
      "<input type=\"text\" value=\"x\"><input type=\"checkbox\" checked>",
      "<svg><script>window.__pwned = 5</script></svg><math><mi>x</mi></math>",
    ].join("");
    const n = appendSafe(evil, host);
    const links = [...host.querySelectorAll("a")].map((a) => [a.textContent, a.getAttribute("href"), a.getAttribute("rel")]);
    return {
      n,
      pwned: window.__pwned ?? null,
      html: host.innerHTML,
      tags: [...new Set([...host.querySelectorAll("*")].map((e) => e.localName))].sort(),
      links,
      styled: host.querySelector("div")?.getAttribute("style"),
      codes: [...host.querySelectorAll("code")].map((c) => c.getAttribute("class")),
      inputs: [...host.querySelectorAll("input")].map((i) => [i.getAttribute("type"), i.hasAttribute("checked"), i.hasAttribute("disabled"), i.getAttribute("value")]),
      hrefs: ["https://a.org", "http://a.org", "mailto:a@b.c", "javascript:alert(1)", "data:text/html,x", " JaVaScRiPt:x", "//evil.org", "/relative", "ftp://x"].map(safeHref),
    };
  });
  assert.equal(result.pwned, null, "nothing in the document ran");
  assert.deepEqual(result.tags, ["a", "b", "code", "div", "h1", "input", "p", "strong"].filter((t) => t !== "b"), `only allowed elements are left: ${result.tags}`);
  assert.ok(!result.html.includes("<script") && !result.html.includes("onclick") && !result.html.includes("onerror") && !result.html.includes("<iframe") && !result.html.includes("<img") && !result.html.includes("<svg") && !result.html.includes("style="), result.html);
  assert.deepEqual(result.links, [["js", null, null], ["ok", "https://example.org/x", "noopener noreferrer"], ["data", null, null]], "only http, https and mailto links keep their address");
  assert.equal(result.styled, null);
  assert.deepEqual(result.codes, [null, "language-rust"], "a class survives only as a language name");
  assert.deepEqual(result.inputs, [["checkbox", false, true, null], ["checkbox", true, true, null]], "an input is only a disabled checkbox");
  assert.deepEqual(result.hrefs, [true, true, true, false, false, false, false, false, false]);
  assert.ok(result.html.includes("bold"), "the text of an unknown element stays");
}

// A real two page PDF (hand made, 853 bytes) through the real PDF.js: read in ranges through `file_range`, drawn to a
// canvas, paged with the buttons and the keys, and the grant ends with the pane.
const TINY_PDF = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUiA1IDAgUl0gL0NvdW50IDIgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCA2MTIgNzkyXSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNyAwIFIgPj4gPj4gPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCA0NSA+PgpzdHJlYW0KQlQgL0YxIDI0IFRmIDcyIDcwMCBUZCAoSGVsbG8gaG9seldhdmUpIFRqIEVUCmVuZHN0cmVhbQplbmRvYmoKNSAwIG9iago8PCAvVHlwZSAvUGFnZSAvUGFyZW50IDIgMCBSIC9NZWRpYUJveCBbMCAwIDYxMiA3OTJdIC9Db250ZW50cyA2IDAgUiAvUmVzb3VyY2VzIDw8IC9Gb250IDw8IC9GMSA3IDAgUiA+PiA+PiA+PgplbmRvYmoKNiAwIG9iago8PCAvTGVuZ3RoIDQyID4+CnN0cmVhbQpCVCAvRjEgMjQgVGYgNzIgNzAwIFRkIChTZWNvbmQgcGFnZSkgVGogRVQKZW5kc3RyZWFtCmVuZG9iago3IDAgb2JqCjw8IC9UeXBlIC9Gb250IC9TdWJ0eXBlIC9UeXBlMSAvQmFzZUZvbnQgL0hlbHZldGljYSA+PgplbmRvYmoKeHJlZgowIDgKMDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTggMDAwMDAgbiAKMDAwMDAwMDEyMSAwMDAwMCBuIAowMDAwMDAwMjQ3IDAwMDAwIG4gCjAwMDAwMDAzNDIgMDAwMDAgbiAKMDAwMDAwMDQ2OCAwMDAwMCBuIAowMDAwMDAwNTYwIDAwMDAwIG4gCnRyYWlsZXIKPDwgL1NpemUgOCAvUm9vdCAxIDAgUiA+PgpzdGFydHhyZWYKNjMwCiUlRU9GCg==";

async function pdfCase(page, url) {
  await page.addInitScript((b64) => {
    const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
    window.__MOCK__.on("file_range", ({ offset, len }) => bytes.slice(offset, offset + len));
  }, TINY_PDF);
  const GRANT = { url: "holzfile://localhost/fedcba9876543210fedcba9876543210", token: "fedcba9876543210fedcba9876543210", mime: "application/pdf", size: 853 };
  await open(page, url, [PREVIEW("/home/holz/tiny.pdf")], {
    __OPEN: { name: "tiny.pdf", size: 853, content: "pdf", mime: "application/pdf", language: null, html: null, text: null },
    __GRANT: GRANT,
  });
  await page.waitForSelector(".pdf-canvas");
  await page.waitForFunction(() => document.querySelector(".pdf-count")?.textContent === "1 / 2");
  // The page was drawn: the canvas has dark ink on white, which a blank canvas does not.
  const ink = () => page.locator(".pdf-canvas").evaluate((c) => {
    const data = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
    let dark = 0;
    for (let i = 0; i < data.length; i += 4) if (data[i] < 100 && data[i + 3] > 0) dark++;
    return dark;
  });
  await page.waitForFunction(() => {
    const c = document.querySelector(".pdf-canvas");
    if (!c || c.width === 0) return false;
    const data = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
    let dark = 0;
    for (let i = 0; i < data.length; i += 4) if (data[i] < 100 && data[i + 3] > 0) dark++;
    return dark > 50;
  });
  assert.ok((await ink()) > 50, "the page has text on it");
  assert.match(await page.locator('.block[data-pane="2"] .bk-status').textContent(), /application\/pdf · 853 B · 1 \/ 2/);
  assert.ok((await page.evaluate(() => window.__MOCK__.callsOf("file_range"))).length >= 1, "the document was read through the core in ranges");

  // Next page by the button and by the key; the first page cannot go back.
  assert.equal(await page.locator(".pdf-bar button", { hasText: "Prev" }).isDisabled(), true);
  await page.locator(".pdf-bar button", { hasText: "Next" }).click();
  await page.waitForFunction(() => document.querySelector(".pdf-count")?.textContent === "2 / 2");
  assert.equal(await page.locator(".pdf-bar button", { hasText: "Next" }).isDisabled(), true);
  // The keys go to the viewer while its pane has the focus.
  await page.evaluate(() => {
    window.__MOCK__.setView({ ...window.__MOCK__.view(), focus: 2 });
    window.__MOCK__.emit("layout", window.__MOCK__.view());
  });
  await page.waitForFunction(() => document.querySelector('.block[data-pane="2"]')?.classList.contains("focused"));
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  await page.keyboard.press("PageUp");
  await page.waitForFunction(() => document.querySelector(".pdf-count")?.textContent === "1 / 2");
  // Zoom changes the drawn width; fit goes back to the width of the pane.
  const width = () => page.locator(".pdf-canvas").evaluate((c) => c.getBoundingClientRect().width);
  const fitted = await width();
  await page.keyboard.press("+");
  await page.waitForFunction((w) => document.querySelector(".pdf-canvas").getBoundingClientRect().width > w, fitted);
  await page.keyboard.press("0");
  await page.waitForFunction((w) => Math.abs(document.querySelector(".pdf-canvas").getBoundingClientRect().width - w) < 2, fitted);
  assert.match(await page.locator(".pdf-zoom").textContent(), /\d+ %/);

  // The pane goes away: the grant ends.
  await page.evaluate(() => {
    window.__BLOCKS = [];
    window.__MOCK__.setView({ ...window.__MOCK__.view(), panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]], dividers: [], focus: 1 });
  });
  await page.evaluate(() => window.__MOCK__.emit("layout", window.__MOCK__.view()));
  await page.waitForFunction(() => window.__MOCK__.callsOf("file_revoke").length === 1);
}
