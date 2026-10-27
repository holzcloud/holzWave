// WebAuthn conversions of the login page (loginpasskey.ts): base64url, the options the server sends, and the credential
// the browser returns. Node has ArrayBuffer, btoa and atob, which is all these functions use.
import test from "node:test";
import assert from "node:assert/strict";
import {
  b64urlDecode,
  b64urlEncode,
  decodeCreationOptions,
  decodeRequestOptions,
  encodeAssertion,
  encodeAttestation,
} from "../../dist/loginpasskey.js";

const buffer = (...bytes) => new Uint8Array(bytes).buffer;
const text = (s) => Buffer.from(s).toString("base64url");
const bytesOf = (b) => Array.from(new Uint8Array(b));

test("base64url_round_trips_and_matches_the_standard_alphabet", () => {
  for (const length of [0, 1, 2, 3, 4, 5, 32, 33, 255]) {
    const bytes = Uint8Array.from({ length }, (_, i) => (i * 37 + 11) & 0xff);
    const encoded = b64urlEncode(bytes);
    assert.equal(encoded, Buffer.from(bytes).toString("base64url"), `${length} bytes`);
    assert.deepEqual(Array.from(b64urlDecode(encoded)), Array.from(bytes), `${length} bytes back`);
    assert.doesNotMatch(encoded, /[+/=]/);
  }
  assert.equal(b64urlEncode(buffer(251, 255, 254)), "-__-", "the url alphabet, not + and /");
  assert.equal(b64urlDecode("").length, 0, "the empty string is zero bytes");
  assert.equal(b64urlEncode(new DataView(buffer(1, 2, 3))), "AQID", "a view is encoded over its own bytes");
});

test("base64url_refuses_padding_other_alphabets_and_impossible_lengths", () => {
  for (const bad of ["AQID=", "AQ==", "AQI=", "+/+/", "AQ+D", "AQ/D", "A", "AQIDB", "AQ ID", "AQ\nID", "é", "AQID\u0000"]) {
    assert.throws(() => b64urlDecode(bad), TypeError, JSON.stringify(bad));
  }
  assert.throws(() => b64urlDecode(5), TypeError);
  assert.throws(() => b64urlEncode("text"), TypeError);
  assert.throws(() => b64urlEncode(null), TypeError);
});

test("decodeRequestOptions_converts_the_binary_members_and_keeps_the_rest", () => {
  const options = decodeRequestOptions({
    challenge: text("challenge"),
    rpId: "holz.example.net",
    timeout: 60000,
    userVerification: "preferred",
    allowCredentials: [
      { id: text("one"), type: "public-key", transports: ["internal"] },
      { id: text("two"), type: "public-key" },
    ],
    extensions: {
      prf: { eval: { first: text("salt-1"), second: text("salt-2") }, evalByCredential: { [text("one")]: { first: text("salt-3") } } },
      other: "kept",
    },
  });
  assert.ok(options.challenge instanceof ArrayBuffer);
  assert.equal(Buffer.from(options.challenge).toString(), "challenge");
  assert.equal(options.rpId, "holz.example.net");
  assert.equal(options.timeout, 60000);
  assert.equal(options.allowCredentials.length, 2);
  assert.ok(options.allowCredentials[0].id instanceof ArrayBuffer);
  assert.equal(Buffer.from(options.allowCredentials[1].id).toString(), "two");
  assert.deepEqual(options.allowCredentials[0].transports, ["internal"]);
  assert.equal(Buffer.from(options.extensions.prf.eval.first).toString(), "salt-1");
  assert.equal(Buffer.from(options.extensions.prf.eval.second).toString(), "salt-2");
  assert.equal(Buffer.from(options.extensions.prf.evalByCredential[text("one")].first).toString(), "salt-3");
  assert.equal(options.extensions.other, "kept");
  // No allow list and no extensions is fine.
  const bare = decodeRequestOptions({ challenge: text("c") });
  assert.equal("allowCredentials" in bare, false);
  assert.equal("extensions" in bare, false);
});

test("decodeRequestOptions_throws_a_TypeError_for_a_member_of_the_wrong_type", () => {
  const good = { challenge: text("c") };
  const bad = [
    null,
    [],
    "options",
    {},
    { challenge: 5 },
    { challenge: "not base64url!" },
    { ...good, allowCredentials: "none" },
    { ...good, allowCredentials: [{ id: 5 }] },
    { ...good, allowCredentials: [{ id: "AQ==" }] },
    { ...good, allowCredentials: ["x"] },
    { ...good, extensions: "prf" },
    { ...good, extensions: { prf: { eval: { first: 5 } } } },
    { ...good, extensions: { prf: { eval: {} } } },
    { ...good, extensions: { prf: { eval: { first: text("a"), second: 7 } } } },
    { ...good, extensions: { prf: { evalByCredential: { x: { first: "+" } } } } },
  ];
  for (const options of bad) assert.throws(() => decodeRequestOptions(options), TypeError, JSON.stringify(options));
});

test("decodeCreationOptions_converts_challenge_user_and_excluded_credentials", () => {
  const options = decodeCreationOptions({
    challenge: text("challenge"),
    rp: { name: "holzWave", id: "holz.example.net" },
    user: { id: text("user-1"), name: "anna", displayName: "Anna" },
    pubKeyCredParams: [{ type: "public-key", alg: -7 }],
    excludeCredentials: [{ id: text("old"), type: "public-key" }],
    extensions: { prf: { eval: { first: text("salt") } } },
  });
  assert.equal(Buffer.from(options.challenge).toString(), "challenge");
  assert.ok(options.user.id instanceof ArrayBuffer);
  assert.equal(Buffer.from(options.user.id).toString(), "user-1");
  assert.equal(options.user.name, "anna");
  assert.deepEqual(options.rp, { name: "holzWave", id: "holz.example.net" });
  assert.deepEqual(options.pubKeyCredParams, [{ type: "public-key", alg: -7 }]);
  assert.equal(Buffer.from(options.excludeCredentials[0].id).toString(), "old");
  assert.equal(Buffer.from(options.extensions.prf.eval.first).toString(), "salt");
  assert.throws(() => decodeCreationOptions({ challenge: text("c") }), TypeError, "no user");
  assert.throws(() => decodeCreationOptions({ challenge: text("c"), user: { id: 5 } }), TypeError);
  assert.throws(() => decodeCreationOptions({ challenge: text("c"), user: { id: text("u") }, excludeCredentials: [{}] }), TypeError);
});

const assertion = (over = {}, extensionResults = {}) => ({
  id: "Y3JlZC1pZA",
  rawId: buffer(99, 114, 101, 100, 45, 105, 100),
  type: "public-key",
  getClientExtensionResults: () => extensionResults,
  response: {
    authenticatorData: buffer(1, 2, 3),
    clientDataJSON: new TextEncoder().encode('{"type":"webauthn.get"}').buffer,
    signature: buffer(4, 5, 6),
    userHandle: null,
    ...over,
  },
});

test("encodeAssertion_adds_prf_only_when_the_browser_returned_a_result", () => {
  const withoutPrf = encodeAssertion(assertion());
  assert.deepEqual(withoutPrf, {
    credential: {
      id: "Y3JlZC1pZA",
      rawId: text("cred-id"),
      type: "public-key",
      response: {
        authenticatorData: "AQID",
        clientDataJSON: text('{"type":"webauthn.get"}'),
        signature: "BAUG",
        userHandle: null,
      },
    },
  });
  assert.equal("prf" in withoutPrf, false);

  const first = Uint8Array.from({ length: 32 }, (_, i) => i).buffer;
  const withPrf = encodeAssertion(assertion({}, { prf: { results: { first } } }));
  assert.equal(withPrf.prf, Buffer.from(new Uint8Array(first)).toString("base64url"));
  assert.equal(withPrf.credential.response.userHandle, null);

  // A user handle is encoded; an extension result without `first`, with an empty one, or of the wrong type adds nothing.
  assert.equal(encodeAssertion(assertion({ userHandle: buffer(9, 9) })).credential.response.userHandle, "CQk");
  for (const results of [{}, { prf: {} }, { prf: { enabled: true } }, { prf: { results: {} } }, { prf: { results: { first: null } } }, { prf: { results: { first: "text" } } }, { prf: { results: { first: new ArrayBuffer(0) } } }, { prf: { results: { second: first } } }]) {
    assert.equal("prf" in encodeAssertion(assertion({}, results)), false, JSON.stringify(results));
  }
  // A browser without the method for extension results has none either.
  const plain = assertion();
  delete plain.getClientExtensionResults;
  assert.equal("prf" in encodeAssertion(plain), false);
  // A typed array result counts too.
  assert.ok(encodeAssertion(assertion({}, { prf: { results: { first: new Uint8Array([1, 2, 3]) } } })).prf === "AQID");
});

test("encodeAssertion_throws_a_TypeError_for_a_credential_that_is_not_one", () => {
  for (const bad of [null, undefined, "credential", {}, { id: "x", type: "public-key" }, assertion({ signature: "text" }), assertion({ authenticatorData: null }), { ...assertion(), id: "" }, { ...assertion(), rawId: "x" }]) {
    assert.throws(() => encodeAssertion(bad), TypeError);
  }
});

test("encodeAttestation_encodes_a_registration_and_adds_prf_only_with_a_result", () => {
  const credential = (extensionResults = {}, withTransports = true) => ({
    id: "bmV3",
    rawId: buffer(110, 101, 119),
    type: "public-key",
    getClientExtensionResults: () => extensionResults,
    response: {
      attestationObject: buffer(7, 8, 9),
      clientDataJSON: new TextEncoder().encode("{}").buffer,
      ...(withTransports ? { getTransports: () => ["internal", "hybrid"] } : {}),
    },
  });
  const plain = encodeAttestation(credential());
  assert.deepEqual(plain, {
    credential: {
      id: "bmV3",
      rawId: "bmV3",
      type: "public-key",
      response: { attestationObject: "BwgJ", clientDataJSON: "e30", transports: ["internal", "hybrid"] },
    },
  });
  assert.equal("prf" in plain, false);
  assert.equal("transports" in encodeAttestation(credential({}, false)).credential.response, false);
  assert.equal(encodeAttestation(credential({ prf: { enabled: true, results: { first: buffer(1, 2, 3) } } })).prf, "AQID");
  assert.equal("prf" in encodeAttestation(credential({ prf: { enabled: true } })), false, "enabled alone is no result");
  assert.throws(() => encodeAttestation({ ...credential(), response: {} }), TypeError);
  assert.throws(() => encodeAttestation(null), TypeError);
});
