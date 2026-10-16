// The words and rules of the AI chat that need no page: which profile a message is sent with, how a profile list and a
// refusal are read, the core's token rule, the words of a place, of a turn and of an end. Pure: no DOM, no command call,
// no timer. Every text in this file is model or user text or comes from the core, so it is cut and cleaned before it is
// kept, and whoever draws it uses text nodes only.
//
// Nothing here decides where a request goes except `pickProfile`, and that one names only what the user chose, what the
// chat is pinned to or the one saved profile. A place is told only by the four names the core's route carries
// (`localityWord`); a name, a preset or an address is never read to guess one.

import type { ChatState, Route, Usage } from "./chatstate.js";

/** The most profiles the page keeps from a list. */
export const MAX_PROFILES = 64;
/** The longest text member of a profile, in characters. */
export const MAX_LABEL = 120;
/** The longest profile id the page keeps, in characters (the core's own limit). */
const MAX_ID = 64;
/** The longest refusal the page shows, in characters. */
const MAX_FAULT = 300;
/** What a refusal says when the core said nothing readable. */
const REFUSED = "the request was refused";

/** One saved profile as the chat shows it: only the members the chip and the picker need. */
export interface ProfileChoice {
  id: string;
  name: string;
  model: string;
  /** The host the profile sends to, as the core shows it; empty when the profile has no address. */
  host: string;
}

/** Control, bidirectional and invisible formatting characters. They are removed from every text kept here. */
const UNSAFE = /[\p{Cc}\u{202A}-\u{202E}\u{2066}-\u{2069}\u{200B}-\u{200F}\u{2060}-\u{2064}\u{FEFF}\u{E0000}-\u{E007F}]/gu;

/** `text` without unsafe characters, cut at `max` characters. */
function clean(text: string, max: number): string {
  return Array.from(text.replace(UNSAFE, "")).slice(0, max).join("");
}

/** A text member of an untrusted object: a string cleaned and cut, else the empty string. */
function label(value: unknown): string {
  return typeof value === "string" ? clean(value, MAX_LABEL) : "";
}

/**
 * Read the answer of `ai_profiles`. Only the four members the chat shows are kept, and each is read by name, so a key
 * status, an address or any member a newer core adds never reaches the page and an own `__proto__` member is never
 * copied. An entry without a usable id is dropped; anything that is not an object with a `profiles` list gives an empty
 * list.
 */
export function readProfiles(raw: unknown): ProfileChoice[] {
  if (typeof raw !== "object" || raw === null) return [];
  const list = (raw as { profiles?: unknown }).profiles;
  if (!Array.isArray(list)) return [];
  const out: ProfileChoice[] = [];
  for (const item of list) {
    if (out.length >= MAX_PROFILES) break;
    if (typeof item !== "object" || item === null) continue;
    const o = item as { id?: unknown; name?: unknown; model?: unknown; host?: unknown };
    const id = o.id;
    if (typeof id !== "string" || id === "" || Array.from(id).length > MAX_ID) continue;
    out.push({ id, name: label(o.name), model: label(o.model), host: label(o.host) });
  }
  return out;
}

/** Why a profile was chosen for a send, or why none was. */
export type PickVia = "choice" | "pin" | "only" | "ask" | "none" | "unknown";

/** The profile a send goes with, and how it was found. `id` is empty when no profile can be named. */
export interface Pick {
  id: string;
  via: PickVia;
}

/**
 * Which profile a message is sent with. The order is the user's choice (when it is in the loaded list, or when no list
 * is loaded yet), then the chat's pin (also when it is no profile: a registered provider is not one), then the only
 * saved profile. Two or more profiles and no choice means `ask`: nothing is sent until the user picks one. A loaded
 * empty list is `none`, no list at all is `unknown` (read it first). A profile is never guessed.
 */
export function pickProfile(profiles: readonly ProfileChoice[] | null, choice: string | null, pin: string | null): Pick {
  if (choice !== null && choice !== "" && (profiles === null || profiles.some((p) => p.id === choice))) {
    return { id: choice, via: "choice" };
  }
  if (pin !== null && pin !== "") return { id: pin, via: "pin" };
  if (profiles === null) return { id: "", via: "unknown" };
  const first = profiles[0];
  if (profiles.length === 0 || first === undefined) return { id: "", via: "none" };
  if (profiles.length === 1) return { id: first.id, via: "only" };
  return { id: "", via: "ask" };
}

/**
 * A refusal of the core as one line. The core answers `{code, reason, text}`; only the text is shown and nothing acts on
 * the code. A bare string (an error of the shell itself) is shown the same way. Anything else is a fixed sentence.
 */
export function faultText(raw: unknown): string {
  let text: unknown = raw;
  if (typeof raw === "object" && raw !== null) text = (raw as { text?: unknown }).text;
  if (typeof text !== "string") return REFUSED;
  const cleaned = clean(text, MAX_FAULT);
  return cleaned === "" ? REFUSED : cleaned;
}

// ---- the token estimate ------------------------------------------------------------------------------------------------

/** The core's rule for a token estimate: UTF-8 bytes divided by this, rounded up. */
export const BYTES_PER_TOKEN = 3;
/** The longest message the core takes, in UTF-8 bytes. */
export const MAX_TEXT_BYTES = 131072;

/** The UTF-8 length of `text` in bytes, counted without making a copy. A lone surrogate counts as the three bytes of U+FFFD. */
export function utf8Bytes(text: string): number {
  let bytes = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c < 0x80) bytes += 1;
    else if (c < 0x800) bytes += 2;
    else if (c >= 0xd800 && c <= 0xdbff) {
      const next = text.charCodeAt(i + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        bytes += 4;
        i += 1;
      } else bytes += 3;
    } else bytes += 3;
  }
  return bytes;
}

/** The core's estimate of the tokens in `text`: bytes divided by three, rounded up. */
export function estimateTokens(text: string): number {
  return Math.ceil(utf8Bytes(text) / BYTES_PER_TOKEN);
}

/** A count written compactly: 340, 1.2k, 12k, 1.5M. */
function compact(count: number): string {
  const n = Number.isFinite(count) ? Math.max(0, Math.floor(count)) : 0;
  if (n < 1000) return String(n);
  if (n < 1_000_000) {
    const k = n < 10_000 ? Math.round(n / 100) / 10 : Math.round(n / 1000);
    if (k < 1000) return `${k}k`;
  }
  const m = n < 10_000_000 ? Math.round(n / 100_000) / 10 : Math.round(n / 1_000_000);
  return `${m}M`;
}

/** The composer's count: `≈ 12 tokens`; empty for no text. */
export function tokenWords(text: string): string {
  if (text === "") return "";
  const tokens = estimateTokens(text);
  return `≈ ${compact(tokens)} token${tokens === 1 ? "" : "s"}`;
}

/** The tokens of a turn: `in 1.2k · out 340`; empty when nothing was reported. */
export function usageWords(usage: Usage | null): string {
  if (usage === null || (usage.input_tokens === 0 && usage.output_tokens === 0)) return "";
  return `in ${compact(usage.input_tokens)} · out ${compact(usage.output_tokens)}`;
}

// ---- where a request goes ----------------------------------------------------------------------------------------------

const PLACES: Readonly<Record<string, string>> = Object.freeze({
  on_device: "on device",
  loopback: "this computer",
  lan: "local network",
  cloud: "cloud",
});

/** What the page says when the route names a place it does not know. */
const UNKNOWN_PLACE = "unknown place";

/** The place a route names as words. Only the four names of the core count; anything else is `unknown place`. */
export function localityWord(locality: unknown): string {
  return typeof locality === "string" && Object.hasOwn(PLACES, locality) ? (PLACES[locality] ?? UNKNOWN_PLACE) : UNKNOWN_PLACE;
}

/** Whether a request to this place leaves the computer for the network. Unknown places are not claimed to. */
export function leavesComputer(locality: unknown): boolean {
  return locality === "lan" || locality === "cloud";
}

/** What the provider chip shows. */
export interface ChipView {
  /** `text` is plain text, `button` opens the picker, `none` says that no provider is set up. */
  kind: "text" | "button" | "none";
  name: string;
  model: string;
  host: string;
  /** The place word of the profile's latest route; empty when no route tells it. */
  where: string;
  leaves: boolean;
  /** The visible text of the chip. */
  text: string;
  /** The sentence that says what the chip sends to. */
  aria: string;
}

/**
 * What the chip says. The profile it names is the one the next send uses (`pickProfile`); its model and host come from
 * the core's route of the latest turn when that route is for the same profile, else from the profile list, else from the
 * chat's pin. The place word comes from the route alone, and only when the route is for that profile: no place is
 * guessed from a name, a preset or an address.
 */
export function chipView(
  route: Route | null,
  profiles: readonly ProfileChoice[] | null,
  pin: { profile: string | null; model: string | null },
  choice: string | null,
): ChipView {
  const pick = pickProfile(profiles, choice, pin.profile);
  const count = profiles === null ? 0 : profiles.length;
  const listed = pick.id === "" ? undefined : profiles?.find((p) => p.id === pick.id);
  let name = "";
  let model = "";
  let host = "";
  let where = "";
  let leaves = false;
  if (pick.id !== "") {
    const told = route !== null && route.profile === pick.id ? route : null;
    name = listed !== undefined && listed.name !== "" ? listed.name : pick.id;
    model = told !== null ? told.model : (listed?.model ?? (pin.profile === pick.id ? (pin.model ?? "") : ""));
    host = told !== null ? (told.host ?? "") : (listed?.host ?? "");
    if (told !== null) {
      where = localityWord(told.locality);
      leaves = leavesComputer(told.locality);
    }
  }
  const button = count >= 2;
  if (pick.id === "" && profiles !== null && count === 0) {
    return { kind: "none", name, model, host, where, leaves, text: "no provider set up", aria: "No provider is set up" };
  }
  if (pick.id === "") {
    // Two or more profiles and none chosen (or the list is not read yet).
    const text = button ? "choose a provider" : "";
    const aria = button ? "Choose a provider. Nothing is sent until you choose one" : "";
    return { kind: button ? "button" : "text", name, model, host, where, leaves, text, aria };
  }
  const parts = [model === "" ? name : `${name} · ${model}`];
  if (host !== "") parts.push(`→ ${host}`);
  const text = parts.join(" ");
  const aria = [
    `Sends to ${name}`,
    model === "" ? "" : `model ${model}`,
    host === "" ? "" : `host ${host}`,
    where === "" ? "" : where,
  ]
    .filter((x) => x !== "")
    .join(", ");
  return { kind: button ? "button" : "text", name, model, host, where, leaves, text, aria: button ? `${aria}. Choose another provider` : aria };
}

// ---- the way to the providers dialog -----------------------------------------------------------------------------------

/**
 * The text of the chat bar's button that opens the providers dialog: `Set up…` while the list is known to be empty,
 * `Providers…` otherwise, and with the number of providers that wait for their review when there are any. A review count
 * without a profile is an inconsistent call and reads as an empty list.
 */
export function setupLabel(profiles: readonly ProfileChoice[] | null, review: number): string {
  if (profiles !== null && profiles.length === 0) return "Set up…";
  return review > 0 ? `Providers… (${review} to review)` : "Providers…";
}

// ---- the words of a turn -----------------------------------------------------------------------------------------------

/** What a running turn is doing: `working` (nothing yet), `thinking` (reasoning only), `writing`; empty when idle. */
export function statusWord(state: ChatState): string {
  if (!state.running) return "";
  if (state.partial.text !== "") return "writing";
  return state.partial.reasoning !== "" ? "thinking" : "working";
}

/** The word after a message's role when it did not complete; empty for a complete one. */
export function statusLabel(status: string): string {
  switch (status) {
    case "complete":
      return "";
    case "cancelled":
      return "stopped";
    case "failed":
      return "failed";
    case "interrupted":
      return "interrupted";
    default:
      return "unfinished";
  }
}

/** The word before a message: who wrote it. */
export function roleWord(role: string): string {
  return role === "user" ? "you" : role === "assistant" ? "assistant" : "tool";
}

/** The status line after a turn ended: how it ended in words. */
export function outcomeWord(end: string): string {
  if (end === "done") return "reply finished";
  if (end === "failed") return "reply failed";
  return "reply stopped";
}

const LIMITS: Readonly<Record<string, string>> = Object.freeze({
  limit_steps: "step limit",
  limit_tokens: "token limit",
  limit_tool_output: "tool output limit",
  limit_reply_size: "reply size limit",
  limit_repeated_call: "repeated call",
  limit_tool_errors: "tool errors",
});

/** The longest end code or class the page spells out, in characters. */
const MAX_CODE = 40;

/** A code of the core as words: underscores become spaces, control characters go, the text is cut. */
function words(code: string): string {
  return clean(code, MAX_CODE).replaceAll("_", " ");
}

/** The row after the last message when a turn did not end plainly. */
export interface EndNote {
  /** `error` for a failed turn, `note` for a stop. */
  kind: "error" | "note";
  /** `failed · overload`, `stopped · step limit`. */
  label: string;
  /** The core's one-line text of an error; empty for a note. */
  text: string;
}

/**
 * The end row of a chat, or null when the turn needs none: no turn ended, it finished, or it was stopped and its
 * message already says so. A failed turn names its class and carries the core's one-line text; a limit says which.
 */
export function endNote(state: ChatState): EndNote | null {
  const ended = state.ended;
  if (ended === null || ended.end === "done") return null;
  if (ended.end === "cancelled") {
    const last = state.messages[state.messages.length - 1];
    if (last !== undefined && last.role === "assistant" && statusLabel(last.status) !== "") return null;
    return { kind: "note", label: "stopped", text: "" };
  }
  if (ended.end === "failed") {
    if (ended.error === null) return { kind: "error", label: "failed", text: "" };
    return { kind: "error", label: `failed · ${words(ended.error.class)}`, text: clean(ended.error.text, MAX_FAULT) };
  }
  const limit = Object.hasOwn(LIMITS, ended.end) ? LIMITS[ended.end] : undefined;
  return { kind: "note", label: `stopped · ${limit ?? words(ended.end)}`, text: "" };
}

/** A chat title for a label: control and bidirectional characters become blanks, blanks collapse, cut at 80 characters. */
export function titleWords(title: string): string {
  return Array.from(title.replace(UNSAFE, " ").replace(/\s+/g, " ").trim()).slice(0, 80).join("");
}

// ---- links in a reply --------------------------------------------------------------------------------------------------

/** The longest address the page opens, in characters. */
const MAX_HREF = 2000;

/**
 * Whether a link of a model's reply may be clicked. A reply written after an injected instruction can hide data in an
 * address, so an address that carries a query string or a user name (and a password) is not a link: the page shows its
 * text followed by the address. Only `http`, `https` and `mailto` are links, an address that cannot be read is not one,
 * and a fragment alone does not make a link inert.
 */
export function linkKind(href: string): "open" | "inert" {
  if (href === "" || href.length > MAX_HREF) return "inert";
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return "inert";
  }
  if (url.protocol === "http:" || url.protocol === "https:") {
    return url.search === "" && url.username === "" && url.password === "" ? "open" : "inert";
  }
  if (url.protocol === "mailto:") return url.search === "" ? "open" : "inert";
  return "inert";
}
