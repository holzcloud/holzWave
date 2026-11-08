// The pure half of the group `Window frame` (framegroupstate.ts): how the answer of `window_frame_status` and
// `window_frame_set` is read, the page's own word tables and the rows and sentences the group draws. No DOM is needed.
// The words are held to the design record (docs/design.md 6.33) here.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  BUTTON_OPTIONS, FRAME_OPTIONS, GROUP, describeFrameGroup, readFrameView,
} from "../../dist/framegroupstate.js";
import { GROUP as HOST_SHELL_GROUP } from "../../dist/hostshellstate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const spec = readFileSync(new URL("../../../docs/design.md", import.meta.url), "utf8");

/** The hostile word of these tests; it appears in test code only. */
const CANARY = "zq-window-frame-7b2d";

test("every sample of the golden is read as the shell wrote it", () => {
  const samples = golden.answers.window_frame;
  assert.ok(samples.length >= 4, "the view that does not apply and views with every kind of word");
  for (const sample of samples) {
    assert.deepEqual(Object.keys(sample).sort(), ["applies", "buttons", "frame", "pending"]);
    const view = readFrameView(sample);
    assert.deepEqual(view, sample, JSON.stringify(sample));
    assert.notEqual(view, sample, "a fresh object");
    assert.equal(Object.getPrototypeOf(view), Object.prototype);
  }
  assert.equal(samples.filter((s) => !s.applies).length, 1);
  assert.ok(samples.some((s) => s.pending), "a view whose choice waits for the next start");
});

test("a view shares nothing with its input and no member of a newer shell reaches the page", () => {
  const extra = JSON.parse(
    `{"applies":true,"pending":false,"frame":"own","buttons":"flat","__proto__":{"frame":"system"},"${CANARY}":1}`,
  );
  const view = readFrameView(extra);
  assert.deepEqual(view, { applies: true, pending: false, frame: "own", buttons: "flat" });
  assert.equal(Object.keys(view).length, 4);
  assert.ok(!JSON.stringify(view).includes(CANARY));
  extra.frame = "system";
  assert.equal(view.frame, "own");
});

test("hostile input: no view for anything that is not one, `auto` for a word that is in no table", () => {
  const ok = { applies: true, pending: false, frame: "own", buttons: "flat" };
  for (const bad of [null, undefined, "applies", 7, true, [], [ok], {}, { ...ok, applies: 1 }, { ...ok, applies: "true" },
    { ...ok, pending: "no" }, { ...ok, pending: null }, { applies: true, frame: "own", buttons: "flat" },
    { pending: false, frame: "own", buttons: "flat" }]) {
    assert.equal(readFrameView(bad), null, JSON.stringify(bad));
  }
  for (const word of ["", "OWN", "Own", "own ", " system", "true", 3, null, undefined, {}, [], `<img src=x onerror=${CANARY}>`,
    "__proto__", "toString", "round", "flat"]) {
    const view = readFrameView({ ...ok, frame: word });
    assert.equal(view.frame, "auto", String(word));
    assert.equal(view.buttons, "flat", "the other member is kept");
    assert.equal(view.applies, true);
  }
  for (const word of ["", "FLAT", "flat ", "own", "system", 3, null, undefined, `<b>${CANARY}</b>`, "constructor"]) {
    const view = readFrameView({ ...ok, buttons: word });
    assert.equal(view.buttons, "auto", String(word));
    assert.equal(view.frame, "own");
  }
  // A missing member reads as `auto` too, without discarding the view.
  assert.deepEqual(readFrameView({ applies: true, pending: true }), { applies: true, pending: true, frame: "auto", buttons: "auto" });
  // An inherited member is not a member.
  const inherited = Object.create({ frame: "system" });
  inherited.applies = true;
  inherited.pending = false;
  assert.equal(readFrameView(inherited).frame, "auto");
});

test("the tables have exactly the words of the shell, each with its label", () => {
  assert.deepEqual(FRAME_OPTIONS, [["auto", "Auto"], ["own", "Own"], ["system", "System"]]);
  assert.deepEqual(BUTTON_OPTIONS, [["auto", "Auto"], ["flat", "Flat"], ["round", "Round"]]);
  // The golden's words are all in the tables.
  const frames = new Set(FRAME_OPTIONS.map(([value]) => value));
  const buttons = new Set(BUTTON_OPTIONS.map(([value]) => value));
  for (const sample of golden.answers.window_frame) {
    assert.ok(frames.has(sample.frame) && buttons.has(sample.buttons), JSON.stringify(sample));
  }
});

test("the group differs between Windows and Linux and is not built anywhere else", () => {
  const view = { applies: true, pending: false, frame: "own", buttons: "round" };

  const windows = describeFrameGroup(view, "windows");
  assert.equal(windows.legend, "Window frame");
  assert.deepEqual(windows.rows.map((r) => r.key), ["frame"]);
  assert.equal(windows.rows[0].name, "Frame");
  assert.equal(windows.rows[0].meta, "Windows and Linux");
  assert.equal(windows.rows[0].selected, "own");
  assert.deepEqual(windows.rows[0].options, FRAME_OPTIONS);
  assert.deepEqual(windows.rows[0].advice, [GROUP.adviceFrame], "one advice sentence, no Wayland sentence");
  assert.equal(windows.status, "");

  const linux = describeFrameGroup(view, "linux");
  assert.deepEqual(linux.rows.map((r) => r.key), ["frame", "buttons"]);
  assert.deepEqual(linux.rows[0].advice, [GROUP.adviceFrame, GROUP.adviceWayland], "the Wayland sentence follows the first");
  assert.equal(linux.rows[1].name, "Buttons");
  assert.equal(linux.rows[1].meta, "Linux");
  assert.equal(linux.rows[1].selected, "round");
  assert.deepEqual(linux.rows[1].options, BUTTON_OPTIONS);
  assert.deepEqual(linux.rows[1].advice, [GROUP.adviceButtons]);

  // The status line is the one sentence while the choice waits, and empty otherwise.
  assert.equal(describeFrameGroup({ ...view, pending: true }, "linux").status, "Applies after the next start.");
  assert.equal(describeFrameGroup({ ...view, pending: true }, "windows").status, "Applies after the next start.");
  assert.equal(describeFrameGroup({ ...view, pending: false }, "linux").status, "");

  // Not built: another system, an `os` that is not known, a view that does not apply, no view.
  for (const os of ["macos", "android", "web", "", "Linux", "WINDOWS", null, undefined, 3]) {
    assert.equal(describeFrameGroup(view, os), null, String(os));
  }
  assert.equal(describeFrameGroup({ ...view, applies: false }, "linux"), null);
  assert.equal(describeFrameGroup({ ...view, applies: false }, "windows"), null);
  assert.equal(describeFrameGroup(null, "linux"), null);
  // The description is a copy: changing it does not change the tables.
  windows.rows[0].options.push(["x", "X"]);
  assert.equal(FRAME_OPTIONS.length, 3);
});

// The sentences below are the design record's (docs/design.md 6.33). The page may not say a word the spec does not say.
test("every word and sentence of the page is in the design record", () => {
  const start = spec.indexOf("### 6.33 Window frame");
  assert.ok(start >= 0, "section 6.33 is found");
  const section = spec.slice(start);
  assert.ok(section.length > 2000, "section 6.33 has its text");
  const rows = section.split("\n").filter((l) => l.startsWith("| "));
  const cells = (l) => l.split("|").slice(1, -1).map((c) => c.trim());
  const find = (name, what) => {
    const line = rows.find((l) => cells(l)[0] === name);
    assert.ok(line, `the spec has the row for ${what}`);
    return cells(line);
  };
  const tick = (text) => text.replace(/^`|`$/g, "");

  // Legend.
  assert.equal(tick(find("Legend", "the legend")[1]), GROUP.legend, "legend");

  // The row `Frame`: its name, the dim meta and its three choices.
  const frame = find("Row", "the row Frame")[1];
  assert.ok(frame.includes(`\`${GROUP.frameName}\``), `row name ${GROUP.frameName}`);
  assert.ok(frame.includes(`the dim meta \`${GROUP.frameMeta}\``), `meta ${GROUP.frameMeta}`);
  for (const [, label] of FRAME_OPTIONS) assert.ok(frame.includes(`\`${label}\``), `option ${label}`);

  // The row `Buttons`: drawn on Linux only.
  const buttons = find("Row, drawn on Linux only", "the row Buttons")[1];
  assert.ok(buttons.includes(`\`${GROUP.buttonsName}\``), `row name ${GROUP.buttonsName}`);
  assert.ok(buttons.includes(`the dim meta \`${GROUP.buttonsMeta}\``), `meta ${GROUP.buttonsMeta}`);
  for (const [, label] of BUTTON_OPTIONS) assert.ok(buttons.includes(`\`${label}\``), `option ${label}`);

  // The three advice sentences and the status sentence, each between backticks in the words column.
  const adviceFrame = rows.map(cells).find((c) => c[0] === "Advice under `Frame`");
  assert.ok(adviceFrame, "the spec has the advice under Frame");
  assert.equal(tick(adviceFrame[1]), GROUP.adviceFrame, "the advice under Frame");
  const wayland = rows.map(cells).find((c) => c[0].startsWith("Advice under `Frame`, Linux only"));
  assert.ok(wayland, "the spec has the Wayland sentence");
  assert.equal(tick(wayland[1]), GROUP.adviceWayland, "the Wayland sentence");
  const adviceButtons = rows.map(cells).find((c) => c[0] === "Advice under `Buttons`");
  assert.ok(adviceButtons, "the spec has the advice under Buttons");
  assert.equal(tick(adviceButtons[1]), GROUP.adviceButtons, "the advice under Buttons");
  const status = rows.map(cells).find((c) => c[0].startsWith("Status line"));
  assert.ok(status, "the spec has the status line");
  assert.ok(status[0].includes('role="status"'), "the status line is a status");
  assert.equal(tick(status[1]), GROUP.status, "the status sentence");

  // The words of the shell, as the spec lists them.
  assert.ok(section.includes("`auto`, `own`, `system` for the first row and `auto`, `flat`, `round` for the second"), "the words");
  assert.ok(section.includes("a word that is in no table reads as `auto`"), "an unknown word reads as auto");
  assert.ok(section.includes("whenever the stored choice differs from the one the open windows were built with"), "when the status shows");
  assert.ok(section.includes("`role=\"group\"` labelled by its legend"), "the group is a labelled fieldset");
  assert.ok(section.includes("after Accessibility and the group `Host shell` where that is drawn, and before the buttons"), "the place");
});

// The design record names no sentence for a command that failed without a reason. The page has one line for that in every group
// of the dialog, and this group says the same words as the group `Host shell` so a person reads one sentence for one thing.
test("the line of a command that failed with no reason is the dialog's one sentence", () => {
  assert.equal(GROUP.failed, HOST_SHELL_GROUP.failed);
  assert.equal(GROUP.failed, "The command failed.");
});
