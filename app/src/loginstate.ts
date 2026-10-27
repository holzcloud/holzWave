// The pure part of the login page (docs/design.md 6.28): the sentences, the reading of what the server answers, the wait
// text, the fragment of an invitation, and the grouping of a secret. No DOM and no network here, so node tests them on
// the compiled file.
//
// The server answers with a status and a code and never with a sentence; the page chooses every word from this file, and
// the words are the ones of the table at the end of design 6.28. A failed sign-in has one sentence whatever went wrong,
// so nothing here depends on which detail was refused.

/** Every failed sign-in: an unknown account, a wrong password, a wrong code and a wrong passkey say the same. */
export const FAILED = "Sign-in failed. Check your details or ask an admin.";
/** The server is there and says it is busy. */
export const BUSY = "The server is busy. Try again in a moment.";
/** No answer arrived, or the server failed. */
export const UNREACHABLE = "Could not reach the server. Try again.";
/** The person was sent here by the page that ended their session. */
export const ENDED = "Your session ended. Sign in again.";
/** An invitation that is unknown, expired or used. */
export const INVITE_GONE = "This link has expired or was used. Ask an admin for a new one.";
/** A setup token that is unknown, expired or used: there is no admin to ask. */
export const SETUP_GONE = "This token has expired or was used. Ask the person who runs this server for a new one.";
/** The username is in use (409 on `username`). */
export const USERNAME_TAKEN = "That username is taken. Choose another.";
/** The server refused a new password (400 on `password`). */
export const SECRET_RULE = "Use at least 12 characters. Not your username, not one repeated character.";
/** A wrong code in the authenticator step of a set-up. */
export const CODE_MISMATCH = "That code did not match. Try again.";
/** A passkey that could not be saved. */
export const PASSKEY_NOT_SAVED = "That passkey was not saved. Try again.";
/** Under the disabled button of the recovery step. */
export const TICK = "Tick the box to continue.";
/** After `Copy`, in the line below the box. */
export const COPIED = "Copied.";

/** How long a person waits, in words: seconds under a minute, minutes up to 90 minutes, then hours; always rounded up. */
export function waitText(seconds: number): string {
  if (typeof seconds !== "number" || !Number.isFinite(seconds) || seconds <= 0) return "a moment";
  if (seconds < 60) {
    const s = Math.ceil(seconds);
    return s === 1 ? "1 second" : `${s} seconds`;
  }
  if (seconds <= 5400) {
    const m = Math.ceil(seconds / 60);
    return m === 1 ? "1 minute" : `${m} minutes`;
  }
  const h = Math.ceil(seconds / 3600);
  return h === 1 ? "1 hour" : `${h} hours`;
}

/** The sentence for a failed request. `status` is 0 when nothing arrived. Only `retry_seconds` of the body is read. */
export function messageFor(status: number, body?: Record<string, unknown>): string {
  if (status === 429) {
    const seconds = body?.retry_seconds;
    return `Too many attempts. Try again in ${waitText(typeof seconds === "number" ? seconds : Number.NaN)}.`;
  }
  if (status === 503) return BUSY;
  if (status === 0 || status >= 500) return UNREACHABLE;
  return FAILED;
}

/** One single sign-on provider the server offers. */
export interface Provider {
  id: string;
  name: string;
}

/** What `GET /api/session` says, after checking. */
export interface SessionView {
  state: "anonymous" | "signed_in" | "locked";
  host: string;
  bootstrap: boolean;
  passkey: boolean;
  password: boolean;
  providers: Provider[];
}

const MAX_HOST = 253;
const MAX_PROVIDERS = 16;
const MAX_NAME = 120;
/** A provider id goes into a path: letters, digits, dash and underscore only. */
const PROVIDER_ID = /^[A-Za-z0-9_-]{1,64}$/;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** The answer of `GET /api/session` as the page uses it, or null when it is not an answer. */
export function readSession(raw: unknown): SessionView | null {
  if (!isObject(raw)) return null;
  const state = raw.state;
  if (state !== "anonymous" && state !== "signed_in" && state !== "locked") return null;
  const host = typeof raw.host === "string" ? raw.host.slice(0, MAX_HOST) : "";
  const methods = isObject(raw.methods) ? raw.methods : {};
  const providers: Provider[] = [];
  if (Array.isArray(methods.oidc)) {
    for (const p of methods.oidc) {
      if (providers.length >= MAX_PROVIDERS) break;
      if (!isObject(p) || typeof p.id !== "string" || typeof p.name !== "string") continue;
      if (!PROVIDER_ID.test(p.id) || p.name === "" || p.name.length > MAX_NAME) continue;
      providers.push({ id: p.id, name: p.name });
    }
  }
  return {
    state,
    host,
    bootstrap: raw.bootstrap === true,
    passkey: methods.passkey === true,
    password: methods.password === true,
    providers,
  };
}

/** Which kind of link the address fragment carried. */
export type FragmentKind = "invite" | "setup";

const TOKENS: Record<FragmentKind, RegExp> = {
  invite: /^hwi_[A-Za-z0-9_-]{43}$/,
  setup: /^hwb_[A-Za-z0-9_-]{43}$/,
};

/** Read `#invite=hwi_...` or `#setup=hwb_...`; anything else (another key, a second parameter, a token of the wrong shape) is null. */
export function parseFragment(hash: string): { kind: FragmentKind; token: string } | null {
  if (typeof hash !== "string" || hash[0] !== "#") return null;
  const body = hash.slice(1);
  if (body.includes("&")) return null;
  const eq = body.indexOf("=");
  if (eq < 0) return null;
  const key = body.slice(0, eq);
  const value = body.slice(eq + 1);
  if (key !== "invite" && key !== "setup") return null;
  return TOKENS[key].test(value) ? { kind: key, token: value } : null;
}

/** The role a link grants, as the title row shows it. */
export type Role = "administrator" | "member";

/** The answer of the `peek` step: the role, and the username when the administrator chose it. */
export function readPeek(raw: unknown): { role: Role; username?: string } | null {
  if (!isObject(raw)) return null;
  const role = raw.role === "admin" || raw.role === "administrator" ? "administrator" : raw.role === "member" ? "member" : null;
  if (role === null) return null;
  const username = raw.username;
  if (typeof username === "string" && username !== "" && username.length <= 128) return { role, username };
  return { role };
}

const SECRET = /^[A-Za-z2-7]{16,128}$/;

/** The answer of the first step of a password set-up: the key (base32) and the `otpauth://` link, or null. */
export function readTotpSetup(raw: unknown): { secret: string; otpauth: string } | null {
  if (!isObject(raw) || typeof raw.secret !== "string" || typeof raw.otpauth !== "string") return null;
  if (!SECRET.test(raw.secret)) return null;
  // eslint-disable-next-line no-control-regex
  if (!raw.otpauth.startsWith("otpauth://") || raw.otpauth.length > 1024 || /[\u0000- \u007f]/.test(raw.otpauth)) return null;
  return { secret: raw.secret, otpauth: raw.otpauth };
}

const RECOVERY = /^[A-Za-z0-9-]{8,128}$/;

/** The recovery code of the last answer of a set-up, or null. */
export function readRecovery(raw: unknown): string | null {
  if (!isObject(raw) || typeof raw.recovery_code !== "string") return null;
  return RECOVERY.test(raw.recovery_code) ? raw.recovery_code : null;
}

/** `JBSWY3DPEHPK3PXP` as `JBSW Y3DP EHPK 3PXP`; a short last group keeps the rest. */
export function groupsOfFour(text: string): string {
  const groups: string[] = [];
  for (let i = 0; i < text.length; i += 4) groups.push(text.slice(i, i + 4));
  return groups.join(" ");
}

/** A recovery code as groups of four joined by hyphens, whatever separators it came with, so never a double hyphen. */
export function groupRecovery(code: string): string {
  const plain = code.replace(/[^A-Za-z0-9]/g, "");
  const groups: string[] = [];
  for (let i = 0; i < plain.length; i += 4) groups.push(plain.slice(i, i + 4));
  return groups.join("-");
}
