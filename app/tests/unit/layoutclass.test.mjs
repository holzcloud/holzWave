// The layout rule on the compiled output in dist/: numbers in, a class out.
import test from "node:test";
import assert from "node:assert/strict";
import { TABLET_MIN_SHORT_SIDE, TABLET_MIN_WIDTH, layoutClass } from "../../dist/layoutclass.js";

test("the thresholds are the ones of the design", () => {
  assert.equal(TABLET_MIN_WIDTH, 840);
  assert.equal(TABLET_MIN_SHORT_SIDE, 480);
});

test("an Android window is a phone or a tablet by its width and the short side of the screen", () => {
  assert.equal(layoutClass({ os: "android", width: 390, shortSide: 390 }), "phone");
  assert.equal(layoutClass({ os: "android", width: 840, shortSide: 480 }), "tablet");
  assert.equal(layoutClass({ os: "android", width: 839, shortSide: 800 }), "phone");
  assert.equal(layoutClass({ os: "android", width: 900, shortSide: 479 }), "phone", "a landscape phone");
  assert.equal(layoutClass({ os: "android", width: 600, shortSide: 800 }), "phone", "a tablet in split screen");
  assert.equal(layoutClass({ os: "android", width: 1280, shortSide: 800 }), "tablet");
});

test("every other platform is a desktop at every width", () => {
  for (const os of ["macos", "windows", "linux", "web", "", "Android", "ios"]) {
    for (const width of [320, 390, 839, 840, 1280, 4000]) {
      assert.equal(layoutClass({ os, width, shortSide: 1000 }), "desktop", `${os || "(empty)"} at ${width}`);
    }
  }
});

test("a number that is not a number gives the phone, never the tablet", () => {
  assert.equal(layoutClass({ os: "android", width: Number.NaN, shortSide: 800 }), "phone");
  assert.equal(layoutClass({ os: "android", width: 900, shortSide: Number.NaN }), "phone");
  assert.equal(layoutClass({ os: "android", width: Number.POSITIVE_INFINITY, shortSide: Number.NaN }), "phone");
});

test("the rule has no memory: the same numbers give the same class in any order", () => {
  const facts = [
    { os: "android", width: 390, shortSide: 390 },
    { os: "android", width: 1280, shortSide: 800 },
    { os: "android", width: 390, shortSide: 390 },
  ];
  assert.deepEqual(facts.map(layoutClass), ["phone", "tablet", "phone"]);
});
