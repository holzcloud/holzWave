// The pure part of the login page (loginstate.ts): the sentences of design 6.28, the wait text, the reading of the server's
// answers, the fragment of a link, the grouping of a secret, and the list of modules the page loads before sign-in.
import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  BUSY,
  FAILED,
  SECRET_RULE,
  UNREACHABLE,
  DEVICE_CODE_ALPHABET,
  groupRecovery,
  groupsOfFour,
  messageFor,
  normaliseDeviceCode,
  parseFragment,
  readPeek,
  readRecovery,
  readSession,
  readTotpSetup,
  waitText,
} from "../../dist/loginstate.js";

const DIST = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "dist");
const T43 = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v";

test("waitText_rounds_up_and_picks_the_unit", () => {
  const table = [
    [1, "1 second"],
    [45, "45 seconds"],
    [40.2, "41 seconds"],
    [60, "1 minute"],
    [61, "2 minutes"],
    [720, "12 minutes"],
    [5400, "90 minutes"],
    [5401, "2 hours"],
    [7200, "2 hours"],
    [7201, "3 hours"],
    [0, "a moment"],
    [-5, "a moment"],
    [Number.NaN, "a moment"],
    [Infinity, "a moment"],
  ];
  for (const [seconds, text] of table) assert.equal(waitText(seconds), text, `${seconds}`);
  assert.equal(waitText("12"), "a moment", "a string is not a number of seconds");
});

test("messageFor_gives_one_sentence_per_class_of_failure", () => {
  assert.equal(messageFor(401), FAILED);
  assert.equal(messageFor(401, { code: "invalid" }), FAILED);
  for (const status of [400, 403, 404, 409, 410, 423, 418]) assert.equal(messageFor(status), FAILED, `${status}`);
  assert.equal(messageFor(429, { code: "throttled", retry_seconds: 720 }), "Too many attempts. Try again in 12 minutes.");
  assert.equal(messageFor(429, { retry_seconds: 40 }), "Too many attempts. Try again in 40 seconds.");
  assert.equal(messageFor(429, { retry_seconds: 61 }), "Too many attempts. Try again in 2 minutes.");
  assert.equal(messageFor(429), "Too many attempts. Try again in a moment.");
  assert.equal(messageFor(429, { retry_seconds: "720" }), "Too many attempts. Try again in a moment.");
  assert.equal(messageFor(503, { code: "busy" }), BUSY);
  assert.equal(messageFor(0), UNREACHABLE);
  assert.equal(messageFor(500), UNREACHABLE);
  assert.equal(messageFor(502), UNREACHABLE);
});

test("messageFor_is_the_same_whatever_detail_was_refused", () => {
  // The unknown account, the wrong password, the wrong code and the wrong passkey are all a 401: the body must not matter.
  const bodies = [undefined, {}, { code: "invalid" }, { code: "unknown_account" }, { code: "wrong_password", field: "password" }, { field: "code" }];
  assert.deepEqual(new Set(bodies.map((b) => messageFor(401, b))), new Set([FAILED]));
});

test("messageFor_reads_nothing_but_the_wait_from_a_body", () => {
  const hostile = { code: "throttled", retry_seconds: 90, message: "<b>account holz is locked</b>", account: "holz", address: "203.0.113.7" };
  const text = messageFor(429, hostile);
  assert.equal(text, "Too many attempts. Try again in 2 minutes.");
  assert.doesNotMatch(text, /holz|203|<b>/);
});

test("parseFragment_accepts_only_one_well_formed_token", () => {
  assert.deepEqual(parseFragment(`#invite=hwi_${T43}`), { kind: "invite", token: `hwi_${T43}` });
  assert.deepEqual(parseFragment(`#setup=hwb_${T43}`), { kind: "setup", token: `hwb_${T43}` });
  const none = [
    "",
    "#",
    `invite=hwi_${T43}`,
    `#invite=hwb_${T43}`,
    `#setup=hwi_${T43}`,
    `#invite=hwi_${T43}x`,
    `#invite=hwi_${T43.slice(1)}`,
    `#invite=hwi_${T43.slice(0, 42)}!`,
    `#invite=hwi_${T43.slice(0, 42)}=`,
    `#invite=hwi_${T43}&setup=hwb_${T43}`,
    `#invite=hwi_${T43}&x=1`,
    "#invite=",
    "#invite",
    `#other=hwi_${T43}`,
    `#Invite=hwi_${T43}`,
    `#invite=%68wi_${T43}`,
  ];
  for (const hash of none) assert.equal(parseFragment(hash), null, JSON.stringify(hash));
  assert.equal(parseFragment(undefined), null);
  assert.equal(parseFragment(null), null);
});

test("parseFragment_reads_a_device_code_in_any_case_with_or_without_the_dash", () => {
  const want = { kind: "device", token: "BCDF-GHJK" };
  for (const hash of ["#device=BCDF-GHJK", "#device=bcdf-ghjk", "#device=BCDFGHJK", "#device=bcdfGHJK", "#device=BcDf-gHjK"]) {
    assert.deepEqual(parseFragment(hash), want, hash);
  }
  const none = [
    "#device=",
    "#device",
    "#device=BCDF-GHJ",
    "#device=BCDF-GHJKL",
    "#device=BCDF--GHJK",
    "#device=BCDF GHJK",
    "#device=BCDF-GHJA", // a vowel is not in the alphabet
    "#device=BCDF-GHJ1",
    "#device=BCDF-GHJK&x=1",
    "#device=BCDF-GHJK&device=BCDF-GHJK",
    "#device=%42CDF-GHJK",
    "#device=BCDF-GHJK ",
    "#Device=BCDF-GHJK",
    `#device=hwi_${T43}`,
    `#invite=BCDF-GHJK`,
  ];
  for (const hash of none) assert.equal(parseFragment(hash), null, JSON.stringify(hash));
});

test("normaliseDeviceCode_is_strict_and_the_alphabet_is_the_one_of_the_record", () => {
  assert.equal(normaliseDeviceCode("bcdfghjk"), "BCDF-GHJK");
  assert.equal(normaliseDeviceCode("BCDF-GHJK"), "BCDF-GHJK");
  for (const bad of ["", " bcdfghjk", "bcdf ghjk", "bcdf-ghjkl", "bcdfghj", "aeiouaei", null, undefined, 12345678, {}]) {
    assert.equal(normaliseDeviceCode(bad), null, String(bad));
  }
  // The alphabet is the one the server makes codes from: twenty consonants, no vowel.
  assert.equal(DEVICE_CODE_ALPHABET.length, 20);
  assert.doesNotMatch(DEVICE_CODE_ALPHABET, /[AEIOUY]/);
  const server = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "server", "src", "device.rs"), "utf8");
  assert.ok(server.includes(`b"${DEVICE_CODE_ALPHABET}"`), "the server makes its user codes from the same letters");
});

test("groupsOfFour_never_changes_the_characters", () => {
  assert.equal(groupsOfFour("JBSWY3DPEHPK3PXP"), "JBSW Y3DP EHPK 3PXP");
  assert.equal(groupsOfFour("JBSWY3DPEH"), "JBSW Y3DP EH");
  assert.equal(groupsOfFour("ABC"), "ABC");
  assert.equal(groupsOfFour("ABCD"), "ABCD");
  assert.equal(groupsOfFour(""), "");
  for (const text of ["", "A", "ABCDE", "JBSWY3DPEHPK3PXP5NMFUWYZ2LQA7TBC", "Q7K2M9XDT4"]) {
    assert.equal(groupsOfFour(text).replaceAll(" ", ""), text);
  }
});

test("groupRecovery_joins_groups_of_four_without_double_separators", () => {
  assert.equal(groupRecovery("Q7K2-M9XD-T4WP-H8NB-C3FZ-6VRA"), "Q7K2-M9XD-T4WP-H8NB-C3FZ-6VRA");
  assert.equal(groupRecovery("Q7K2M9XDT4WP"), "Q7K2-M9XD-T4WP");
  assert.equal(groupRecovery("Q7K2--M9XD-"), "Q7K2-M9XD");
  assert.equal(groupRecovery("Q7K2 M9XD"), "Q7K2-M9XD");
  assert.equal(groupRecovery("Q7K2M"), "Q7K2-M");
  assert.equal(groupRecovery(""), "");
  assert.doesNotMatch(groupRecovery("AB--CD--EF--"), /--|^-|-$/);
});

test("readSession_keeps_only_what_the_page_uses", () => {
  const read = readSession({
    state: "anonymous",
    host: "holz.example.net",
    bootstrap: true,
    methods: {
      passkey: true,
      password: true,
      oidc: [
        { id: "corp", name: "Company SSO" },
        { id: "../x", name: "bad id" },
        { id: "a b", name: "bad id" },
        { id: "ok_1-2", name: "" },
        { id: "no-name" },
        "text",
        { id: "last", name: "Last" },
      ],
    },
    extra: "ignored",
  });
  assert.deepEqual(read, {
    state: "anonymous",
    host: "holz.example.net",
    bootstrap: true,
    passkey: true,
    password: true,
    providers: [
      { id: "corp", name: "Company SSO" },
      { id: "last", name: "Last" },
    ],
  });
  assert.equal(readSession({ state: "signed_in", host: "h", csrf: "t" }).state, "signed_in");
  assert.equal(readSession({ state: "locked", host: "h" }).state, "locked");
  for (const raw of [null, undefined, "x", 5, [], {}, { state: "other" }, { state: 1 }]) assert.equal(readSession(raw), null);
  // A method that is not exactly true is off; a missing methods member is no method.
  const off = readSession({ state: "anonymous", host: "h", methods: { passkey: "yes", password: 1 } });
  assert.equal(off.passkey, false);
  assert.equal(off.password, false);
  assert.equal(readSession({ state: "anonymous", host: "h" }).password, false);
  // More than 16 providers: the rest is dropped.
  const many = Array.from({ length: 40 }, (_, i) => ({ id: `p${i}`, name: `P${i}` }));
  assert.equal(readSession({ state: "anonymous", host: "h", methods: { oidc: many } }).providers.length, 16);
});

test("readers_refuse_what_is_not_an_answer", () => {
  assert.deepEqual(readPeek({ role: "admin" }), { role: "administrator" });
  assert.deepEqual(readPeek({ role: "member", existing: true, username: "ben" }), { role: "member", username: "ben" });
  assert.deepEqual(readPeek({ role: "member", username: "" }), { role: "member" });
  for (const raw of [null, {}, { role: "root" }, { role: 1 }, "member"]) assert.equal(readPeek(raw), null);

  const good = { secret: "JBSWY3DPEHPK3PXP", otpauth: "otpauth://totp/holzWave:holz?secret=JBSWY3DPEHPK3PXP&issuer=holzWave" };
  assert.deepEqual(readTotpSetup(good), good);
  for (const bad of [
    { ...good, secret: "short" },
    { ...good, secret: "jbswy3dpehpk3pxp!" },
    { ...good, secret: "JBSWY3DPEHPK3PX1" },
    { ...good, otpauth: "https://example.net/" },
    { ...good, otpauth: "otpauth://totp/a b" },
    { ...good, otpauth: `otpauth://${"a".repeat(1100)}` },
    { secret: good.secret },
    null,
  ]) {
    assert.equal(readTotpSetup(bad), null, JSON.stringify(bad));
  }

  assert.equal(readRecovery({ recovery_code: "Q7K2-M9XD-T4WP-H8NB-C3FZ-6VRA" }), "Q7K2-M9XD-T4WP-H8NB-C3FZ-6VRA");
  for (const bad of [null, {}, { recovery_code: "short" }, { recovery_code: "has space 1234" }, { recovery_code: 5 }, { recovery_code: "<b>x</b>-ABCD-EFGH" }]) {
    assert.equal(readRecovery(bad), null, JSON.stringify(bad));
  }
});

test("the_rule_for_a_refused_password_is_the_sentence_of_the_design", () => {
  assert.equal(SECRET_RULE, "Use at least 12 characters. Not your username, not one repeated character.");
});

/** The modules `dist/login.js` loads, by import: the pre-authentication list the edge plan serves. */
function closure(entry) {
  const seen = new Set();
  const walk = (name) => {
    if (seen.has(name)) return;
    seen.add(name);
    const text = readFileSync(join(DIST, `${name}.js`), "utf8");
    assert.doesNotMatch(text, /\bimport\s*\(/, `${name}.js loads a module dynamically`);
    for (const m of text.matchAll(/(?:^|\n)\s*(?:import|export)\s[^;]*?from\s*["']\.\/([a-z0-9-]+)\.js["']/g)) walk(m[1]);
    for (const m of text.matchAll(/(?:^|\n)\s*import\s*["']\.\/([a-z0-9-]+)\.js["']/g)) walk(m[1]);
  };
  walk(entry);
  return [...seen].sort();
}

test("the_login_page_loads_exactly_the_modules_the_edge_serves_before_sign_in", () => {
  assert.deepEqual(closure("login"), ["bridge", "login", "loginpasskey", "loginstate", "secretsteps", "webhttp", "webtransport"]);
});

test("the_login_page_takes_nothing_but_http_from_the_bridge_and_uses_no_network_api", () => {
  const source = readFileSync(join(DIST, "login.js"), "utf8");
  const fromBridge = [...source.matchAll(/import\s*\{([^}]*)\}\s*from\s*["']\.\/bridge\.js["']/g)].flatMap((m) => m[1].split(",").map((s) => s.trim()));
  assert.deepEqual(fromBridge.sort(), ["HttpError", "http"]);
  for (const name of ["login", "loginstate", "loginpasskey", "secretsteps"]) {
    const text = readFileSync(join(DIST, `${name}.js`), "utf8");
    assert.doesNotMatch(text, /\bfetch\s*\(|WebSocket|XMLHttpRequest|sendBeacon|innerHTML|outerHTML|insertAdjacentHTML|document\.write|localStorage|sessionStorage|document\.cookie|console\./, name);
    assert.doesNotMatch(text, /https?:\/\//, `${name} has an absolute address`);
  }
});

test("the_entry_document_has_no_inline_script_or_style", () => {
  const html = readFileSync(join(DIST, "login.html"), "utf8");
  assert.deepEqual([...html.matchAll(/<script\b[^>]*>/g)].map((m) => m[0]), ['<script type="module" src="/login.js">']);
  assert.doesNotMatch(html, /<style\b|\sstyle=|\son[a-z]+=|javascript:/i);
  // The server answers this document at `/auth/oidc/callback` too, so the files are named from the root: a relative name would
  // be looked for under `/auth/oidc/` and the page would stay blank after a single sign-on.
  assert.match(html, /href="\/app\.css"/);
  assert.match(html, /href="\/login\.css"/);
  assert.doesNotMatch(html, /(?:href|src)="\./);
});

test("the_application_document_names_its_files_from_the_root_like_the_login_document", () => {
  const html = readFileSync(join(DIST, "index.html"), "utf8");
  assert.deepEqual([...html.matchAll(/<script\b[^>]*>/g)].map((m) => m[0]), ['<script type="module" src="/main.js">']);
  assert.doesNotMatch(html, /<style\b|\sstyle=|\son[a-z]+=|javascript:/i);
  // The server answers this document at `/` and at `/index.html`; a name relative to the document would be wrong for the next route
  // that serves it deeper, the blank page that the login document had under `/auth/oidc/callback`.
  assert.match(html, /href="\/app\.css"/);
  assert.doesNotMatch(html, /(?:href|src)="\./);
  // No file of the document is named by a path relative to it, and every name that starts at the root is a file of the build.
  for (const [, name] of html.matchAll(/(?:href|src)="([^"]+)"/g)) {
    assert.ok(name.startsWith("/") && !name.startsWith("//"), `${name} starts at the root`);
    assert.ok(existsSync(join(DIST, name.slice(1))), `${name} is a file of the build`);
  }
});
