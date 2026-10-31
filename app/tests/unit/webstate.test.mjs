// The pure rules of the browser build's page code (webstate.ts) and the web platform of chords.ts. The OS setting of
// chords.ts is module state and node runs every test file in a process of its own, so each test sets the OS it needs.
import test from "node:test";
import assert from "node:assert/strict";
import { actionIdFor, chordFor, setBindingTable, setOs } from "../../dist/chords.js";
import {
  CLIPBOARD_REFUSED, DESKTOP_ONLY_ACTIONS, FULLSCREEN_NOTICE, ICON_ATTENTION, ICON_PLAIN, LOCK_KEYS, NOT_AVAILABLE, Notices, canLockKeys,
  NOTIFY_FLAG_KEY, NOTIFY_GAP_MS, NOTIFY_GROUP, NOTIFY_TEXT, NotifyGate, commandKind, noticeIcon, noticeTitle, permissionState,
  DROP_LABEL, FOLDERS_SKIPPED, MAX_DROP_FILES, batchText, ONLY_SIXTEEN, hasNewline, notUploaded, readUploaded, refusalText, shellQuote, takeFiles,
  uploadFolder, uploadUrl, uploadedText, uploadingText, DOWNLOAD_ONLY_SENTENCE, GrantList, MAX_GRANTS, baseName, readGrant,
} from "../../dist/webstate.js";
import { findTargets, isLinkModifier, linkAction } from "../../dist/links.js";
import { isAvailable, listActions, listAvailableActions, registerAction, runAction, setAvailability } from "../../dist/actions.js";
import { Attention } from "../../dist/attentionstate.js";
import { readdirSync, readFileSync } from "node:fs";

const CLIPBOARD = [
  ["Ctrl+Shift+C", "edit.copy"],
  ["Ctrl+Insert", "edit.copy"],
  ["Ctrl+Shift+V", "edit.paste"],
  ["Shift+Insert", "edit.paste"],
];

test("on the web the fallback holds the fixed clipboard chords of Windows and Linux", () => {
  setBindingTable([]);
  setOs("web");
  for (const [chord, id] of CLIPBOARD) assert.equal(actionIdFor(chord), id, chord);
  assert.equal(chordFor("edit.copy"), "Ctrl+Shift+C");
  assert.equal(chordFor("edit.paste"), "Ctrl+Shift+V");
});

test("on the web Ctrl+V, plain Ctrl+C and the Cmd chords stay with the browser or the program", () => {
  setBindingTable([]);
  setOs("web");
  assert.equal(actionIdFor("Ctrl+V"), undefined, "Ctrl+V reaches the program as a key");
  assert.equal(actionIdFor("Ctrl+C"), undefined, "plain Ctrl+C is never a copy");
  assert.equal(actionIdFor("Cmd+C"), undefined);
  assert.equal(actionIdFor("Cmd+V"), undefined);
  assert.equal(actionIdFor("Cmd+W"), undefined, "Cmd+W is not a web chord");
  assert.equal(actionIdFor("Cmd+D"), undefined);
});

test("the web is its own platform: it is not the macOS table and no Win-key chord stands in", () => {
  setBindingTable([]);
  setOs("web");
  assert.equal(chordFor("pane.split_right"), undefined);
  assert.equal(chordFor("pane.close"), undefined);
});

test("with the core's table set the fixed clipboard chords of the web stay fixed", () => {
  setOs("web");
  setBindingTable([
    { action: "pane.split_right", chord: "Ctrl+Shift+D" },
    { action: "pane.close", chord: "Ctrl+Shift+W" },
  ]);
  assert.equal(actionIdFor("Ctrl+Shift+D"), "pane.split_right", "a row of the core's web table runs its action");
  for (const [chord, id] of CLIPBOARD) assert.equal(actionIdFor(chord), id, chord);
  assert.equal(actionIdFor("Ctrl+V"), undefined);
});

test("the refused-read sentence is the one the design gives", () => {
  assert.equal(CLIPBOARD_REFUSED, "The browser did not allow reading the clipboard.");
});

test("the keys locked in full screen are the six a browser keeps and Escape, in this order", () => {
  assert.deepEqual([...LOCK_KEYS], ["KeyW", "KeyT", "KeyN", "Tab", "PageUp", "PageDown", "Escape"]);
  assert.equal(new Set(LOCK_KEYS).size, 7, "no code twice");
});

test("canLockKeys is true only for a navigator whose keyboard has a lock function", () => {
  assert.equal(canLockKeys({ keyboard: { lock() {}, unlock() {} } }), true);
  assert.equal(canLockKeys({ keyboard: { lock: async () => {} } }), true);
  assert.equal(canLockKeys({}), false, "no keyboard object");
  assert.equal(canLockKeys({ keyboard: {} }), false, "a keyboard without lock");
  assert.equal(canLockKeys({ keyboard: { lock: "yes" } }), false, "a lock that is not a function");
  assert.equal(canLockKeys({ keyboard: { lock: null } }), false);
  assert.equal(canLockKeys({ keyboard: null }), false);
  assert.equal(canLockKeys(null), false);
  assert.equal(canLockKeys(undefined), false);
  assert.equal(canLockKeys("keyboard"), false);
});

test("the full-screen notice is the sentence the design gives", () => {
  assert.equal(FULLSCREEN_NOTICE, "Browser shortcuts are off in full screen. Hold Escape to leave.");
});

// ---- links ----------------------------------------------------------------------------------------------------------

test("on the web the link modifier is Ctrl or Cmd, and neither alone is no modifier", () => {
  assert.equal(isLinkModifier({ ctrlKey: true }, "web"), true, "Ctrl");
  assert.equal(isLinkModifier({ metaKey: true }, "web"), true, "Cmd");
  assert.equal(isLinkModifier({ ctrlKey: true, metaKey: true }, "web"), true, "both");
  assert.equal(isLinkModifier({}, "web"), false, "neither");
  assert.equal(isLinkModifier({ ctrlKey: false, metaKey: false, shiftKey: true }, "web"), false, "Shift is not a link modifier");
});

test("the link modifier of macOS and of the other platforms is what it was", () => {
  assert.equal(isLinkModifier({ metaKey: true }, "macos"), true);
  assert.equal(isLinkModifier({ ctrlKey: true }, "macos"), false, "Ctrl+click is the menu on macOS");
  assert.equal(isLinkModifier({ ctrlKey: true }, "linux"), true);
  assert.equal(isLinkModifier({ metaKey: true }, "linux"), false);
  assert.equal(isLinkModifier({ ctrlKey: true }, "windows"), true);
  assert.equal(isLinkModifier({}, "linux"), false);
});

test("a path is no link where a folder cannot be shown, and a URL is the same action either way", () => {
  const [url] = findTargets(Array.from("https://example.com/a?b=1"));
  const [path] = findTargets(Array.from("./src/main.rs"));
  assert.equal(url.kind, "url");
  assert.equal(path.kind, "path");
  const want = { command: "open_url", args: { url: "https://example.com/a?b=1" } };
  assert.deepEqual(linkAction(url, "/home/u", false), want);
  assert.deepEqual(linkAction(url, "/home/u"), want, "the default keeps every existing call");
  assert.deepEqual(linkAction(url, "/home/u", true), want);
  assert.equal(linkAction(path, "/home/u", false), null, "no path where nothing can be revealed");
  assert.deepEqual(linkAction(path, "/home/u"), { command: "reveal_path", args: { path: "./src/main.rs", base: "/home/u" } });
  assert.deepEqual(linkAction(path, null, true), { command: "reveal_path", args: { path: "./src/main.rs", base: null } });
});

// ---- controls the browser does not have -----------------------------------------------------------------------------

test("the answer for a control the browser does not have is the sentence the design gives", () => {
  assert.equal(NOT_AVAILABLE, "Not available in the browser.");
});

test("the two tear-off actions are the ones the browser build cannot offer", () => {
  assert.deepEqual([...DESKTOP_ONLY_ACTIONS], ["pane.move_to_new_window", "tab.move_to_new_window"]);
});

test("an action is available unless it carries a test that says no; unavailable actions stay in the registry and still run", () => {
  let ran = 0;
  registerAction({ id: "t45.plain", title: "Plain", run: () => ran++ });
  registerAction({ id: "t45.gated", title: "Gated", run: () => ran++ });
  assert.equal(setAvailability("t45.gated", () => false), true);
  assert.equal(setAvailability("t45.unknown", () => false), false, "an unknown id is not an error");
  const plain = listActions().find((a) => a.id === "t45.plain");
  const gated = listActions().find((a) => a.id === "t45.gated");
  assert.equal(isAvailable(plain), true, "no test: available, as every action was before");
  assert.equal(isAvailable(gated), false);
  assert.ok(listActions().some((a) => a.id === "t45.gated"), "the registry still holds it");
  const listed = listAvailableActions().map((a) => a.id);
  assert.ok(listed.includes("t45.plain"));
  assert.ok(!listed.includes("t45.gated"), "the palette's list leaves it out");
  assert.equal(runAction("t45.gated"), true);
  assert.equal(ran, 1, "a stored shortcut or a script still reaches it");
  setAvailability("t45.gated", () => true);
  assert.ok(listAvailableActions().some((a) => a.id === "t45.gated"), "the test is asked each time");
});

test("an availability test that throws makes the action unavailable, not the palette broken", () => {
  registerAction({ id: "t45.broken", title: "Broken", run: () => {} });
  setAvailability("t45.broken", () => {
    throw new Error("boom");
  });
  assert.equal(isAvailable(listActions().find((a) => a.id === "t45.broken")), false);
  assert.ok(Array.isArray(listAvailableActions()));
});

test("only the browser build's entry file sets an availability test", () => {
  const dist = new URL("../../dist/", import.meta.url);
  const users = readdirSync(dist)
    .filter((f) => f.endsWith(".js"))
    .filter((f) => /\bsetAvailability\(/.test(readFileSync(new URL(f, dist), "utf8")))
    .sort();
  assert.deepEqual(users, ["actions.js", "web.js"]);
});

// ---- tab marks ------------------------------------------------------------------------------------------------------

test("the notice state records a kind only while the page is hidden, once, and clear empties it", () => {
  const n = new Notices();
  assert.deepEqual(n.list(), []);
  assert.equal(n.note("bell", false), false, "a visible page records nothing");
  assert.deepEqual(n.list(), []);
  assert.equal(n.note("bell", true), true, "hidden: recorded");
  assert.equal(n.note("bell", true), false, "a repeat of a kind changes nothing");
  assert.equal(n.note("done", true), true);
  assert.deepEqual(n.list(), ["bell", "done"]);
  assert.equal(n.clear(), true);
  assert.deepEqual(n.list(), []);
  assert.equal(n.clear(), false, "clearing nothing changes nothing");
});

test("the notice list is in the fixed order of the bar whatever order the kinds came in", () => {
  const n = new Notices();
  for (const kind of ["done", "failed", "bell", "ask"]) n.note(kind, true);
  assert.deepEqual(n.list(), ["ask", "bell", "failed", "done"]);
  assert.equal(n.drop("ask"), true, "a question that was answered goes");
  assert.equal(n.drop("ask"), false);
  assert.deepEqual(n.list(), ["bell", "failed", "done"]);
});

test("a kind the bar does not know is not recorded", () => {
  const n = new Notices();
  assert.equal(n.note("sound", true), false);
  assert.equal(n.note("__proto__", true), false);
  assert.deepEqual(n.list(), []);
});

test("the title is the glyphs of the bar in the fixed order, a space, and the base title; the base alone for none", () => {
  assert.equal(noticeTitle("holzWave", []), "holzWave");
  assert.equal(noticeTitle("holzWave", ["bell"]), "\u25B2 holzWave");
  assert.equal(noticeTitle("holzWave", ["ask", "bell"]), "? \u25B2 holzWave");
  assert.equal(noticeTitle("holzWave", ["ask", "bell", "failed", "done"]), "? \u25B2 ! \u2713 holzWave");
  assert.equal(noticeTitle("holzWave", ["done", "ask"]), "? \u2713 holzWave", "the order is the bar's, not the caller's");
});

test("the icon is the attention file for a non-empty set and the plain file otherwise", () => {
  assert.equal(ICON_PLAIN, "favicon.svg");
  assert.equal(ICON_ATTENTION, "favicon-attention.svg");
  assert.equal(noticeIcon([]), ICON_PLAIN);
  assert.equal(noticeIcon(["done"]), ICON_ATTENTION);
  assert.equal(noticeIcon(["ask", "bell", "failed", "done"]), ICON_ATTENTION);
});

test("a command that ended is finished for status 0 or none and failed for any other whole number", () => {
  assert.equal(commandKind(0), "done");
  assert.equal(commandKind(null), "done");
  assert.equal(commandKind(undefined), "done");
  assert.equal(commandKind(1), "failed");
  assert.equal(commandKind(130), "failed");
  assert.equal(commandKind(-1), "done", "a negative status counts as none, as on the bar");
  assert.equal(commandKind(1.5), "done");
  assert.equal(commandKind("1"), "done");
  assert.equal(commandKind(Number.NaN), "done");
});

const SRC = new URL("../../src/", import.meta.url);
const text = (name) => readFileSync(new URL(name, SRC), "utf8");

test("the two icon files are SVG with a viewBox, and the attention file is the plain file plus one dot in the rose of the page", () => {
  const plain = text("favicon.svg");
  const attention = text("favicon-attention.svg");
  for (const [name, svg] of [["favicon.svg", plain], ["favicon-attention.svg", attention]]) {
    assert.match(svg, /^<svg\b[^>]*\sxmlns="http:\/\/www\.w3\.org\/2000\/svg"/, `${name} is an SVG document`);
    assert.match(svg, /<svg\b[^>]*\sviewBox="[^"]+"/, `${name} has a viewBox`);
    assert.ok(svg.trimEnd().endsWith("</svg>"), name);
    assert.ok(!/<script|onload=|href=/i.test(svg), `${name} carries no script and no link`);
  }
  const head = plain.trimEnd().slice(0, -"</svg>".length);
  assert.ok(attention.startsWith(head), "the attention file starts with the whole plain file");
  const extra = attention.slice(head.length).trim();
  const elements = extra.match(/<[a-zA-Z][^>]*>/g) ?? [];
  assert.equal(elements.length, 1, `one more element: ${extra}`);
  assert.match(elements[0], /^<circle\b/, "the dot is a circle");
  const fill = /\sfill="(#[0-9a-fA-F]{6})"/.exec(elements[0]);
  assert.ok(fill, "the dot has a literal fill");
  const rose = /--hw-rose:\s*(#[0-9a-fA-F]{6})\b/.exec(text("app.css"));
  assert.ok(rose, "app.css defines --hw-rose");
  assert.equal(fill[1].toLowerCase(), rose[1].toLowerCase(), "the dot is the rose of the page");
  assert.ok(!/<circle\b/.test(plain), "the plain file has no dot");
});

test("the icon link and the title marks come from the browser-only module, which web.js starts and the Settings dialog loads on demand", () => {
  const dist = new URL("../../dist/", import.meta.url);
  const files = readdirSync(dist).filter((f) => f.endsWith(".js"));
  const users = files.filter((f) => /["']\.\/webnotice\.js["']/.test(readFileSync(new URL(f, dist), "utf8"))).sort();
  assert.deepEqual(users, ["settings-ui.js", "web.js"]);
  assert.ok(/import\(["']\.\/webnotice\.js["']\)/.test(readFileSync(new URL("settings-ui.js", dist), "utf8")), "the dialog loads it with a dynamic import");
  assert.ok(!/^import .*webnotice/m.test(readFileSync(new URL("settings-ui.js", dist), "utf8")), "and never with a static one");
  for (const page of ["index.html", "login.html"]) assert.ok(!/rel="[^"]*icon/.test(readFileSync(new URL(page, dist), "utf8")), `${page} has no icon link`);
});

test("the attention store names the panes whose question waits, sorted, and tells when that changes", () => {
  const store = new Attention();
  let told = 0;
  store.subscribe(() => told++);
  assert.deepEqual(store.waiting(), []);
  let panes = [3, 1];
  const off = store.provide("t", () => panes);
  assert.deepEqual(store.waiting(), [1, 3], "sorted");
  store.changed();
  assert.equal(told, 1);
  store.changed();
  assert.equal(told, 1, "no change, no telling");
  panes = [];
  store.changed();
  assert.equal(told, 2);
  assert.deepEqual(store.waiting(), []);
  store.provide("bad", () => {
    throw new Error("boom");
  });
  assert.deepEqual(store.waiting(), [], "a provider that throws names none");
  panes = [2, 2, 5];
  store.provide("u", () => [5]);
  assert.deepEqual(store.waiting(), [2, 5], "unique across providers");
  off();
});

// ---- notifications ----------------------------------------------------------------------------------------------

test("the state of the notifications is the permission and the person's flag together", () => {
  assert.equal(permissionState("default", false), "off");
  assert.equal(permissionState("default", true), "off", "a flag without the permission is off");
  assert.equal(permissionState("granted", true), "on");
  assert.equal(permissionState("granted", false), "off", "the browser allows it but the person did not turn it on");
  assert.equal(permissionState("denied", false), "blocked by the browser");
  assert.equal(permissionState("denied", true), "blocked by the browser", "whatever the flag says");
  assert.equal(permissionState("unheard-of", true), "off");
  assert.equal(permissionState(undefined, true), "off");
});

test("a notification says the words of the mark and never anything else", () => {
  assert.deepEqual({ ...NOTIFY_TEXT }, { ask: "Waits for you", bell: "Rang the bell", failed: "Command failed", done: "Command finished" });
  assert.equal(NOTIFY_GAP_MS, 5000);
  assert.equal(NOTIFY_FLAG_KEY, "holzwave.notify");
});

test("a notification is made only while hidden and on, and at most one per kind in five seconds", () => {
  const gate = new NotifyGate();
  assert.equal(gate.next("bell", 0, false, "on"), null, "a visible page makes none");
  assert.equal(gate.next("bell", 0, true, "off"), null, "off makes none");
  assert.equal(gate.next("bell", 0, true, "blocked by the browser"), null);
  assert.equal(gate.next("sound", 0, true, "on"), null, "an unknown kind makes none");
  assert.equal(gate.next("__proto__", 0, true, "on"), null);
  assert.equal(gate.next("bell", 1000, true, "on"), "Rang the bell", "the refusals above did not use the window");
  assert.equal(gate.next("bell", 1001, true, "on"), null, "a second of the same kind within five seconds is refused");
  assert.equal(gate.next("bell", 5999, true, "on"), null, "a refusal does not move the window");
  assert.equal(gate.next("failed", 1002, true, "on"), "Command failed", "another kind is not");
  assert.equal(gate.next("done", 1003, true, "on"), "Command finished");
  assert.equal(gate.next("ask", 1004, true, "on"), "Waits for you");
  assert.equal(gate.next("bell", 6000, true, "on"), "Rang the bell", "five seconds after the last shown one it may be shown again");
  assert.equal(gate.next("bell", 6001, true, "on"), null);
  assert.equal(gate.next("bell", 100, true, "on"), "Rang the bell", "a clock that went back does not silence it for good");
});

test("the group Notifications has the sentences of the design in each of its three states", () => {
  assert.deepEqual(
    { legend: NOTIFY_GROUP.legend, name: NOTIFY_GROUP.name, meta: NOTIFY_GROUP.meta },
    { legend: "Notifications", name: "Notify me", meta: "bell and attention" },
  );
  assert.deepEqual(NOTIFY_GROUP.rows.off, {
    tone: "plain",
    advice: "Bell and attention marks also show in the tab title. Turn this on to get a browser notification as well.",
    button: "Turn on",
  });
  assert.deepEqual(NOTIFY_GROUP.rows.on, {
    tone: "ok",
    advice: "A browser notification appears when a pane out of view rings the bell or needs you. It never contains terminal text.",
    button: "Turn off",
  });
  assert.deepEqual(NOTIFY_GROUP.rows["blocked by the browser"], {
    tone: "warn",
    advice: "The browser blocks notifications for this site. Allow them in the site settings of the browser, then choose Check again.",
    button: "Check again",
  });
  assert.equal(NOTIFY_GROUP.checked, "Checked.");
});

// ---- upload by drop ---------------------------------------------------------------------------------------------------

test("a path is quoted for the shell with single quotes, and a single quote inside it is closed, escaped and opened again", () => {
  assert.equal(shellQuote("/home/p/uploads/a b.txt"), "'/home/p/uploads/a b.txt'");
  assert.equal(shellQuote("/home/p/it's.txt"), "'/home/p/it'\\''s.txt'");
  assert.equal(shellQuote("/home/p/''"), "'/home/p/'\\'''\\'''");
  assert.equal(shellQuote("/home/p/-rf"), "'/home/p/-rf'", "a leading dash in the name is inside the quotes");
  assert.equal(shellQuote(""), "''");
  assert.equal(shellQuote("$(rm -rf ~) `x` ; & | > *"), "'$(rm -rf ~) `x` ; & | > *'", "no other character is special inside single quotes");
});

test("a newline stays inside the quotes and is reported, so the paste confirmation applies", () => {
  const path = "/home/p/a\nb";
  assert.equal(shellQuote(path), "'/home/p/a\nb'");
  assert.equal(hasNewline(path), true);
  assert.equal(hasNewline("/home/p/a\rb"), true);
  assert.equal(hasNewline("/home/p/ab"), false);
});

test("the upload address carries the encoded name in the query, at most 200 bytes of it, cut on a character", () => {
  assert.equal(uploadUrl("a b.txt"), "/api/upload?name=a%20b.txt");
  assert.equal(uploadUrl("x/../&=?#"), "/api/upload?name=x%2F..%2F%26%3D%3F%23", "nothing of the name can leave the query");
  assert.equal(uploadUrl("a".repeat(200)), `/api/upload?name=${"a".repeat(200)}`);
  assert.equal(uploadUrl("a".repeat(201)), `/api/upload?name=${"a".repeat(200)}`);
  const wide = "ä".repeat(101); // two bytes each: 202 bytes
  assert.equal(uploadUrl(wide), `/api/upload?name=${encodeURIComponent("ä".repeat(100))}`, "a character is never cut in half");
  const emoji = "\u{1F600}".repeat(60); // four bytes each: 240 bytes
  assert.equal(uploadUrl(emoji), `/api/upload?name=${encodeURIComponent("\u{1F600}".repeat(50))}`);
  assert.equal(uploadUrl(""), "/api/upload?name=upload");
  assert.equal(uploadUrl("  \t "), "/api/upload?name=upload");
  assert.doesNotThrow(() => uploadUrl("lone\ud800surrogate"), "a name that is not well formed is still a name");
});

test("a refusal is told in the words of the design, from the status and the member field only", () => {
  assert.equal(refusalText(400, { code: "bad_request", field: "size" }), "too large");
  assert.equal(refusalText(413, {}), "too large");
  assert.equal(refusalText(400, { code: "bad_request", field: "name" }), "name not allowed");
  assert.equal(refusalText(429, {}), "try again");
  assert.equal(refusalText(503, { code: "busy" }), "try again");
  assert.equal(refusalText(423, { code: "locked" }), "secrets are locked");
  assert.equal(refusalText(401, {}), "sign in again");
  assert.equal(refusalText(403, {}), "sign in again");
  assert.equal(refusalText(500, {}), "upload failed");
  assert.equal(refusalText(0, undefined), "upload failed", "no answer at all");
  assert.equal(refusalText(400, { code: "bad_request" }), "upload failed", "a 400 without a field is not told");
  assert.equal(refusalText(400, { field: 7 }), "upload failed", "a field that is not a string is ignored");
  assert.equal(refusalText(400, "size"), "upload failed", "a body that is not an object is ignored");
  assert.equal(refusalText(400, null), "upload failed");
  assert.equal(refusalText(400, [{ field: "size" }]), "upload failed");
  assert.equal(refusalText(400, { message: "size", error: "name" }), "upload failed", "no member but field counts");
  assert.equal(refusalText(Number.NaN, {}), "upload failed");
  assert.equal(notUploaded("too large"), "Not uploaded: too large.");
});

test("an answer of the upload is read only when it holds an absolute path and a size", () => {
  assert.deepEqual(readUploaded({ path: "/home/p/uploads/a.txt", size: 12 }), { path: "/home/p/uploads/a.txt", size: 12 });
  assert.deepEqual(readUploaded({ path: "/a", size: 0, extra: 1 }), { path: "/a", size: 0 });
  assert.deepEqual(readUploaded({ path: "/home/p/a\nb", size: 1 }), { path: "/home/p/a\nb", size: 1 }, "a newline is quoted, not refused");
  assert.equal(readUploaded({ path: `/${"a".repeat(4095)}`, size: 1 })?.size, 1, "4,096 characters are the limit");
  assert.equal(readUploaded({ path: `/${"a".repeat(4096)}`, size: 1 }), null);
  for (const bad of [
    null, undefined, "x", 3, [], {}, { path: "/a" }, { size: 1 }, { path: "a/b", size: 1 }, { path: "", size: 1 }, { path: "~/a", size: 1 },
    { path: "/a\u0000b", size: 1 }, { path: "/a\u001bb", size: 1 }, { path: "/a\tb", size: 1 }, { path: "/a\u007fb", size: 1 },
    { path: 7, size: 1 }, { path: "/a", size: -1 }, { path: "/a", size: 1.5 }, { path: "/a", size: "1" },
    { path: "/a", size: Number.MAX_SAFE_INTEGER + 1 }, { path: "/a", size: Number.NaN },
  ]) {
    assert.equal(readUploaded(bad), null, JSON.stringify(bad));
  }
});

test("the folder of an answer is the directory part of its path", () => {
  assert.equal(uploadFolder("/home/p/uploads/a b.txt"), "/home/p/uploads");
  assert.equal(uploadFolder("/a.txt"), "/");
  assert.equal(uploadFolder("/home/p/uploads/"), "/home/p/uploads");
  assert.equal(uploadFolder("relative"), "");
});

test("at most 16 files are taken from a drop, folders are left and both are counted, not dropped silently", () => {
  const files = (n) => Array.from({ length: n }, (_, i) => ({ id: i }));
  assert.deepEqual(takeFiles([]), { files: [], over: 0, folders: 0 });
  assert.equal(takeFiles(files(16)).files.length, 16);
  assert.equal(takeFiles(files(16)).over, 0);
  const many = takeFiles(files(17));
  assert.deepEqual([many.files.length, many.over, many.folders], [16, 1, 0]);
  assert.deepEqual(many.files.map((f) => f.id), Array.from({ length: 16 }, (_, i) => i), "the first 16, in order");
  const mixed = takeFiles([{ id: 0 }, { id: 1, directory: true }, { id: 2, directory: false }, { id: 3, directory: true }]);
  assert.deepEqual(mixed.files.map((f) => f.id), [0, 2]);
  assert.deepEqual([mixed.over, mixed.folders], [0, 2]);
  const folders = takeFiles(Array.from({ length: 20 }, () => ({ directory: true })));
  assert.deepEqual([folders.files.length, folders.over, folders.folders], [0, 0, 20]);
  const late = takeFiles([...Array.from({ length: 3 }, () => ({ directory: true })), ...files(18)]);
  assert.deepEqual([late.files.length, late.over, late.folders], [16, 2, 3], "folders do not use up the 16");
  assert.equal(MAX_DROP_FILES, 16);
});

test("the sentences of an upload are the ones the design gives", () => {
  assert.equal(DROP_LABEL, "Drop to upload");
  assert.equal(ONLY_SIXTEEN, "Only 16 files at a time.");
  assert.equal(FOLDERS_SKIPPED, "Folders are not uploaded.");
  assert.equal(uploadingText("a b.txt"), "Uploading a b.txt");
  assert.equal(uploadingText("x\u0007y‮z"), "Uploading x�y�z", "a control or direction character in a name is shown as a replacement mark");
  assert.equal(uploadingText("n".repeat(100)), `Uploading ${"n".repeat(47)}…`, "a long name is cut for the status line");
  assert.equal(uploadedText(1), "Uploaded");
  assert.equal(uploadedText(3), "Uploaded 3 files");
});

test("the end of a run of uploads says how many arrived, the last refusal, and what was left out", () => {
  const none = { ok: 0, failed: [], over: 0, folders: 0 };
  assert.equal(batchText({ ...none, ok: 1 }), "Uploaded");
  assert.equal(batchText({ ...none, ok: 3 }), "Uploaded 3 files");
  assert.equal(batchText({ ...none, failed: ["too large"] }), "Not uploaded: too large.");
  assert.equal(batchText({ ...none, failed: ["name not allowed", "try again"] }), "Not uploaded: try again.", "the last refusal speaks");
  assert.equal(batchText({ ...none, ok: 2, failed: ["too large"] }), "Uploaded 2 files. Not uploaded: too large.");
  assert.equal(batchText({ ...none, ok: 16, over: 1 }), "Uploaded 16 files. Only 16 files at a time.");
  assert.equal(batchText({ ...none, ok: 1, folders: 2 }), "Uploaded. Folders are not uploaded.");
  assert.equal(batchText({ ...none, folders: 1 }), "Folders are not uploaded.");
  assert.equal(batchText({ ...none, over: 3, folders: 1 }), "Only 16 files at a time. Folders are not uploaded.");
  assert.equal(batchText(none), "");
});

// ---- download ---------------------------------------------------------------------------------------------------------

test("the sentence for a file kind that is not granted is the one the design gives", () => {
  assert.equal(DOWNLOAD_ONLY_SENTENCE, "Download works for images, video, audio and PDF files.");
  assert.equal(MAX_GRANTS, 16);
});

test("the grant list keeps the last 16 tokens and hands back the oldest when it is full", () => {
  const list = new GrantList();
  assert.deepEqual(list.all(), []);
  for (let i = 1; i <= 16; i++) assert.equal(list.add(`t${i}`), null, `token ${i} fits`);
  assert.equal(list.all().length, 16);
  assert.equal(list.add("t17"), "t1", "the 17th pushes the first out");
  assert.equal(list.add("t18"), "t2");
  assert.deepEqual(list.all(), Array.from({ length: 16 }, (_, i) => `t${i + 3}`), "oldest first");
  list.all().length = 0;
  assert.equal(list.all().length, 16, "the array is a copy");
});

test("a token that is already held is not added twice and pushes nothing out", () => {
  const list = new GrantList();
  assert.equal(list.add("a"), null);
  assert.equal(list.add("a"), null);
  assert.deepEqual(list.all(), ["a"]);
  for (let i = 0; i < 15; i++) list.add(`b${i}`);
  assert.equal(list.add("a"), null, "a duplicate of a full list evicts nothing");
  assert.equal(list.all().length, 16);
});

test("clear gives back every token held, oldest first, and empties the list", () => {
  const list = new GrantList();
  for (const t of ["a", "b", "c"]) list.add(t);
  assert.deepEqual(list.clear(), ["a", "b", "c"]);
  assert.deepEqual(list.all(), []);
  assert.deepEqual(list.clear(), []);
});

test("a grant is read only when its address is the same-origin path of its own token", () => {
  assert.deepEqual(readGrant({ url: "/files/abc-123_X", token: "abc-123_X", mime: "image/png", size: 5 }), { url: "/files/abc-123_X", token: "abc-123_X" });
  for (const bad of [
    null, "x", [], {}, { token: "abc" }, { url: "/files/abc" }, { url: "/files/abc", token: "abd" }, { url: "/files/abc", token: 7 },
    { url: "https://evil.example/files/abc", token: "abc" }, { url: "//evil.example/files/abc", token: "abc" }, { url: "holzfile://abc", token: "abc" },
    { url: "/files/abc/x", token: "abc/x" }, { url: "/files/../abc", token: "../abc" }, { url: "/files/a b", token: "a b" }, { url: "/files/", token: "" },
    { url: "/files/abc?x=1", token: "abc?x=1" }, { url: "/other/abc", token: "abc" }, { url: `/files/${"a".repeat(129)}`, token: "a".repeat(129) },
  ]) {
    assert.equal(readGrant(bad), null, JSON.stringify(bad));
  }
});

test("the name of a downloaded file is the last part of its path", () => {
  assert.equal(baseName("/home/p/a b.png"), "a b.png");
  assert.equal(baseName("a.png"), "a.png");
  assert.equal(baseName("C:\\Users\\p\\a.png"), "a.png");
  assert.equal(baseName("/home/p/"), "download");
  assert.equal(baseName(""), "download");
  assert.equal(baseName("/home/p/a\u0007b.png"), "a\ufffdb.png", "a control character is not kept in a file name");
});
