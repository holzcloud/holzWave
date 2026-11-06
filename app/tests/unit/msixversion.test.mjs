// The mapping from a project version to the MSIX (Store) version: scheme A, one pure function. A Store version cannot go
// down once uploaded, so the table below is the contract: a change that moves one of these values breaks it on purpose.
import test from "node:test";
import assert from "node:assert/strict";
import { compareMsixVersions, msixVersion, parseMsixVersion, parseProjectVersion } from "../../../packaging/msix/version.mjs";

test("the mapping gives the values that the owner chose", () => {
  const table = {
    "0.0.1-beta3": "1.0.123.0",
    "0.0.1-beta2": "1.0.122.0",
    "0.0.1": "1.0.199.0",
    "0.0.1-alpha1": "1.0.101.0",
    "0.0.1-rc2": "1.0.162.0",
    "0.1.0": "1.1.99.0",
    "1.2.3": "2.2.399.0",
  };
  for (const [project, store] of Object.entries(table)) assert.equal(msixVersion(project), store, project);
});

test("the mapping is monotonic over the release order", () => {
  const order = [];
  for (const [x, y, z] of [[0, 0, 1], [0, 0, 2], [0, 1, 0], [0, 1, 1], [0, 2, 0], [1, 0, 0], [1, 0, 1], [1, 1, 0], [2, 0, 0]]) {
    const base = `${x}.${y}.${z}`;
    order.push(`${base}-alpha1`, `${base}-alpha19`, `${base}-beta1`, `${base}-beta39`, `${base}-rc1`, `${base}-rc38`, base);
  }
  for (let i = 1; i < order.length; i++) {
    assert.ok(
      compareMsixVersions(msixVersion(order[i - 1]), msixVersion(order[i])) < 0,
      `${order[i - 1]} (${msixVersion(order[i - 1])}) must be lower than ${order[i]} (${msixVersion(order[i])})`,
    );
  }
  // Within one kind the number raises the version.
  for (const kind of ["alpha", "beta", "rc"]) {
    assert.ok(compareMsixVersions(msixVersion(`0.0.1-${kind}1`), msixVersion(`0.0.1-${kind}2`)) < 0, kind);
  }
});

test("a version that fits no rule is refused with a message that names the value", () => {
  const refused = [
    ["", /not a project version/],
    ["v0.0.1", /"v0\.0\.1"/],
    ["0.0.1-beta", /"0\.0\.1-beta"/],
    ["0.0.1-beta0", /"0\.0\.1-beta0"/],
    ["0.0.1-nightly1", /"0\.0\.1-nightly1"/],
    ["0.0.01", /"0\.0\.01"/],
    ["00.0.1", /"00\.0\.1"/],
    ["0.0.1-beta01", /"0\.0\.1-beta01"/],
    ["+0.0.1", /"\+0\.0\.1"/],
    ["-1.0.0", /"-1\.0\.0"/],
    [" 0.0.1", /" 0\.0\.1"/],
    ["0.0.1 ", /"0\.0\.1 "/],
    ["0.0.1\n", /not a project version/],
    ["0.0", /"0\.0"/],
    ["0.0.1.0", /"0\.0\.1\.0"/],
    ["0.0.1-alpha20", /alpha number 20/],
    ["0.0.1-beta40", /beta number 40/],
    ["0.0.1-rc39", /rc number 39/],
    // 655*100 + 99 = 65599 is above 65535: the last patch with a final stage is 654, and 655 only fits low stages.
    ["0.0.655", /patch number/],
    ["0.0.656-alpha1", /patch number/],
    ["0.65535.0", null],
    ["0.65536.0", /minor number/],
    ["65534.0.0", null],
    ["65535.0.0", /major number/],
    ["99999999.0.0", /too large/],
  ];
  for (const [text, message] of refused) {
    if (message === null) {
      assert.doesNotThrow(() => msixVersion(text), text);
    } else {
      assert.throws(() => msixVersion(text), message, JSON.stringify(text));
    }
  }
  assert.throws(() => msixVersion(undefined), /not a project version/);
  assert.throws(() => msixVersion(null), /not a project version/);
  assert.equal(msixVersion("0.0.654"), "1.0.65499.0");
  assert.equal(msixVersion("0.0.655-alpha1"), "1.0.65501.0");
});

test("every result has four integers, the first not 0, the fourth 0, each at most 65535", () => {
  const samples = ["0.0.1-alpha1", "0.0.1-beta1", "0.0.1-rc1", "0.0.1", "0.65535.654", "65534.0.0", "3.4.5-rc38", "10.20.30-beta39"];
  for (const s of samples) {
    const out = msixVersion(s);
    const parts = out.split(".");
    assert.equal(parts.length, 4, out);
    for (const p of parts) {
      assert.match(p, /^(0|[1-9][0-9]*)$/, out);
      assert.ok(Number(p) <= 65535, out);
    }
    assert.notEqual(Number(parts[0]), 0, out);
    assert.equal(Number(parts[3]), 0, out);
    assert.deepEqual(parseMsixVersion(out).length, 4);
  }
  assert.throws(() => parseMsixVersion("0.0.1.0"), /first part/);
  assert.throws(() => parseMsixVersion("1.0.1.1"), /fourth part/);
  assert.throws(() => parseMsixVersion("1.0.65536.0"), /above 65535/);
  assert.throws(() => parseMsixVersion("1.0.1"), /not an MSIX version/);
});

test("the releases so far map to distinct, strictly rising versions", () => {
  const releases = ["0.0.1-alpha1", "0.0.1-beta1", "0.0.1-beta2"];
  const mapped = releases.map(msixVersion);
  assert.deepEqual(mapped, ["1.0.101.0", "1.0.121.0", "1.0.122.0"]);
  for (let i = 1; i < mapped.length; i++) assert.ok(compareMsixVersions(mapped[i - 1], mapped[i]) < 0);
  assert.equal(new Set(mapped).size, mapped.length);
});

test("parseProjectVersion returns the parts", () => {
  assert.deepEqual(parseProjectVersion("1.2.3-rc4"), { major: 1, minor: 2, patch: 3, kind: "rc", number: 4 });
  assert.deepEqual(parseProjectVersion("0.0.1"), { major: 0, minor: 0, patch: 1, kind: null, number: 0 });
});
