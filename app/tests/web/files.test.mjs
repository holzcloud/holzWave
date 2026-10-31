// Files in a real browser against the real server: a file dropped on the pane arrives in the person's upload folder with the
// bytes it had and its path is typed on the command line; a file over the configured limit is refused with the page's sentence
// and nothing arrives; odd names are stored under cleaned names inside the folder; a granted file downloads with the same
// bytes and the headers of the file route, and the link works only for the person who holds the grant.
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { runTest, signIn, oracle, poll, check, equal } from "./lib.mjs";

const LIMIT = 72 * 1024;
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");

// Fixed pseudo-random bytes: the same on every run (a counter hashed in blocks).
function fixedBytes(length, seed) {
  const out = Buffer.alloc(length);
  for (let block = 0; block * 32 < length; block++) createHash("sha256").update(`${seed}:${block}`).digest().copy(out, block * 32, 0, Math.min(32, length - block * 32));
  return out;
}

// A 1x1 PNG, so that the server grants it as an image.
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==", "base64");

await runTest("files", { upload_max_bytes: LIMIT }, async ({ harness, open }) => {
  const owner = await harness.account({ role: "admin", name: "pf-owner" });
  const other = await harness.account({ role: "member", name: "pf-other" });

  const ownerContext = await open();
  const page = await ownerContext.context.newPage();
  await signIn(page, harness, owner);
  const socket = await oracle(page);
  const layout = await poll(async () => {
    const view = await socket.layout();
    return view.panes?.length > 0 ? view : undefined;
  }, "the application shows a pane", 20_000);
  const pane = layout.focus;

  const lines = (text) => text.split("\n").map((l) => l.trimEnd());
  const joined = async () => lines(await socket.screen(pane)).join("");
  const typeLine = async (text, { enter = true } = {}) => {
    await page.locator("canvas#term").click();
    await page.keyboard.type(text);
    if (enter) await page.keyboard.press("Enter");
  };

  /** Drop a file on the pane's canvas the way a browser does: the three events, the last with the data. */
  const drop = (name, bytes) =>
    page.evaluate(
      ({ name, data }) => {
        const bin = atob(data);
        const array = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) array[i] = bin.charCodeAt(i);
        const transfer = new DataTransfer();
        transfer.items.add(new File([array], name, { type: "application/x-something-else" }));
        const canvas = document.querySelector("canvas#term");
        const box = canvas.getBoundingClientRect();
        const at = { clientX: box.left + box.width / 2, clientY: box.top + box.height / 2, bubbles: true, cancelable: true, dataTransfer: transfer };
        for (const type of ["dragenter", "dragover", "drop"]) canvas.dispatchEvent(new DragEvent(type, at));
      },
      { name, data: bytes.toString("base64") },
    );

  /** The path the page typed after `marker` on the command line: the text between its single quotes. */
  const typedPath = (screenJoined, marker) => {
    const from = screenJoined.lastIndexOf(marker);
    if (from < 0) return null;
    const match = /'([^']*)'/.exec(screenJoined.slice(from + marker.length));
    return match ? match[1] : null;
  };

  await typeLine('hw_sum() { sha256sum "$@" 2>/dev/null || shasum -a 256 "$@"; }');

  // ---- a file of 64 KiB arrives with the bytes it had ----------------------------------------------------------------------------
  const body = fixedBytes(64 * 1024, "drop");
  await typeLine("hw_sum #ONE ", { enter: false });
  await drop("random64k.bin", body);
  const firstPath = await poll(async () => typedPath(await joined(), "#ONE "), "the path of the dropped file is typed on the command line", 20_000);
  check(firstPath.startsWith("/") && firstPath.endsWith("random64k.bin"), "the typed text is a server path ending in the file name");
  const folder = firstPath.slice(0, firstPath.lastIndexOf("/"));
  check(/\/uploads$/.test(folder), `the file is in an uploads folder (${folder.split("/").slice(-1)[0]})`);
  // The command line holds a marker and the typed path; it is cleared and the command is typed again with the path as its argument.
  await page.keyboard.press("Control+U");
  await typeLine(`hw_sum '${firstPath}'`);
  const digest = await poll(async () => {
    const hit = lines(await socket.screen(pane)).map((l) => /^([0-9a-f]{64})\b/.exec(l)).find(Boolean);
    return hit ? hit[1] : undefined;
  }, "the hash of the stored file is on the screen", 15_000);
  equal(digest, sha(body), "the stored file has the hash of the dropped bytes");

  // ---- a file over the limit -----------------------------------------------------------------------------------------------------------
  const over = fixedBytes(LIMIT + 4 * 1024, "over");
  await drop("big.bin", over);
  const refused = page.getByText("Not uploaded: too large.", { exact: true });
  await refused.waitFor({ timeout: 20_000 });
  await typeLine(`ls -1 '${folder}'`);
  const listing = await poll(async () => {
    const seen = lines(await socket.screen(pane));
    const at = seen.lastIndexOf(`ls -1 '${folder}'`);
    // the command may wrap: look for the file's name below the last echo of the command
    return seen.slice(at + 1).includes("random64k.bin") ? seen.slice(at + 1) : undefined;
  }, "the folder is listed", 15_000);
  check(!listing.includes("big.bin"), "the over-limit file did not arrive");
  const limitSentence = (await refused.textContent()).trim();

  // ---- odd names -----------------------------------------------------------------------------------------------------------------------------
  const api = (method, path, bytes) =>
    page.evaluate(
      async ({ method, path, data }) => {
        const { http } = await import(new URL("bridge.js", location.href).href);
        const bin = atob(data);
        const array = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) array[i] = bin.charCodeAt(i);
        try {
          return { ok: await http(method, path, new Blob([array])) };
        } catch (e) {
          return { err: `${e.status ?? ""} ${e.message}` };
        }
      },
      { method, path, data: bytes.toString("base64") },
    );
  const escaped = await api("PUT", `/api/upload?name=${encodeURIComponent("../../escape.txt")}`, Buffer.from("not outside"));
  let escapedName;
  if (escaped.ok) {
    const path = escaped.ok.path;
    check(path.startsWith(`${folder}/`) && !path.slice(folder.length + 1).includes("/") && !path.includes(".."), `a name with ../ is stored inside the folder (${path.slice(folder.length)})`);
    escapedName = path.slice(folder.length + 1);
  } else {
    escapedName = `refused (${escaped.err})`;
  }
  const controls = await api("PUT", `/api/upload?name=${encodeURIComponent("we\u0001ird\u0007\u001b[31m.txt")}`, Buffer.from("odd"));
  let controlName;
  if (controls.ok) {
    const name = controls.ok.path.slice(controls.ok.path.lastIndexOf("/") + 1);
    check(!/[\u0000-\u001f\u007f]/.test(name), "a name with control characters is stored without them");
    check(controls.ok.path.startsWith(`${folder}/`), "a name with control characters is stored inside the folder");
    controlName = JSON.stringify(name);
  } else {
    controlName = `refused (${controls.err})`;
  }

  // ---- download ----------------------------------------------------------------------------------------------------------------------------------
  const picture = await api("PUT", `/api/upload?name=${encodeURIComponent("pic.png")}`, PNG);
  check(picture.ok !== undefined, `the picture was uploaded (${picture.err ?? ""})`);
  const picturePath = picture.ok.path;
  const waiting = page.waitForEvent("download", { timeout: 20_000 });
  await page.evaluate(async (path) => {
    const { downloadFile } = await import(new URL("webfiles.js", location.href).href);
    await downloadFile({ source: "local", path });
  }, picturePath);
  const download = await waiting;
  const saved = readFileSync(await download.path());
  equal(sha(saved), sha(PNG), "the downloaded file has the hash of the uploaded bytes");
  equal(download.suggestedFilename(), "pic.png", "the suggested file name");
  const link = new URL(download.url()).pathname;
  check(/^\/files\/[0-9a-f]{32}$/.test(link), "the grant is 32 lowercase hexadecimal characters");
  check(!page.url().includes("/files/"), "the link is not in the address");
  check(!(await page.title()).includes("files"), "the link is not in the title");
  check(!(await page.locator("#status").innerText()).includes(link.slice(-32)), "the link is not in the status line");

  const own = await ownerContext.context.request.get(`${harness.origin}${link}`);
  equal(own.status(), 200, "the owner reads the granted file");
  equal(sha(await own.body()), sha(PNG), "the bytes the owner reads");
  const headers = own.headers();
  equal(headers["content-security-policy"], "default-src 'none'; sandbox", "Content-Security-Policy of the file route");
  equal(headers["cross-origin-resource-policy"], "same-site", "Cross-Origin-Resource-Policy of the file route");
  equal(headers["x-content-type-options"], "nosniff", "X-Content-Type-Options of the file route");
  equal(headers["cache-control"], "no-store", "Cache-Control of the file route");

  const stranger = await open();
  const nobody = await stranger.context.request.get(`${harness.origin}${link}`);
  equal(nobody.status(), 401, "the link without a session");
  const otherContext = await open();
  const otherPage = await otherContext.context.newPage();
  await signIn(otherPage, harness, other);
  const theirs = await otherContext.context.request.get(`${harness.origin}${link}`);
  const unknown = await otherContext.context.request.get(`${harness.origin}/files/${randomBytes(16).toString("hex")}`);
  equal(theirs.status(), 404, "the link with another person's session");
  equal(unknown.status(), 404, "an unknown token");
  equal(await theirs.text(), await unknown.text(), "another person's link answers like an unknown one");

  // A path outside the home cannot be granted.
  const outside = await socket.call("file_grant", { source: "local", path: "/etc/hosts" });
  check(outside.err !== undefined, "a path outside the home is not granted");

  ownerContext.expectClean("the owner's context");
  stranger.expectClean("the context with no session");
  otherContext.expectClean("the other person's context");
  await socket.close();
  console.log(
    `files: dropped 64 KiB: hashes equal, path typed in the uploads folder; over the limit (${LIMIT / 1024} KiB set, ${(over.length / 1024)} KiB dropped): "${limitSentence}", the folder listing did not have it; ` +
      `name with ../ -> ${escapedName}; name with control characters -> ${controlName}; download: same hash, name pic.png, grant of 32 lowercase hexadecimal characters, link not in address, title or status line, ` +
      `headers CSP/CORP/nosniff/no-store as listed; no cookie: ${nobody.status()}; other person: ${theirs.status()} (an unknown token: ${unknown.status()}, same body); /etc/hosts grant refused`,
  );
});
