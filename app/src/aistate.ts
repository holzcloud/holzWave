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

const KEY_ADVICE: Readonly<Record<KeyState, string>> = Object.freeze({
  not_set: "",
  present: "The key is stored in the system keychain. It is never shown again; enter a new one to replace it.",
  missing: "The keychain has no key for this provider. Enter it again.",
  wrong_address: "The address changed after the key was saved, so the key is not used. Enter the key again.",
  damaged: "The saved key record is damaged. Enter the key again.",
  unreadable: "The keychain could not be read. Try again later.",
  no_secure_store: "This device has no system keychain, so a key cannot be saved. A server that needs no key still works.",
});

/** What to do about a key in this state, in one or two sentences; empty when nothing needs saying. */
export function keyAdvice(state: unknown): string {
  const known = keyStateOf(state);
  return known === null ? "" : KEY_ADVICE[known];
}

/**
 * How loud a key state is: `ok` for a saved key, `warn` for anything that needs attention, `plain` for no key where a key is
 * optional. The word is the first cue and the colour the second; every warn word is also underlined.
 */
export function keyTone(state: unknown, need: "optional" | "required"): "ok" | "warn" | "plain" {
  if (state === "present") return "ok";
  if (state === "not_set") return need === "required" ? "warn" : "plain";
  return "warn";
}

// ---- the core's refusals, in words at their field ---------------------------------------------------------------------------

/** The part of the form a refusal belongs to; `form` is the status line. */
export type FaultField = "name" | "address" | "model" | "wire" | "key" | "form";

/** A refusal of the core as the dialog shows it. */
export interface Fault {
  field: FaultField;
  /** The core's stable name of the reason; empty when it sent none. */
  reason: string;
  text: string;
}

/** Which refusal came from which command: a save of a provider, or a key. */
export type FaultContext = "profile" | "key";

type Entry = { field: FaultField; text: string };

const entry = (field: FaultField, text: string): Entry => ({ field, text });

/**
 * The words for every reason name the core has (the 18 address refusals and the profile reasons for a save, the key reasons
 * for a key), each at the field it belongs to. The names are held to the core by the golden; a name the page does not have
 * falls back to the core's own line.
 */
export const REASONS: Readonly<Record<FaultContext, Readonly<Record<string, Entry>>>> = Object.freeze({
  profile: Object.freeze({
    // the 18 refusals of an address, in the core's order
    syntax: entry("address", "This does not look like an address. Check it and try again."),
    too_long: entry("address", "The address is too long. It can have at most 2048 characters."),
    scheme: entry("address", "Use an address that starts with https, or with http for a server on this computer or your own network."),
    credentials: entry("address", "The address must not contain a user name or password."),
    host: entry("address", "The address has no host name. Enter the address of the server."),
    port: entry("address", "The port of the address cannot be zero."),
    query: entry("address", "The address must not contain a query."),
    path: entry("address", "The path of the address is not allowed: it has a backslash, an encoded slash or dot, or a dot segment."),
    plain_http: entry("address", "Plain http is only allowed for this computer, or for your local network when you tick the box below."),
    link_local: entry("address", "This is a link-local address. Requests to it are refused."),
    metadata: entry("address", "This is a cloud metadata address. Requests to it are refused."),
    unspecified: entry("address", "This is the unspecified address, which names no server."),
    multicast: entry("address", "This is a multicast address, which names no single server."),
    reserved: entry("address", "This is a reserved address. Use the address of a real server."),
    local_only: entry("address", "Local only is on, and this address is not on this computer."),
    key_scope: entry("key", "The key was saved for another address. Enter the key again."),
    key_in_url: entry("form", "A key cannot be part of the address. Enter it in the key field."),
    header: entry("form", "A header of the request is not allowed."),
    // the reasons of the profile itself
    id: entry("form", "The provider id is not valid."),
    reserved_id: entry("form", "This provider id is reserved."),
    name: entry("name", "The name must be 1 to 80 characters without control characters."),
    model: entry("model", "The model must be at most 200 characters without control characters."),
    wire: entry("wire", "This type of provider does not speak that API shape."),
    preset_change: entry("form", "The type of a saved provider cannot change. Add a new provider for another type."),
    needs_address: entry("address", "This type needs an address. Enter the address of your gateway or server."),
    too_many: entry("form", "There are already 32 providers. Remove one first."),
    unknown_profile: entry("form", "This provider no longer exists. Close the dialog and open it again."),
  }),
  key: Object.freeze({
    no_secure_store: entry("key", "This device has no system keychain, so a key cannot be saved."),
    store: entry("key", "The system keychain refused the request. Try again later."),
    invalid: entry("key", "A key is one line of printable characters without spaces, at most 2048 bytes."),
    unknown_profile: entry("form", "This provider no longer exists. Close the dialog and open it again."),
    no_such_slot: entry("key", "This type of provider has no such key."),
  }),
});

/** The longest reason name the page keeps, in characters. */
const MAX_REASON = 40;

/**
 * A refusal of the core as words at a field. The reason name picks the words from `REASONS`; a name that is in no table is
 * shown as the core's own line (`text`, cleaned and cut) in the status line; anything that is not an object with a text is
 * a fixed sentence. Nothing the user typed is in any of these words.
 */
export function describeFault(raw: unknown, context: FaultContext): Fault {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return { field: "form", reason: "", text: REFUSED };
  const named = own(raw, "reason");
  const reason = typeof named === "string" ? clean(named, MAX_REASON) : "";
  const table = REASONS[context];
  const known = reason !== "" && Object.hasOwn(table, reason) ? table[reason] : undefined;
  if (known !== undefined) return { field: known.field, reason, text: known.text };
  return { field: "form", reason, text: typeof own(raw, "text") === "string" ? coreText(raw) : REFUSED };
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

// ---- the review of a gateway or a server of the user's own ----------------------------------------------------------------------

/** The longest host the page writes into a card or a button, in characters (the longest name a host can have). */
const MAX_HOST = 253;

/** The ids of the providers whose address waits for the user's review, in list order. */
export function reviewIds(raw: unknown): string[] {
  return readProfileViews(raw)
    .profiles.filter((r) => r.disclosure !== null)
    .map((r) => r.id);
}

/** The card the dialog shows for a provider that waits for review. */
export interface DisclosureWords {
  title: string;
  body: string;
  /** The text of the button that allows this host. */
  allow: string;
}

/**
 * What a gateway or a server of the user's own can see, and the button that allows it. The sentence about OpenRouter says
 * only what the core does (it asks the gateway not to use providers that keep data) and that logging stays possible.
 */
export function disclosureWords(kind: DisclosureKind, preset: PresetTag | string, host: string): DisclosureWords {
  const shown = clean(host, MAX_HOST);
  if (kind === "gateway") {
    const more =
      preset === "openrouter"
        ? " holzWave asks OpenRouter not to use providers that keep your data. OpenRouter can still log what it receives."
        : "";
    return {
      title: `Gateway: ${shown}`,
      body: `A gateway relays your prompts to other providers and may keep a log of them. holzWave sends nothing to ${shown} until you allow it.${more}`,
      allow: `Allow ${shown}`,
    };
  }
  return {
    title: `Your own server: ${shown}`,
    body: `holzWave cannot tell who runs ${shown} or what it keeps. It sees every prompt you send to it. holzWave sends nothing until you allow it.`,
    allow: `Allow ${shown}`,
  };
}

// ---- the privacy switches --------------------------------------------------------------------------------------------------------

/** The hint of the `Local only` box. */
export const LOCAL_ONLY_HINT =
  "Refuse every request whose destination is not this computer. A server on your local network counts as not this computer.";
/** The choices of `Models on this computer`: the value the core takes and the words. */
export const LOCAL_CONTEXT_OPTIONS: readonly (readonly [string, string])[] = Object.freeze([
  ["ask", "Ask first"],
  ["auto", "Do not ask"],
] as const);
/** The hint of `Models on this computer`. */
export const LOCAL_CONTEXT_HINT =
  "Whether a model on this computer needs your consent before it sees terminal text. A provider that is not on this computer always asks.";
