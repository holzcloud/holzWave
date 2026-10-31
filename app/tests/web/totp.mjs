// The authenticator app of the tests: the six-digit codes of RFC 6238 (HMAC-SHA1, 30 seconds), written with node:crypto
// only. It is the test's own implementation on purpose; the server's is the one under test.
import { createHmac } from "node:crypto";

export const STEP_SECONDS = 30;

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/** The bytes of a base32 text. Upper or lower case, spaces and the groups of four the page shows are accepted. */
export function base32decode(text) {
  const clean = String(text).replace(/[\s=-]+/g, "").toUpperCase();
  let bits = 0;
  let value = 0;
  const out = [];
  for (const ch of clean) {
    const at = ALPHABET.indexOf(ch);
    if (at < 0) throw new Error("the secret is not base32");
    value = (value << 5) | at;
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      out.push((value >>> bits) & 0xff);
    }
  }
  return Buffer.from(out);
}

/** The step (30-second counter) a time in unix seconds falls in. */
export const stepOf = (unixSeconds) => Math.floor(unixSeconds / STEP_SECONDS);

/** The six-digit code of a step. */
export function codeForStep(secretBase32, step) {
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(step));
  const mac = createHmac("sha1", base32decode(secretBase32)).update(counter).digest();
  const offset = mac[mac.length - 1] & 0x0f;
  const number = ((mac[offset] & 0x7f) << 24) | (mac[offset + 1] << 16) | (mac[offset + 2] << 8) | mac[offset + 3];
  return String(number % 1_000_000).padStart(6, "0");
}

/** The code an authenticator app shows at `unixSeconds`. */
export const totp = (secretBase32, unixSeconds) => codeForStep(secretBase32, stepOf(unixSeconds));

/** The `secret` parameter of an `otpauth://` link. */
export function secretOfLink(link) {
  const url = new URL(link);
  if (url.protocol !== "otpauth:") throw new Error("not an otpauth link");
  const secret = url.searchParams.get("secret");
  if (secret === null || secret === "") throw new Error("the link has no secret");
  return secret;
}

/**
 * The codes of one account over time. The server accepts a code once and only for a step later than the last one it took
 * (the window is one step either side), so a person who signs in more than once in half a minute needs the next step's code.
 * `next()` gives the earliest code the server can still accept, waiting (bounded) for the clock when none is left.
 */
export class Authenticator {
  constructor(secret, lastStep = 0) {
    this.secret = secret;
    this.lastStep = lastStep;
  }

  /** Mark a step as used (the account's own enrolment, or a sign-in made by hand). */
  used(step) {
    this.lastStep = Math.max(this.lastStep, step);
  }

  /** The code for a sign-in now. The page-made first administrator spent the current step when it enrolled. */
  async next(now = () => Math.floor(Date.now() / 1000), limitMs = 40_000) {
    const t0 = Date.now();
    for (;;) {
      const current = stepOf(now());
      const want = Math.max(this.lastStep + 1, current - 1);
      // One step of the future is inside the server's window; a step further is not yet.
      if (want <= current + 1) {
        // Too close to the end of the step: the code could arrive after the window moved on.
        if (want >= current + 1 || now() % STEP_SECONDS < STEP_SECONDS - 2) {
          this.lastStep = want;
          return codeForStep(this.secret, want);
        }
      }
      if (Date.now() - t0 > limitMs) throw new Error(`timeout: no authenticator code was usable (last step ${this.lastStep}, now ${current})`);
      await new Promise((done) => setTimeout(done, 250));
    }
  }
}
