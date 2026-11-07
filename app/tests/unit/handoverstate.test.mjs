// The pure model of the file hand-over page (handoverstate.ts, docs/design.md section 6.6): the size as a person reads it, the host's
// answer read defensively, the line each answer gives, and the guard that keeps the page's words equal to the spec's.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  ACTION_IN,
  ACTION_OUT,
  LIMIT_MB,
  MAX_NAME_CHARS,
  STRINGS,
  WARN_MARK,
  formatSize,
  messageFor,
  parseOutcome,
} from "../../dist/handoverstate.js";

test("formatSize writes decimal units as the spec's lines do", () => {
  assert.equal(formatSize(1288490), "1.2 MB");
  assert.equal(formatSize(612000000), "612 MB");
  assert.equal(formatSize(900), "900 B");
  assert.equal(formatSize(1536), "1.5 kB");
  assert.equal(formatSize(0), "0 B");
  assert.equal(formatSize(999), "999 B");
  assert.equal(formatSize(1000), "1 kB");
  assert.equal(formatSize(1999), "1.9 kB", "cut down, never rounded up");
  assert.equal(formatSize(9999), "9.9 kB");
  assert.equal(formatSize(10000), "10 kB");
  assert.equal(formatSize(999999), "999 kB", "no rollover to a thousand kilobytes");
  assert.equal(formatSize(1000000), "1 MB");
  assert.equal(formatSize(268435456), "268 MB");
  assert.equal(formatSize(3_400_000_000), "3.4 GB");
  assert.equal(formatSize(2_000_000_000_000), "2 TB");
});

test("formatSize gives a plain text for a value that is no size and never undefined", () => {
  for (const bad of [-1, -0.5, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, 1e30, Number.MAX_SAFE_INTEGER + 2, undefined, null, "12", {}]) {
    const text = formatSize(bad);
    assert.equal(typeof text, "string", String(bad));
    assert.equal(text, STRINGS.unknownSize, String(bad));
  }
  assert.equal(formatSize(1.9), "1 B", "a fraction of a byte is cut");
});

const done = (over = {}) => ({ kind: "done", name: "report.pdf", size: 1288490, shown: "~/handover/report.pdf", renamed: false, ...over });

test("messageFor gives the spec's lines", () => {
  assert.deepEqual(messageFor(done(), "in"), { text: "Copied report.pdf (1.2 MB) to ~/handover/report.pdf", tone: "plain" });
  assert.deepEqual(messageFor(done({ shown: "~/report.pdf" }), "out"), { text: "Copied report.pdf (1.2 MB) out", tone: "plain" });
  // A name that was taken: the chosen name first, the place with the number after it.
  assert.deepEqual(messageFor(done({ renamed: true, shown: "~/handover/report-2.pdf" }), "in"), {
    text: "Copied report.pdf (1.2 MB) to ~/handover/report-2.pdf",
    tone: "plain",
  });
  const big = { kind: "too_large", name: "report.pdf", size: 612000000 };
  assert.deepEqual(messageFor(big, "in"), { text: "report.pdf is 612 MB; the limit is 256 MB", tone: "warn" });
  assert.deepEqual(messageFor(big, "out"), { text: "report.pdf is 612 MB; the limit is 256 MB", tone: "warn" });
  assert.deepEqual(messageFor({ ...big, size: null }, "in"), { text: "report.pdf is larger than 256 MB", tone: "warn" });
  assert.equal(messageFor({ kind: "cancelled" }, "in"), null, "a dialog that is left says nothing");
  assert.equal(messageFor({ kind: "cancelled" }, "out"), null);
  for (const kind of ["failed", "unavailable"]) {
    assert.deepEqual(messageFor({ kind }, "in"), { text: "Could not copy the file", tone: "warn" }, kind);
    assert.deepEqual(messageFor({ kind }, "out"), { text: "Could not copy the file", tone: "warn" }, kind);
  }
  assert.equal(LIMIT_MB, 256);
});

test("the limit shown is the one constant", () => {
  const text = messageFor({ kind: "too_large", name: "a", size: 300000000 }, "in").text;
  assert.ok(text.endsWith(`the limit is ${LIMIT_MB} MB`), text);
});

test("parseOutcome accepts exactly the kinds of the host", () => {
  assert.deepEqual(parseOutcome({ kind: "done", name: "a.txt", size: 5, shown: "~/handover/a.txt", renamed: true }), {
    kind: "done",
    name: "a.txt",
    size: 5,
    shown: "~/handover/a.txt",
    renamed: true,
  });
  assert.deepEqual(parseOutcome({ kind: "cancelled" }), { kind: "cancelled" });
  assert.deepEqual(parseOutcome({ kind: "failed" }), { kind: "failed" });
  assert.deepEqual(parseOutcome({ kind: "unavailable" }), { kind: "unavailable" });
  assert.deepEqual(parseOutcome({ kind: "too_large", name: "a.bin", size: 7 }), { kind: "too_large", name: "a.bin", size: 7 });
  assert.deepEqual(parseOutcome({ kind: "too_large", name: "a.bin", size: null }), { kind: "too_large", name: "a.bin", size: null });
  assert.deepEqual(parseOutcome({ kind: "too_large", name: "a.bin" }), { kind: "too_large", name: "a.bin", size: null }, "an absent size is unknown");
  assert.deepEqual(parseOutcome({ kind: "failed", text: "a path or a provider's words", extra: 1 }), { kind: "failed" }, "extra fields are not carried");
});

test("parseOutcome is null for an unknown kind or a wrong type and never throws", () => {
  const bad = [
    null, undefined, 7, "done", [], [done()], {}, { kind: 1 }, { kind: "Done" }, { kind: "ok" }, { kind: "DONE" }, { kind: "" },
    done({ name: 1 }), done({ name: null }), done({ size: "5" }), done({ size: -1 }), done({ size: Number.NaN }), done({ size: Number.POSITIVE_INFINITY }),
    done({ shown: 3 }), done({ renamed: "yes" }), done({ renamed: undefined }),
    { kind: "done" }, { kind: "too_large" }, { kind: "too_large", name: 4, size: 1 }, { kind: "too_large", name: "a", size: "9" }, { kind: "too_large", name: "a", size: -3 },
  ];
  for (const value of bad) assert.equal(parseOutcome(value), null, JSON.stringify(value) ?? String(value));
  const hostile = { get kind() { throw new Error("no"); } };
  assert.equal(parseOutcome(hostile), null, "an object that throws when it is read is no answer");
});

test("parseOutcome cuts a name to 255 characters before anything shows it", () => {
  assert.equal(MAX_NAME_CHARS, 255);
  const long = parseOutcome(done({ name: "x".repeat(10000) }));
  assert.equal([...long.name].length, 255);
  const emoji = parseOutcome({ kind: "too_large", name: "😀".repeat(1000), size: 1 });
  assert.equal([...emoji.name].length, 255);
  assert.ok(!/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/.test(emoji.name), "no pair is cut in the middle");
  assert.ok(parseOutcome(done({ shown: "~/handover/" + "y".repeat(10000) })).shown.length <= 512);
  assert.equal(parseOutcome(done({ name: "x".repeat(255) })).name.length, 255);
  // Markup stays text: the model does not touch it.
  assert.equal(parseOutcome(done({ name: "<img src=x onerror=1>" })).name, "<img src=x onerror=1>");
});

// ---- the guard between the page and the spec -------------------------------------------------------

const spec = readFileSync(new URL("../../../docs/design.md", import.meta.url), "utf8");
const start = spec.indexOf("**File hand-over.**");
const end = spec.indexOf("**Notification.**");
const rule = spec.slice(start, end);
const quoted = [...rule.matchAll(/`([^`]+)`/g)].map((m) => m[1]);

test("the spec has the rule the page builds", () => {
  assert.ok(start > 0 && end > start, "the rule File hand-over is in section 6.6");
  assert.ok(quoted.length >= 8, `found ${quoted.length} quoted texts`);
});

test("every string of the page is in the spec's rule word for word", () => {
  const sample = [
    STRINGS.copyIn,
    STRINGS.copyOut,
    STRINGS.askPath,
    STRINGS.failed,
    messageFor(done(), "in").text,
    messageFor(done({ renamed: true, shown: "~/handover/report-2.pdf" }), "in").text,
    messageFor(done({ shown: "~/report.pdf" }), "out").text,
    messageFor({ kind: "too_large", name: "report.pdf", size: 612000000 }, "in").text,
  ];
  for (const text of sample) assert.ok(rule.includes(`\`${text}\``), `the spec does not have \`${text}\``);
});

test("every text the spec puts in backticks is a string or a line of the page", () => {
  const produced = new Set([
    STRINGS.copyIn,
    STRINGS.copyOut,
    STRINGS.askPath,
    STRINGS.failed,
    messageFor(done(), "in").text,
    messageFor(done({ renamed: true, shown: "~/handover/report-2.pdf" }), "in").text,
    messageFor(done({ shown: "~/report.pdf" }), "out").text,
    messageFor({ kind: "too_large", name: "report.pdf", size: 612000000 }, "in").text,
  ]);
  /** An example of a name in the spec's prose, and the mark of a warn line (`WARN_MARK`): not texts of their own. */
  const NOT_TEXT = new Set(["report-2.pdf", WARN_MARK]);
  for (const text of quoted) assert.ok(produced.has(text) || NOT_TEXT.has(text), `the spec has \`${text}\`, which the page does not make`);
});

test("the spec says the limit, the one-at-a-time rule and what does not exist", () => {
  assert.ok(rule.includes("256 MiB"));
  assert.ok(rule.includes("one at a time"));
  assert.ok(rule.includes("no folder mirror and no all-files permission"));
});

test("the ids of the two actions give the palette the section Files", () => {
  for (const id of [ACTION_IN, ACTION_OUT]) assert.equal(id.slice(0, id.indexOf(".")), "files");
  assert.equal(STRINGS.files, "Files");
});
