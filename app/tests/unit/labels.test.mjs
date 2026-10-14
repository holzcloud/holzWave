// The texts of the chrome on the compiled output in dist/: no DOM is needed to import or run them.
import test from "node:test";
import assert from "node:assert/strict";
import { NOTICE_MS, clockText, paneCountText, refusalText, rendererText, shortenHome, tabLabel } from "../../dist/labels.js";

test("the module imports and runs without a DOM", () => {
  assert.equal(typeof document, "undefined");
  assert.equal(typeof tabLabel, "function");
});

test("a tab label is its number, a space and its title", () => {
  assert.equal(tabLabel(0, "shell"), "1 shell");
  assert.equal(tabLabel(1, "notes.md"), "2 notes.md");
  assert.equal(tabLabel(8, "ninth"), "9 ninth");
  assert.equal(tabLabel(11, "twelve"), "12 twelve", "the number is the position, not a digit chord");
});

test("a tab with an empty title is only its number", () => {
  assert.equal(tabLabel(0, ""), "1");
});

test("a title is never parsed: markup stays text", () => {
  assert.equal(tabLabel(0, "<b>x</b>"), "1 <b>x</b>");
});

test("the clock is HH:MM in local time, zero padded", () => {
  assert.equal(clockText(new Date(2026, 0, 5, 9, 5, 59)), "09:05");
  assert.equal(clockText(new Date(2026, 0, 5, 14, 32, 0)), "14:32");
  assert.equal(clockText(new Date(2026, 0, 5, 0, 0, 0)), "00:00");
  assert.equal(clockText(new Date(2026, 0, 5, 23, 59, 59)), "23:59");
});

test("the clock of an invalid date is a placeholder, never NaN", () => {
  assert.equal(clockText(new Date(Number.NaN)), "--:--");
});

test("the pane count is singular for one and plural otherwise", () => {
  assert.equal(paneCountText(1), "1 pane");
  assert.equal(paneCountText(2), "2 panes");
  assert.equal(paneCountText(0), "0 panes");
  assert.equal(paneCountText(12), "12 panes");
});

test("the pane count of a number that is not a count is zero", () => {
  assert.equal(paneCountText(Number.NaN), "0 panes");
  assert.equal(paneCountText(-3), "0 panes");
  assert.equal(paneCountText(Number.POSITIVE_INFINITY), "0 panes");
});

test("the home prefix of a macOS path is a tilde", () => {
  assert.equal(shortenHome("/Users/holz/Projects/holzWave", "macos"), "~/Projects/holzWave");
  assert.equal(shortenHome("/Users/holz", "macos"), "~");
  assert.equal(shortenHome("/Users/holz/", "macos"), "~/");
});

test("the home prefix of a Linux path is a tilde", () => {
  assert.equal(shortenHome("/home/holz/src/app", "linux"), "~/src/app");
  assert.equal(shortenHome("/home/holz", "linux"), "~");
});

test("the home prefix of a Windows path is a tilde, with either slash and the leading slash of a file URL path", () => {
  assert.equal(shortenHome("C:\\Users\\holz\\src", "windows"), "~\\src");
  assert.equal(shortenHome("C:/Users/holz/src", "windows"), "~/src");
  assert.equal(shortenHome("/C:/Users/holz/src", "windows"), "~/src");
});

test("a path outside a home directory is unchanged", () => {
  assert.equal(shortenHome("/etc/nginx", "macos"), "/etc/nginx");
  assert.equal(shortenHome("/Users", "macos"), "/Users");
  assert.equal(shortenHome("/Users/", "macos"), "/Users/");
  assert.equal(shortenHome("/var/home/holz", "linux"), "/var/home/holz");
  assert.equal(shortenHome("/Usersfoo/bar", "macos"), "/Usersfoo/bar", "the prefix is a whole path segment");
});

test("the shared folder of macOS is not a home directory", () => {
  assert.equal(shortenHome("/Users/Shared/data", "macos"), "/Users/Shared/data");
});

test("the platform's convention is the only one used", () => {
  assert.equal(shortenHome("/home/holz/x", "macos"), "/home/holz/x");
  assert.equal(shortenHome("/Users/holz/x", "linux"), "/Users/holz/x");
  assert.equal(shortenHome("/Users/holz/x", ""), "/Users/holz/x", "an unknown platform leaves the path alone");
});

test("a path that is not a path is returned as text", () => {
  assert.equal(shortenHome("", "macos"), "");
  assert.equal(shortenHome("~/x", "macos"), "~/x");
});

test("the renderer segment is the name and the draw time", () => {
  assert.equal(rendererText("webgl2", 0.4237), "webgl2 0.42 ms");
  assert.equal(rendererText("canvas2d", 12), "canvas2d 12.00 ms");
});

test("the renderer segment is the name alone before a draw time exists", () => {
  assert.equal(rendererText("webgl2", Number.NaN), "webgl2");
  assert.equal(rendererText("webgl2", -1), "webgl2");
  assert.equal(rendererText("webgl2", Number.POSITIVE_INFINITY), "webgl2");
});

test("a draw time of zero is a measurement: a timer coarser than the draw reads 0 ms", () => {
  assert.equal(rendererText("canvas2d", 0), "canvas2d 0.00 ms");
});

test("a refused tear-off is told in one short line, with the shell's reason when there is one", () => {
  assert.equal(refusalText("pane"), "Pane stays here: it cannot get a window of its own");
  assert.equal(refusalText("tab"), "Tab stays here: it cannot get a window of its own");
  assert.equal(refusalText("tab", "this cannot be a window of its own"), "Tab not moved: this cannot be a window of its own");
  assert.equal(refusalText("pane", "  \u001b[31mcannot\nopen a window\u0007 "), "Pane not moved: [31mcannot open a window", "control characters are gone");
  assert.equal(refusalText("pane", "x".repeat(500)).length, "Pane not moved: ".length + 80, "cut to 80 characters");
  for (const odd of [undefined, null, 42, {}, "", "   "]) assert.match(refusalText("pane", odd), /^Pane stays here/, String(odd));
  assert.ok(NOTICE_MS >= 2000 && NOTICE_MS <= 10000, "long enough to read, short enough to be gone");
});
