// The decision and the preview of a paste confirmation on the compiled output in dist/. Neither touches the DOM
// at import, so node imports the module. The outcome strings come from the golden protocol fixture, not from a
// copy in the page.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { PREVIEW_COLUMNS, PREVIEW_LINES, pasteDecision, previewOf } from "../../dist/paste-confirm.js";

const here = dirname(fileURLToPath(import.meta.url));
const golden = JSON.parse(readFileSync(join(here, "../fixtures/protocol.json"), "utf8"));
const [WRITTEN, EMPTY, REJECTED] = golden.paste;

test("the outcome strings are the ones of the golden protocol", () => {
  assert.deepEqual(golden.paste, ["written", "empty", "rejected"]);
});

test("pasteDecision: written and empty are done, whatever the preference or the text", () => {
  for (const outcome of [WRITTEN, EMPTY]) {
    for (const setting of [true, false]) {
      assert.equal(pasteDecision(outcome, "a\nb", setting), "done", `${outcome} with preference ${setting}`);
    }
  }
});

test("pasteDecision: rejected asks when the preference is on", () => {
  assert.equal(pasteDecision(REJECTED, "a\nb", true), "confirm");
});

test("pasteDecision: rejected is re-sent without asking when the preference is off and no marker is present", () => {
  assert.equal(pasteDecision(REJECTED, "a\nb", false), "resend");
});

test("pasteDecision: the bracketed-paste end marker always asks, preference or not", () => {
  assert.equal(pasteDecision(REJECTED, "x\u001b[201~\nrm -rf ~", false), "confirm", "ESC [ 2 0 1 ~");
  assert.equal(pasteDecision(REJECTED, "x\u009b201~\ny", false), "confirm", "the 8-bit CSI spelling");
  assert.equal(pasteDecision(REJECTED, "x\u001b[200~\ny", false), "resend", "the start marker alone is not the end marker");
});

test("pasteDecision: an answer the page does not know is done, never a send", () => {
  for (const outcome of [null, undefined, "", "Rejected", 3, {}]) {
    assert.equal(pasteDecision(outcome, "a\nb", false), "done", String(outcome));
  }
});

test("previewOf counts lines for LF, CRLF and CR and ignores one trailing break", () => {
  assert.equal(previewOf("ls").lines, 1);
  assert.equal(previewOf("a\nb").lines, 2);
  assert.equal(previewOf("a\r\nb\r\nc").lines, 3);
  assert.equal(previewOf("a\rb").lines, 2);
  assert.equal(previewOf("a\nb\n").lines, 2, "a final newline does not add a line");
  assert.equal(previewOf("a\n\nb").lines, 3, "an empty line is a line");
  assert.equal(previewOf("").lines, 0);
});

test("previewOf shows at most 5 lines, each cut at 80 characters", () => {
  assert.equal(PREVIEW_LINES, 5);
  assert.equal(PREVIEW_COLUMNS, 80);
  const text = ["one", "two", "x".repeat(200), "four", "five", "six", "seven"].join("\n");
  const p = previewOf(text);
  assert.equal(p.lines, 7);
  assert.equal(p.shown.length, 5);
  assert.equal(p.hidden, 2);
  assert.deepEqual(p.shown.slice(0, 2), ["one", "two"]);
  assert.ok([...p.shown[2]].length <= PREVIEW_COLUMNS + 1, `cut line is ${[...p.shown[2]].length} characters`);
  assert.ok(p.shown[2].startsWith("x".repeat(PREVIEW_COLUMNS)), "the first 80 characters are kept");
  assert.ok(p.shown[2].endsWith("\u2026"), "a cut line says so");
});

test("previewOf cuts by code point: an emoji is one character", () => {
  const line = "\u{1F600}".repeat(100);
  const p = previewOf(line);
  assert.equal([...p.shown[0]].filter((c) => c === "\u{1F600}").length, PREVIEW_COLUMNS);
});

test("previewOf shows control characters as visible markers and flags control sequences", () => {
  const p = previewOf("echo \u001b[31mred\u0007\u007f\u0085\nnext");
  assert.equal(p.control, true);
  assert.equal(p.shown[0], "echo \u241b[31mred\u2407\u2421<U+0085>");
  assert.ok(![...p.shown.join("")].some((c) => c.charCodeAt(0) < 0x20 || c.charCodeAt(0) === 0x7f), "no raw control character");
});

test("previewOf marks invisible and bidirectional format characters", () => {
  const p = previewOf("safe\u202egnp.exe\u200b\ufeff");
  assert.equal(p.shown[0], "safe<U+202E>gnp.exe<U+200B><U+FEFF>");
  assert.equal(p.control, false, "format characters are marked but are not terminal control sequences");
});

test("previewOf does not flag tabs, newlines or plain text as control sequences", () => {
  assert.equal(previewOf("a\tb\nc\r\nd").control, false);
  assert.equal(previewOf("plain text").control, false);
});

test("previewOf keeps markup as text and stays cheap on a large paste", () => {
  assert.deepEqual(previewOf("<img src=x onerror=alert(1)>").shown, ["<img src=x onerror=alert(1)>"]);
  const big = "y".repeat(2 * 1024 * 1024) + "\n" + "z\n".repeat(1000);
  const t0 = performance.now();
  const p = previewOf(big);
  assert.equal(p.lines, 1001);
  assert.equal(p.shown.length, 5);
  assert.ok([...p.shown[0]].length <= PREVIEW_COLUMNS + 1);
  assert.ok(performance.now() - t0 < 500, "a 2 MiB paste is previewed in well under a second");
});
