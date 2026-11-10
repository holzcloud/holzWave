// The pure part of the device features of the web page (docs/design.md 6.35): every word of the page that approves a device,
// of the Devices group and of the Device access group; the strict readers of what the server answers (the lookup of a code, the
// two lists of devices and the state of the two switches); the time words; and the sentence for each refusal. No DOM and no
// network here, so node tests it on the compiled file, and a test compares the words with the tables of the record.
//
// The server sends data and codes, never a sentence, and none of it is trusted: a string is a string of a bounded length with no
// control or direction character in it, a list has a bound, a platform comes from a closed list, and a member the page does not
// use is not carried on. Everything the page shows from an answer is put in with `textContent`.
import { agoText, CHANGE_FAILED, wholeId } from "./accountstate.js";
import { messageFor } from "./loginstate.js";

export { DEVICE_CODE_ALPHABET, normaliseDeviceCode } from "./loginstate.js";

// ---- the words of the page `Authorize device` -----------------------------------------------------------------------------

export const AUTHORIZE_TITLE = "Authorize device";
/** The palette entry that asks for a typed code. */
export const AUTHORIZE_ENTRY = "Authorize device…";
export const ROW_DEVICE = "Device";
export const ROW_PLATFORM = "Platform";
export const ROW_ADDRESS = "Address";
export const ROW_REQUESTED = "Requested";
/** The label of the code to compare with the app, and of the field of the typed code. */
export const CODE_LABEL = "Code";
export const APPROVE_SENTENCE = "Approve only if you started this on your own device.";
export const APPROVE = "Approve";
export const DENY = "Deny";
export const LOOK_UP = "Look up";
export const CANCEL = "Cancel";
export const CLOSE = "Close";
export const APPROVED = "Device approved.";
export const DENIED = "Request denied.";
/** A code that is unknown, used, expired or malformed, and a feature that is off: one sentence, so a probe learns nothing. */
export const NOT_VALID = "That code is not valid or has expired.";

// ---- the words of the lists -----------------------------------------------------------------------------------------------

export const DEVICES_LEGEND = "Devices";
export const NO_DEVICES = "No devices.";
export const REVOKE = "Revoke";
export const REVOKE_ARMED = "Click again to revoke";
/** The heads of the administrator's list; the person's list has no heads. */
export const LIST_HEADS = ["Name", "Owner", "Platform", "Last seen"] as const;
export const NEVER = "never";

// ---- the words of the group `Device access` -------------------------------------------------------------------------------

export const ACCESS_LEGEND = "Device access";
export const SWITCH_SYNC = "Sync target";
export const SWITCH_REMOTE = "Remote sessions";
export const SWITCH_ON = "ON";
export const SWITCH_OFF = "OFF";
export const LOCKED_LINE = "Turned off in the server configuration.";
export const ACCESS_HELP = "Off by default. Turning one off ends the matching connections; devices stay signed in.";

/** The two switches in the order the group draws them, with the word of each. */
export const SWITCHES = [
  { key: "sync", label: SWITCH_SYNC },
  { key: "remote", label: SWITCH_REMOTE },
] as const;

export type SwitchKey = (typeof SWITCHES)[number]["key"];

// ---- reading what the server answers --------------------------------------------------------------------------------------

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** The longest name and address the page shows, in characters (the server keeps no more). */
export const MAX_NAME = 64;
const MAX_ADDR = 64;
const MAX_ID = 128;
/** The most devices of one person the page takes, and the most of everybody (the server's own cap of a list). */
export const MAX_PERSON_DEVICES = 64;
export const MAX_ALL_DEVICES = 1000;

// Control characters, line and paragraph separators and the characters that change the direction of text: none of them has a place
// in a name that a person reads to decide something.
// Written as a string: the compiler turns a separator in a regular expression literal into the raw character, which a regular
// expression literal cannot hold.
const UNSEEN = new RegExp("[\\u0000-\\u001f\\u007f-\\u009f\\u2028\\u2029\\u200e\\u200f\\u202a-\\u202e\\u2066-\\u2069]", "gu");

/** A string from the server made fit for the page: no control or direction character, at most `max` characters, never empty. Null for anything else. */
export function shown(v: unknown, max: number): string | null {
  if (typeof v !== "string" || v.length > max * 8) return null;
  const clean = v.replace(UNSEEN, "").trim();
  if (clean === "") return null;
  const chars = Array.from(clean);
  return chars.length <= max ? clean : chars.slice(0, max).join("");
}

const time = (v: unknown): number | null => {
  if (typeof v !== "string" || v.length > 64) return null;
  const ms = Date.parse(v);
  return Number.isFinite(ms) ? ms : null;
};

/** The platforms a device gives, as the server's closed list names them, and the word the page shows for each. */
export const PLATFORMS: Readonly<Record<string, string>> = { macos: "macOS", windows: "Windows", linux: "Linux", android: "Android", other: "Other" };

/** The word for a platform of the closed list, or null for any other value. */
export function platformWord(v: unknown): string | null {
  return typeof v === "string" && Object.prototype.hasOwnProperty.call(PLATFORMS, v) ? (PLATFORMS[v] ?? null) : null;
}

/** What the lookup of a code says: who asks, and from where. Nothing of the codes. */
export interface Lookup {
  name: string;
  platform: string;
  addr: string;
  requestedMs: number;
}

/** The answer of `POST /api/device/lookup` as the page uses it, or null when it is not of the documented shape. */
export function readLookup(raw: unknown): Lookup | null {
  if (!isObject(raw)) return null;
  const name = shown(raw.name, MAX_NAME);
  const platform = platformWord(raw.platform);
  const addr = shown(raw.addr, MAX_ADDR);
  const requestedMs = time(raw.created_at);
  if (name === null || platform === null || addr === null || requestedMs === null) return null;
  return { name, platform, addr, requestedMs };
}

/** One device of a list. `owner` is the name of the person it belongs to, in the administrator's list only. */
export interface DeviceRow {
  id: string;
  name: string;
  platform: string;
  /** When it was last seen, or null when it never was. */
  lastSeenMs: number | null;
  owner: string | null;
}

function readDevice(raw: unknown, admin: boolean): DeviceRow | null {
  if (!isObject(raw)) return null;
  const id = wholeId(raw.id, MAX_ID);
  const name = shown(raw.name, MAX_NAME);
  const platform = platformWord(raw.platform);
  if (id === null || id === "" || name === null || platform === null) return null;
  if (time(raw.created_at) === null) return null;
  let lastSeenMs: number | null = null;
  if (raw.last_seen !== null && raw.last_seen !== undefined) {
    lastSeenMs = time(raw.last_seen);
    if (lastSeenMs === null) return null;
    // The start of time is how a device that was never seen is written.
    if (lastSeenMs <= 0) lastSeenMs = null;
  }
  let owner: string | null = null;
  if (admin) {
    owner = shown(raw.username, MAX_NAME) ?? shown(raw.display_name, MAX_NAME);
    if (owner === null) return null;
  }
  return { id, name, platform, lastSeenMs, owner };
}

/**
 * The answer of `GET /api/me/devices` (`admin` false) or `GET /api/admin/devices` (true) as the page uses it, or null when it is not of
 * the documented shape or holds more than the page takes. Only the five members above are kept.
 */
export function readDevices(raw: unknown, admin: boolean): DeviceRow[] | null {
  if (!isObject(raw) || !Array.isArray(raw.devices)) return null;
  if (raw.devices.length > (admin ? MAX_ALL_DEVICES : MAX_PERSON_DEVICES)) return null;
  const rows: DeviceRow[] = [];
  for (const d of raw.devices) {
    const row = readDevice(d, admin);
    if (row === null) return null;
    rows.push(row);
  }
  return rows;
}

/** One switch as the group draws it: on or off, and whether the operator's configuration lets it be on at all. */
export interface SwitchState {
  /** Whether the switch is drawn `ON`: never while it is locked, whatever the server's stored value says. */
  on: boolean;
  locked: boolean;
}

export type Access = Record<SwitchKey, SwitchState>;

function readSwitch(raw: unknown): SwitchState | null {
  if (!isObject(raw) || typeof raw.on !== "boolean" || typeof raw.allowed !== "boolean") return null;
  return { on: raw.on && raw.allowed, locked: !raw.allowed };
}

/** The answer of `GET` and `POST /api/admin/device-access` as the page uses it, or null when it is not of the documented shape. */
export function readAccess(raw: unknown): Access | null {
  if (!isObject(raw)) return null;
  const sync = readSwitch(raw.sync);
  const remote = readSwitch(raw.remote);
  return sync === null || remote === null ? null : { sync, remote };
}

/** The body that turns one switch on or off, and nothing else. */
export function accessBody(key: SwitchKey, on: boolean): Partial<Record<SwitchKey, boolean>> {
  return { [key]: on };
}

// ---- time and the rows ----------------------------------------------------------------------------------------------------

/** When a device was last seen, in words: `never`, `just now`, `3 min ago`, `3 h ago`, `2 days ago`. */
export function lastSeenText(ms: number | null, now: number): string {
  return ms === null ? NEVER : agoText((now - ms) / 1000);
}

/** When the request was made: `just now`, `3 min ago`. */
export function requestedText(ms: number, now: number): string {
  return agoText((now - ms) / 1000);
}

/** The line of a device in the person's list: `macOS · just now`. */
export function metaText(d: DeviceRow, now: number): string {
  return `${d.platform} · ${lastSeenText(d.lastSeenMs, now)}`;
}

/** The devices as rows: the one seen last first, a device never seen last, the order of the server otherwise. */
export function orderDevices(rows: readonly DeviceRow[]): DeviceRow[] {
  return rows
    .map((row, at) => ({ row, at }))
    .sort((a, b) => (b.row.lastSeenMs ?? -1) - (a.row.lastSeenMs ?? -1) || a.at - b.at)
    .map((x) => x.row);
}

// ---- what a refused request means -----------------------------------------------------------------------------------------

/**
 * The sentence for a request about a code that failed (`status` 0: nothing arrived). Unknown, used, expired, malformed and
 * hidden all give the one sentence; the server being busy or the person trying too often say so.
 */
export function codeMessage(status: number, body?: Record<string, unknown>): string {
  if (status === 429 || status === 503 || status === 0 || status >= 500) return messageFor(status, body);
  return NOT_VALID;
}

/** The sentence for a failed change of a list or a switch. `status` is 0 when nothing arrived. */
export function deviceChangeMessage(status: number, body?: Record<string, unknown>): string {
  if (status === 0 || status === 429 || status === 503 || status >= 500) return messageFor(status, body);
  return CHANGE_FAILED;
}
