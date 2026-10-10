// The terminal font: JetBrains Mono is bundled (D-17), declared in app.css, loaded before the first measurement,
// and the cell metrics that come out of the measurement are whole device pixels (RESEARCH Pitfall 11).
import assert from "node:assert/strict";
import { newPage } from "./lib.mjs";

const FILES = ["JetBrainsMono-Regular.woff2", "JetBrainsMono-Bold.woff2", "JetBrainsMono-Italic.woff2"];

/** Record, at the first canvas measurement, what state the page's JetBrains Mono faces are in. */
const recordFirstMeasure = () => {
  window.__measureLog = [];
  const original = CanvasRenderingContext2D.prototype.measureText;
  CanvasRenderingContext2D.prototype.measureText = function (text) {
    const faces = [...document.fonts].filter((f) => f.family.includes("JetBrains Mono"));
    window.__measureLog.push({ text, faces: faces.map((f) => `${f.style} ${f.weight} ${f.status}`) });
    return original.call(this, text);
  };
};

async function metricsAt(page, url) {
  await page.addInitScript(recordFirstMeasure);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  return page.evaluate(async () => {
    const { state } = await import("./state.js");
    return {
      dpr: window.devicePixelRatio,
      metrics: { ...state.metrics },
      log: window.__measureLog,
      check: document.fonts.check('14px "JetBrains Mono"'),
      bold: document.fonts.check('bold 14px "JetBrains Mono"'),
      italic: document.fonts.check('italic 14px "JetBrains Mono"'),
      faces: [...document.fonts]
        .filter((f) => f.family.includes("JetBrains Mono"))
        .map((f) => ({ style: f.style, weight: f.weight, status: f.status, display: f.display })),
    };
  });
}

export default async function ({ page, url }) {
  // ---- the files are served as fonts, with their licence -----------------------------------------------------
  for (const name of FILES) {
    const res = await fetch(`${url}fonts/${name}`);
    assert.equal(res.status, 200, `${name} is not served`);
    assert.match(res.headers.get("content-type") ?? "", /^(font\/woff2|application\/font-woff2)/, `${name} content type`);
    const bytes = new Uint8Array(await res.arrayBuffer());
    assert.ok(bytes.length > 10_000, `${name} is too small to be the font (${bytes.length} bytes)`);
    assert.equal(String.fromCharCode(...bytes.subarray(0, 4)), "wOF2", `${name} is not a woff2 file`);
  }
  const licence = await fetch(`${url}fonts/OFL.txt`);
  assert.equal(licence.status, 200, "the licence is not served next to the fonts");
  assert.match(await licence.text(), /SIL Open Font License/, "OFL.txt is not the licence text");

  // ---- the page loads the three faces before it measures ------------------------------------------------------
  const one = await metricsAt(page, url);
  assert.equal(one.check, true, 'document.fonts.check(\'14px "JetBrains Mono"\')');
  assert.equal(one.bold, true, "bold face is not loaded");
  assert.equal(one.italic, true, "italic face is not loaded");
  const faces = one.faces.map((f) => `${f.style} ${f.weight} ${f.status}`).sort();
  assert.deepEqual(faces, ["italic 400 loaded", "normal 400 loaded", "normal 700 loaded"], "the three @font-face rules");
  assert.ok(one.faces.every((f) => f.display === "block"), "font-display must be block");
  assert.ok(one.log.length > 0, "the page never measured text");
  assert.deepEqual(
    one.log[0].faces.slice().sort(),
    ["italic 400 loaded", "normal 400 loaded", "normal 700 loaded"],
    "the first measurement ran before the fonts were loaded",
  );
  const m1 = one.metrics;
  assert.equal(one.dpr, 1);
  assert.ok(m1.cellW > 0 && m1.cellH > 0, "no cell metrics");
  assert.ok(Number.isInteger(m1.cellW * 1) && Number.isInteger(m1.cellH * 1), `dpr 1 metrics ${m1.cellW} x ${m1.cellH}`);

  // ---- dpr 2: the cell is a whole number of device pixels, not of CSS pixels ----------------------------------
  const browser = page.context().browser();
  const made = await newPage(browser, { deviceScaleFactor: 2 });
  try {
    const two = await metricsAt(made.page, url);
    assert.equal(two.dpr, 2);
    const { cellW, cellH } = two.metrics;
    assert.ok(Number.isInteger(cellW * 2), `cellW ${cellW} is not whole device pixels at dpr 2`);
    assert.ok(Number.isInteger(cellH * 2), `cellH ${cellH} is not whole device pixels at dpr 2`);
    assert.equal(two.log[0].faces.length, 3, "dpr 2: the first measurement saw no JetBrains Mono faces");
    assert.ok(two.log[0].faces.every((f) => f.endsWith(" loaded")), "dpr 2: fonts were not loaded at the first measurement");
    assert.deepEqual(made.errors, [], "page errors or CSP violations at dpr 2");
  } finally {
    await made.page.close().catch(() => {});
  }
}
