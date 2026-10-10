// Unit tests run on the compiled output in dist/ with node:test: no test framework, no @types/node.
import test from "node:test";
import assert from "node:assert/strict";
import { BOLD, ITALIC, UNDERLINE, INVERSE, css, font } from "../../dist/frame.js";

test("css pads a colour to six hex digits", () => {
  assert.equal(css(0xff8fb8), "#ff8fb8");
  assert.equal(css(1), "#000001");
  assert.equal(css(0), "#000000");
});

test("font puts italic before bold and names JetBrains Mono", () => {
  const f = font(BOLD | ITALIC, 14);
  assert.ok(f.startsWith("italic bold 14px"), f);
  assert.ok(f.includes("JetBrains Mono"), f);
  assert.ok(font(0, 12).startsWith("12px "), font(0, 12));
});

test("the style flags are distinct powers of two", () => {
  const flags = [BOLD, ITALIC, UNDERLINE, INVERSE];
  for (const f of flags) assert.ok(f > 0 && (f & (f - 1)) === 0, `${f} is not a power of two`);
  assert.equal(new Set(flags).size, flags.length);
});
