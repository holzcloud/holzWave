// The words and rules of the AI chat that need no page: which profile a message is sent with, how a profile list and a
// refusal are read. Pure: no DOM, no command call, no timer. Every text in this file is model or user text or comes
// from the core, so it is cut and cleaned before it is kept, and whoever draws it uses text nodes only.
//
// Nothing here decides where a request goes except `pickProfile`, and that one names only what the user chose, what the
// chat is pinned to or the one saved profile. A name, a preset or an address is never read to guess a place.

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
