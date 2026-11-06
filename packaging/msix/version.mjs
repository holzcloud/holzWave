// The MSIX (Microsoft Store) version of a holzWave release, as one pure function.
//
// Why a project version cannot be used as it is: an MSIX version has four numbers. The first may not be 0 (holzWave is
// 0.0.x), the fourth belongs to the Store and must stay 0, every number is 0 to 65535, and a pre-release suffix such as
// `-beta2` does not exist. The Store always serves the highest version it has, so a version can never go down once it is
// uploaded: this mapping cannot be changed after the first package is uploaded, and every later version must map higher.
//
// The mapping (chosen by the owner, "scheme A"):
//
//   major.minor.patch[-alphaN | -betaN | -rcN]  ->  (major+1).minor.(patch*100 + stage).0
//
//   stage: alpha N = N (N 1 to 19), beta N = 20 + N (N 1 to 39), rc N = 60 + N (N 1 to 38), a final version = 99
//
//   0.0.1-alpha1 -> 1.0.101.0   0.0.1-beta2 -> 1.0.122.0   0.0.1-rc2 -> 1.0.162.0   0.0.1 -> 1.0.199.0   0.1.0 -> 1.1.99.0
//
// The ranges keep every stage of a release below the next one's (alpha < beta < rc < final) and below the first stage of
// the next patch number. A version that fits no rule is refused with a message; nothing is rounded, clamped or guessed.
// Plain ES module, no dependency.

const VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-(alpha|beta|rc)([1-9][0-9]*))?$/;

// Highest stage number per pre-release kind, and the stage that the kind starts from.
const KINDS = {
  alpha: { base: 0, max: 19 },
  beta: { base: 20, max: 39 },
  rc: { base: 60, max: 38 },
};
const FINAL_STAGE = 99;
const MAX_PART = 65535;

/**
 * Splits a project version into its parts. Strict: the whole text must be `major.minor.patch` with an optional
 * `-alphaN`, `-betaN` or `-rcN` (N from 1, no leading zeros anywhere, no sign, no space, no leading `v`).
 * @param {string} text
 * @returns {{major: number, minor: number, patch: number, kind: ("alpha"|"beta"|"rc"|null), number: number}}
 */
export function parseProjectVersion(text) {
  if (typeof text !== "string") throw new Error(`not a project version: ${String(text)} (a string is needed)`);
  const m = VERSION.exec(text);
  if (!m) {
    throw new Error(
      `not a project version: ${JSON.stringify(text)} (expected major.minor.patch, optionally -alphaN, -betaN or -rcN with N from 1)`,
    );
  }
  // Seven digits are far above every limit below; the cap keeps Number() exact for any input.
  for (const part of [m[1], m[2], m[3], m[5]]) {
    if (part !== undefined && part.length > 7) throw new Error(`a number is too large in project version ${JSON.stringify(text)}`);
  }
  return {
    major: Number(m[1]),
    minor: Number(m[2]),
    patch: Number(m[3]),
    kind: m[4] ?? null,
    number: m[5] === undefined ? 0 : Number(m[5]),
  };
}

/**
 * The MSIX version of a project version, for example "0.0.1-beta2" -> "1.0.122.0".
 * @param {string} projectVersion
 * @returns {string}
 */
export function msixVersion(projectVersion) {
  const v = parseProjectVersion(projectVersion);
  let stage = FINAL_STAGE;
  if (v.kind !== null) {
    const rule = KINDS[v.kind];
    if (v.number > rule.max) {
      throw new Error(
        `the ${v.kind} number ${v.number} of ${JSON.stringify(projectVersion)} is above ${rule.max}: the stage would reach the next stage's range`,
      );
    }
    stage = rule.base + v.number;
  }
  const first = v.major + 1;
  const third = v.patch * 100 + stage;
  if (first > MAX_PART) throw new Error(`the major number of ${JSON.stringify(projectVersion)} is above ${MAX_PART - 1}: the MSIX first part would pass ${MAX_PART}`);
  if (v.minor > MAX_PART) throw new Error(`the minor number of ${JSON.stringify(projectVersion)} is above ${MAX_PART}`);
  if (third > MAX_PART) {
    throw new Error(`the patch number of ${JSON.stringify(projectVersion)} is too large: patch*100 + stage is ${third}, above ${MAX_PART}`);
  }
  return `${first}.${v.minor}.${third}.0`;
}

/**
 * The four numbers of an MSIX version string, checked against the Store's rules (four parts, the first not 0, the fourth 0,
 * each 0 to 65535). Throws on anything else.
 * @param {string} text
 * @returns {[number, number, number, number]}
 */
export function parseMsixVersion(text) {
  const m = /^(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})$/.exec(String(text));
  if (!m) throw new Error(`not an MSIX version: ${JSON.stringify(text)}`);
  const parts = m.slice(1).map(Number);
  if (parts.some((p) => p > MAX_PART)) throw new Error(`an MSIX version part is above ${MAX_PART}: ${text}`);
  if (parts[0] === 0) throw new Error(`the first part of an MSIX version cannot be 0: ${text}`);
  if (parts[3] !== 0) throw new Error(`the fourth part of an MSIX version belongs to the Store and must be 0: ${text}`);
  return /** @type {[number, number, number, number]} */ (parts);
}

/** Compares two MSIX version strings: negative when a < b, 0 when equal, positive when a > b. */
export function compareMsixVersions(a, b) {
  const pa = parseMsixVersion(a);
  const pb = parseMsixVersion(b);
  for (let i = 0; i < 4; i++) {
    if (pa[i] !== pb[i]) return pa[i] - pb[i];
  }
  return 0;
}
