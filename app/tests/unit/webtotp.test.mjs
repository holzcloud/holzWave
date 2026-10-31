// The TOTP helper of the real-server browser tests, against the published vectors of RFC 6238 (appendix B, SHA-1) and the
// RFC 4226 HOTP values, so that a wrong helper cannot make a wrong test look right.
import assert from "node:assert/strict";
import test from "node:test";
import { Authenticator, base32decode, codeForStep, secretOfLink, stepOf, totp } from "../web/totp.mjs";

// The ASCII secret `12345678901234567890` in base32.
const SECRET = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

test("the base32 decoder reads the secret of the RFC and tolerates case, spaces and groups", () => {
  assert.equal(base32decode(SECRET).toString("ascii"), "12345678901234567890");
  assert.equal(base32decode(SECRET.toLowerCase()).toString("ascii"), "12345678901234567890");
  assert.equal(base32decode("GEZD GNBV GY3T QOJQ GEZD GNBV GY3T QOJQ").toString("ascii"), "12345678901234567890");
  assert.equal(base32decode("GEZD-GNBV-GY3T-QOJQ-GEZD-GNBV-GY3T-QOJQ").toString("ascii"), "12345678901234567890");
  assert.throws(() => base32decode("not base 32 !"), /base32/);
});

test("the six low digits of the published 8-digit vectors of RFC 6238 (SHA-1)", () => {
  const vectors = [
    [59, "287082"],
    [1111111109, "081804"],
    [1111111111, "050471"],
    [1234567890, "005924"],
    [2000000000, "279037"],
    [20000000000, "353130"],
  ];
  for (const [time, code] of vectors) assert.equal(totp(SECRET, time), code, `time ${time}`);
});

test("the HOTP values of RFC 4226 appendix D for counters 0 to 9", () => {
  const expected = ["755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583", "399871", "520489"];
  expected.forEach((code, counter) => assert.equal(codeForStep(SECRET, counter), code, `counter ${counter}`));
});

test("a code changes with the step and not within it", () => {
  assert.equal(stepOf(59), 1);
  assert.equal(stepOf(60), 2);
  assert.equal(totp(SECRET, 60), totp(SECRET, 89));
  assert.notEqual(totp(SECRET, 59), totp(SECRET, 60));
});

test("the secret parameter of an otpauth link", () => {
  assert.equal(secretOfLink(`otpauth://totp/holzWave:anna?secret=${SECRET}&issuer=holzWave&digits=6&period=30`), SECRET);
  assert.throws(() => secretOfLink("https://example.org/?secret=ABC"), /otpauth/);
  assert.throws(() => secretOfLink("otpauth://totp/x?issuer=y"), /no secret/);
});

test("an authenticator never gives a step twice and waits for the clock when no step is left", async () => {
  let clock = 1_000_000_000 - (1_000_000_000 % 30) + 10; // ten seconds into a step
  const now = () => clock;
  const account = new Authenticator(SECRET, stepOf(clock) - 1);
  const first = await account.next(now);
  assert.equal(first, codeForStep(SECRET, stepOf(clock)), "the current step");
  const second = await account.next(now);
  assert.equal(second, codeForStep(SECRET, stepOf(clock) + 1), "then the next one, inside the server's window");
  const pending = account.next(now, 2_000);
  await assert.rejects(pending, /timeout: no authenticator code/);
  clock += 30;
  assert.equal(await account.next(now), codeForStep(SECRET, stepOf(clock) + 1));
});
