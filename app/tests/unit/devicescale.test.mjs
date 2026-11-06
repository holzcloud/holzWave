// The device-local UI scale on the compiled output in dist/: what the storage may hold, what survives a storage that throws, and the
// two rules that make a pinch change the text on a touch layout only.
import test from "node:test";
import assert from "node:assert/strict";
import { DEVICE_SCALE_KEY, effectiveScale, getDeviceScale, isDeviceScale, setDeviceScale, terminalFontPx } from "../../dist/devicescale.js";

/** A Web Storage stand-in over a Map. */
function memory(seed = {}) {
  const map = new Map(Object.entries(seed));
  return {
    map,
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => void map.set(k, String(v)),
    removeItem: (k) => void map.delete(k),
  };
}

test("the key is hw.deviceScale", () => {
  assert.equal(DEVICE_SCALE_KEY, "hw.deviceScale");
});

test("nothing stored, or anything that is not a scale of a pinch, reads as null", () => {
  assert.equal(getDeviceScale(memory()), null);
  const bad = ["garbage", "", " ", "125.0", "125.5", "NaN", "Infinity", "-100", "+125", " 125", "125 ", "0125", "1e3", "0x7d", "74", "76", "80.0",
    "205", "70", "0", "1000", "99999999999999999999", "x".repeat(10_000), "1".repeat(10_000), "125\n", "null", "undefined", "١٢٥"];
  for (const text of bad) assert.equal(getDeviceScale(memory({ [DEVICE_SCALE_KEY]: text })), null, JSON.stringify(text.slice(0, 20)));
  assert.equal(getDeviceScale(memory({ "hw.other": "125" })), null, "an unrelated key");
  assert.equal(getDeviceScale(null), null);
  assert.equal(getDeviceScale(undefined), null);
});

test("a stored multiple of 5 from 75 to 200 is read", () => {
  for (let n = 75; n <= 200; n += 5) assert.equal(getDeviceScale(memory({ [DEVICE_SCALE_KEY]: String(n) })), n);
});

test("setDeviceScale stores the plain text and null removes it", () => {
  const s = memory();
  setDeviceScale(125, s);
  assert.equal(s.map.get(DEVICE_SCALE_KEY), "125");
  assert.equal(getDeviceScale(s), 125);
  setDeviceScale(null, s);
  assert.equal(s.map.has(DEVICE_SCALE_KEY), false);
  assert.equal(getDeviceScale(s), null);
});

test("a value a pinch cannot give is not stored", () => {
  const s = memory();
  for (const v of [Number.NaN, Infinity, 101, 70, 205, 12.5, -100, 0, "125", undefined]) setDeviceScale(v, s);
  assert.equal(s.map.size, 0);
  assert.equal(isDeviceScale(125), true);
  assert.equal(isDeviceScale(130.5), false);
});

test("a storage that throws on read or write is survived", () => {
  const boom = () => {
    throw new DOMException("denied", "SecurityError");
  };
  const broken = { getItem: boom, setItem: boom, removeItem: boom };
  assert.equal(getDeviceScale(broken), null);
  assert.doesNotThrow(() => setDeviceScale(125, broken));
  assert.doesNotThrow(() => setDeviceScale(null, broken));
  assert.doesNotThrow(() => setDeviceScale(125, null));
});

test("the device scale replaces the shared one on a touch layout only", () => {
  assert.equal(effectiveScale({ shared: 100, device: 125, touch: true }), 125);
  assert.equal(effectiveScale({ shared: 100, device: null, touch: true }), 100, "no device value: the shared one");
  assert.equal(effectiveScale({ shared: 100, device: 125, touch: false }), 100, "the desktop never sees it");
  assert.equal(effectiveScale({ shared: 150, device: 75, touch: false }), 150);
});

test("on a touch layout the font follows font size times scale, held to 8 to 40; elsewhere it is the font size", () => {
  assert.equal(terminalFontPx({ fontPx: 14, scale: 125, touch: true }), 18, "14 x 125 % = 17.5 rounds to 18");
  assert.equal(terminalFontPx({ fontPx: 14, scale: 100, touch: true }), 14);
  assert.equal(terminalFontPx({ fontPx: 14, scale: 75, touch: true }), 11);
  assert.equal(terminalFontPx({ fontPx: 40, scale: 200, touch: true }), 40, "held to the top of the range");
  assert.equal(terminalFontPx({ fontPx: 8, scale: 75, touch: true }), 8, "held to the bottom");
  for (const scale of [75, 100, 125, 200, 300, Number.NaN]) {
    assert.equal(terminalFontPx({ fontPx: 14, scale, touch: false }), 14, `the desktop rule is unchanged at ${scale}`);
  }
  assert.equal(terminalFontPx({ fontPx: 14, scale: Number.NaN, touch: true }), 14);
});
