// The pure half of the AI providers dialog (docs/design.md section 6.13): the page's copy of the provider table, the reader
// of the provider list, the draft of a provider and the input of a save, and the words the dialog shows. No DOM, no command
// call, no timer. Every text in this file is the core's or the user's, so it is cut and cleaned before it is kept, and
// whoever draws it uses text nodes and `value` only.
//
// The page decides nothing about where a request goes. It sends an address exactly as typed (trimmed), shows the core's
// refusal by its stable name, and shows the host the core reported. The provider table below is held to the core by the
// golden `tests/fixtures/ai_profiles.json`, which a core test writes from the real tables, so a renamed type or member fails
// in the same change on both sides. The table holds no address: whether a type has a default address is a rule word
// (`default` or `required`), never an address.

/** The most providers the page keeps from a list (the core keeps at most 32). */
export const MAX_VIEWS = 64;
/** The most notes of the core the page keeps from a list. */
export const MAX_NOTES = 16;
/** The longest note, in characters. */
const MAX_NOTE = 200;
/** The longest text member of a provider (name, model, host), in characters. */
const MAX_TEXT = 300;
/** The longest address, in characters (the core's own limit). */
const MAX_ADDRESS = 2048;
/** The longest provider id the page keeps, in characters. */
const MAX_ID = 64;
/** The longest refusal the page shows, in characters. */
const MAX_FAULT = 300;
/** What a refusal says when the core said nothing readable. */
export const REFUSED = "the request was refused";

export type WireTag = "openai_chat" | "openai_responses" | "anthropic" | "gemini" | "ollama";
export type PresetTag =
  | "anthropic" | "openai" | "gemini" | "ollama" | "lm_studio" | "llama_cpp" | "openrouter" | "vercel_gateway"
  | "cloudflare_gateway" | "litellm" | "custom";
export type KeyState = "not_set" | "present" | "missing" | "wrong_address" | "damaged" | "unreadable" | "no_secure_store";
export type DisclosureKind = "gateway" | "third_party";
/** Which slot a key is for: the provider's own key, or the token of a gateway in front of it. */
export type Slot = "provider" | "gateway";

/** One kind of provider as the core's table has it, without any address. */
export interface PresetInfo {
  tag: PresetTag;
  label: string;
  /** The API shapes the type speaks; the first is the one a new provider starts with. */
  wires: readonly WireTag[];
  /** For each shape: `default` when the type has an address of its own for it, `required` when the user must give one. */
  address: Readonly<Partial<Record<WireTag, "default" | "required">>>;
  key: "optional" | "required";
  gateway_token: boolean;
  disclosure: DisclosureKind | null;
}

const row = (
  tag: PresetTag, label: string, wires: readonly WireTag[], address: PresetInfo["address"], key: PresetInfo["key"],
  gateway_token: boolean, disclosure: DisclosureKind | null,
): PresetInfo => ({ tag, label, wires, address, key, gateway_token, disclosure });

/** The provider types, in the order of the core's table. */
export const PRESETS: readonly PresetInfo[] = Object.freeze([
  row("anthropic", "Anthropic", ["anthropic"], { anthropic: "default" }, "required", false, null),
  row("openai", "OpenAI", ["openai_responses"], { openai_responses: "default" }, "required", false, null),
  row("gemini", "Google Gemini", ["gemini"], { gemini: "default" }, "required", false, null),
  row("ollama", "Ollama", ["ollama"], { ollama: "default" }, "optional", false, null),
  row("lm_studio", "LM Studio", ["openai_chat"], { openai_chat: "default" }, "optional", false, null),
  row("llama_cpp", "llama.cpp", ["openai_chat"], { openai_chat: "default" }, "optional", false, null),
  row("openrouter", "OpenRouter", ["openai_chat"], { openai_chat: "default" }, "required", false, "gateway"),
  row(
    "vercel_gateway", "Vercel AI Gateway", ["openai_chat", "anthropic"], { openai_chat: "default", anthropic: "default" },
    "required", false, "gateway",
  ),
  row(
    "cloudflare_gateway", "Cloudflare AI Gateway", ["anthropic", "openai_responses", "openai_chat", "gemini"],
    { anthropic: "required", openai_responses: "required", openai_chat: "required", gemini: "required" },
    "optional", true, "gateway",
  ),
  row("litellm", "LiteLLM", ["openai_chat"], { openai_chat: "default" }, "optional", false, "gateway"),
  row(
    "custom", "Custom", ["openai_chat", "openai_responses", "anthropic", "gemini", "ollama"],
    { openai_chat: "required", openai_responses: "required", anthropic: "required", gemini: "required", ollama: "required" },
    "optional", false, "third_party",
  ),
]);

/** The names of the API shapes, as the dialog shows them. */
export const WIRE_LABELS: Readonly<Record<WireTag, string>> = Object.freeze({
  openai_chat: "OpenAI Chat Completions",
  openai_responses: "OpenAI Responses",
  anthropic: "Anthropic Messages",
  gemini: "Gemini generateContent",
  ollama: "Ollama chat",
});

/** The two keys a provider can have: its own, and the token of a gateway in front of it (the argument `slot`). */
export const SLOTS: readonly Slot[] = Object.freeze(["provider", "gateway"] as const);

/** The type with this tag, or null. */
export function presetOf(tag: unknown): PresetInfo | null {
  return typeof tag === "string" ? (PRESETS.find((p) => p.tag === tag) ?? null) : null;
}

const KEY_STATES: readonly KeyState[] = [
  "not_set", "present", "missing", "wrong_address", "damaged", "unreadable", "no_secure_store",
];

function keyStateOf(value: unknown): KeyState | null {
  return KEY_STATES.find((k) => k === value) ?? null;
}

function wireOf(value: unknown): WireTag | null {
  return (Object.keys(WIRE_LABELS) as WireTag[]).find((w) => w === value) ?? null;
}

// ---- reading the list ---------------------------------------------------------------------------------------------------

/** Control, bidirectional and invisible formatting characters. They are removed from every text kept here. */
const UNSAFE = /[\p{Cc}\u{202A}-\u{202E}\u{2066}-\u{2069}\u{200B}-\u{200F}\u{2060}-\u{2064}\u{FEFF}\u{E0000}-\u{E007F}]/gu;

/** `text` without unsafe characters, cut at `max` characters. */
function clean(text: string, max: number): string {
  return Array.from(text.replace(UNSAFE, "")).slice(0, max).join("");
}

/** A text member of an untrusted object: a string cleaned and cut, else the empty string. */
function text(value: unknown, max: number): string {
  return typeof value === "string" ? clean(value, max) : "";
}

/** The own member `name` of an object; never one the prototype chain would supply. */
function own(object: object, name: string): unknown {
  return Object.hasOwn(object, name) ? (object as Record<string, unknown>)[name] : undefined;
}

/** The notice a provider carries before its first send: which host, and what kind of server. */
export interface Disclosure {
  host: string;
  kind: DisclosureKind;
}

/** One provider as the core lists it: the members the dialog uses, and no key. */
export interface ProfileRow {
  id: string;
  name: string;
  preset: PresetTag;
  wire: WireTag;
  base_url: string;
  model: string;
  allow_lan_http: boolean;
  /** The host the core reported, with its port when it is not the default of the scheme. */
  host: string;
  key: KeyState;
  /** The state of the gateway token; null for a type that has none. */
  gateway_key: KeyState | null;
  disclosure: Disclosure | null;
}

/** The list the dialog shows: the providers and the core's notes about the file they live in. */
export interface ProfileList {
  profiles: ProfileRow[];
  notes: string[];
}

function readDisclosure(value: unknown, host: string): Disclosure | null {
  if (value === null || value === undefined) return null;
  // Anything the page cannot read is the stricter kind, with the host of the provider: a notice is never dropped.
  if (typeof value !== "object" || Array.isArray(value)) return { host, kind: "third_party" };
  const kind = own(value, "kind") === "gateway" ? "gateway" : "third_party";
  const shown = text(own(value, "host"), MAX_TEXT);
  return { host: shown === "" ? host : shown, kind };
}

function readRow(item: unknown): ProfileRow | null {
  if (typeof item !== "object" || item === null || Array.isArray(item)) return null;
  const id = own(item, "id");
  if (typeof id !== "string" || id === "" || Array.from(id).length > MAX_ID) return null;
  const preset = presetOf(own(item, "preset"));
  const wire = wireOf(own(item, "wire"));
  if (preset === null || wire === null) return null;
  const host = text(own(item, "host"), MAX_TEXT);
  const gateway = own(item, "gateway_key");
  return {
    id,
    name: text(own(item, "name"), MAX_TEXT),
    preset: preset.tag,
    wire,
    base_url: text(own(item, "base_url"), MAX_ADDRESS),
    model: text(own(item, "model"), MAX_TEXT),
    allow_lan_http: own(item, "allow_lan_http") === true,
    host,
    // A state the page does not know is never taken for a good one.
    key: keyStateOf(own(item, "key")) ?? "damaged",
    gateway_key: gateway === null || gateway === undefined ? null : (keyStateOf(gateway) ?? "damaged"),
    disclosure: readDisclosure(own(item, "disclosure"), host),
  };
}

/**
 * Read the answer of `ai_profiles`. Every member is read by name from the object the core sent, so a key, a secret or any
 * member a newer core adds never reaches the page, and an own `__proto__` member is never copied. An entry without a usable
 * id, of an unknown type or shape, or that is not an object is dropped; the same id twice is one row; at most 64 rows and
 * 16 notes are kept. Anything that is not an object with a list of providers gives an empty list.
 */
export function readProfileViews(raw: unknown): ProfileList {
  const empty: ProfileList = { profiles: [], notes: [] };
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return empty;
  const list = own(raw, "profiles");
  if (!Array.isArray(list)) return empty;
  const profiles: ProfileRow[] = [];
  const seen = new Set<string>();
  for (const item of list) {
    if (profiles.length >= MAX_VIEWS) break;
    const parsed = readRow(item);
    if (parsed === null || seen.has(parsed.id)) continue;
    seen.add(parsed.id);
    profiles.push(parsed);
  }
  const notes: string[] = [];
  const given = own(raw, "notes");
  if (Array.isArray(given)) {
    for (const note of given) {
      if (notes.length >= MAX_NOTES) break;
      if (typeof note === "string") notes.push(clean(note, MAX_NOTE));
    }
  }
  return { profiles, notes };
}

// ---- the draft and the input of a save --------------------------------------------------------------------------------------

/** What the form holds: a provider as the user is editing it. */
export interface ProfileDraft {
  /** The id of the saved provider being edited; null for a new one. */
  id: string | null;
  name: string;
  preset: PresetTag;
  wire: WireTag;
  base_url: string;
  model: string;
  allow_lan_http: boolean;
}

/** The argument `input` of `ai_profile_save`: exactly these eight members, and no key. */
export interface ProfileInputMsg {
  id: string | null;
  name: string;
  preset: PresetTag;
  wire: WireTag;
  base_url: string;
  model: string;
  allow_lan_http: boolean;
  acknowledge_gateway: boolean;
}

/** A new provider: the first type of the table with its first shape, everything else empty. */
export function newDraft(): ProfileDraft {
  const first = PRESETS[0] as PresetInfo;
  return { id: null, name: "", preset: first.tag, wire: first.wires[0] as WireTag, base_url: "", model: "", allow_lan_http: false };
}

/** The draft of a saved provider. */
export function draftOf(saved: ProfileRow): ProfileDraft {
  return {
    id: saved.id, name: saved.name, preset: saved.preset, wire: saved.wire, base_url: saved.base_url, model: saved.model,
    allow_lan_http: saved.allow_lan_http,
  };
}

/** Whether two drafts hold the same values. */
export function sameDraft(a: ProfileDraft, b: ProfileDraft): boolean {
  return (
    a.id === b.id && a.name === b.name && a.preset === b.preset && a.wire === b.wire && a.base_url === b.base_url &&
    a.model === b.model && a.allow_lan_http === b.allow_lan_http
  );
}

/**
 * The input of a save. The name, the address and the model lose the blanks around them and nothing else: the page does not
 * check, change or classify what was typed. `acknowledge` is true only for the call of the button that allows a gateway.
 */
export function inputOf(draft: ProfileDraft, acknowledge: boolean): ProfileInputMsg {
  return {
    id: draft.id,
    name: draft.name.trim(),
    preset: draft.preset,
    wire: draft.wire,
    base_url: draft.base_url.trim(),
    model: draft.model.trim(),
    allow_lan_http: draft.allow_lan_http,
    acknowledge_gateway: acknowledge === true,
  };
}

// ---- words ----------------------------------------------------------------------------------------------------------------

const KEY_WORDS: Readonly<Record<KeyState, string>> = Object.freeze({
  not_set: "no key",
  present: "key saved",
  missing: "key missing",
  wrong_address: "key for another address",
  damaged: "key damaged",
  unreadable: "keychain unreadable",
  no_secure_store: "no keychain",
});

/** The state of a key as one word; `unknown` for a state the page does not know. */
export function keyWord(state: unknown): string {
  const known = keyStateOf(state);
  return known === null ? "unknown" : KEY_WORDS[known];
}

/**
 * The core's own one line of a refusal: the `text` of the object the core sent, or a bare string of the shell, cleaned and
 * cut. Anything else is a fixed sentence.
 */
export function coreText(raw: unknown): string {
  let line: unknown = raw;
  if (typeof raw === "object" && raw !== null) line = own(raw, "text");
  if (typeof line !== "string") return REFUSED;
  const cleaned = clean(line, MAX_FAULT);
  return cleaned === "" ? REFUSED : cleaned;
}

/** The hint under the address field: whether the type has an address of its own for this API shape. */
export function addressHint(info: PresetInfo, wire: WireTag): string {
  return info.address[wire] === "default"
    ? "Leave empty to use the address of this type."
    : "Required: the address of your gateway or server.";
}
