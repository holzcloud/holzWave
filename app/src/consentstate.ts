// The pure half of the consent card (docs/design.md section 6.14): what the core's three context answers are read as, which
// ids a send names, and the words that tell what would go out. No DOM, no bridge, no command call, no timer: whoever draws
// or sends uses text nodes and the controller of `consent.ts`.
//
// The page never holds the text of an item. Every answer is read member by member (a member that is not named here never
// reaches the page, and an own `__proto__` is never copied), every text is cleaned of control, bidirectional and invisible
// characters and cut at its bound, and a malformed item makes the whole offer unreadable: what the card shows is the offer
// that a send redeems, or nothing. Nothing here decides whether a share is required, classifies an address or a host, or
// guesses a place: the core's answer says it, and the four place names of `localityWord` are the only words for it.

import { BYTES_PER_TOKEN, faultText, leavesComputer, localityWord } from "./chatview.js";

/** What a context item holds, as the core names it. */
export type CtxKind = "selection" | "screen" | "last_command" | "file" | "preview";

/** The five kinds, in the order the page lists them. */
export const CTX_KINDS: readonly CtxKind[] = ["selection", "screen", "last_command", "file", "preview"];

/** The most kinds one answer lists (and one preview names). */
export const MAX_KINDS = 5;
/** The most items one offer holds (the core's own limit). */
export const MAX_PICKS = 8;
/** The most findings one item reports: one per rule of the redactor. */
export const MAX_FINDINGS = 22;
/** The longest label, in characters, ellipsis included. */
export const MAX_LABEL = 200;
/** The longest host, in characters (a DNS name). */
export const MAX_HOST = 253;
/** The longest profile or model name, in characters. */
const MAX_NAME = 200;
/** The longest rule name, in characters. */
const MAX_RULE = 40;
/** The longest place string kept, in characters; the words come from `localityWord`. */
const MAX_PLACE = 32;
/** The longest time an offer may live, in seconds. */
const MAX_TTL = 3600;
/** The largest size or count a number of the core may have. */
const MAX_COUNT = 4294967295;

/** What `ai_context_state` and `ai_context_share` answer. */
export interface CtxState {
  profile: string;
  model: string;
  host: string;
  locality: string;
  /** Whether the pane must be shared before it is read. The core says it; the page never decides. */
  required: boolean;
  shared: boolean;
  kinds: CtxKind[];
  persist: boolean;
}

/** One thing the redactor did to an item: the rule's name and how often it fired. */
export interface Finding {
  rule: string;
  count: number;
}

/** One item of an offer: a label, a size and findings, never its text. */
export interface OfferItem {
  id: number;
  kind: CtxKind;
  label: string;
  bytes: number;
  findings: Finding[];
}

/** What `ai_context_preview` answers. */
export interface Offer {
  offer: string;
  profile: string;
  model: string;
  host: string;
  locality: string;
  ttl_seconds: number;
  items: OfferItem[];
  /** Kinds that were read and had nothing. */
  empty: CtxKind[];
  bytes: number;
  persist: boolean;
}

/** What a send names: the offer and the numbers of the ticked items. */
export interface Held {
  offer: string;
  picked: number[];
}

/** The modes of the card. */
export type Mode = "closed" | "busy" | "ask" | "pick" | "offer" | "blocked";

/**
 * Where a message of the chat would go: the pane it acts for, the provider and model the next send names, the saved entry
 * of that provider (`id|model|host` as the core listed it, null while the list is unknown) and the two privacy settings
 * that decide what asks and what is refused.
 */
export interface Route {
  /** The pick is `ask`: two or more providers and none chosen. */
  ask: boolean;
  pane: number | null;
  profile: string | null;
  model: string | null;
  entry: string | null;
  /** The two privacy settings as one string; empty while the settings are unknown. */
  settings: string;
}

/** How a route differs from an earlier one. */
export type RouteChange = "same" | "adopt" | "pane" | "provider" | "settings";

/**
 * What changed between two routes. The pane comes first, then the provider (the profile, the model, or the saved entry when
 * both sides know it), then the privacy settings. An entry that was unknown before and is known now, with everything else
 * equal, is `adopt`: the list was read, nothing changed. `ask` is not compared.
 */
export function routeChange(before: Route, after: Route): RouteChange {
  if (before.pane !== after.pane) return "pane";
  if (before.profile !== after.profile || before.model !== after.model) return "provider";
  if (before.entry !== null && after.entry !== null && before.entry !== after.entry) return "provider";
  if (before.settings !== after.settings) return "settings";
  if (before.entry === null && after.entry !== null) return "adopt";
  return "same";
}

/**
 * The mode an answer of the core leads to: no kinds means there is nothing to share (`blocked`); a share is needed and
 * not given means `ask`; otherwise `pick`. Whether a share is needed is the core's answer, never the page's.
 */
export function decideMode(state: CtxState): "ask" | "pick" | "blocked" {
  if (state.kinds.length === 0) return "blocked";
  return state.required && !state.shared ? "ask" : "pick";
}

/** What the toggle `Auto` shows. */
export interface AutoButtonView {
  /** The visible text: a filled or hollow dot, the word and the pane. */
  text: string;
  /** The accessible name: the visible words without the dot, so the name contains what is seen. */
  label: string;
  pressed: boolean;
  title: string;
}

/**
 * The toggle of the composer (docs/design.md section 6.14): on reads `● Auto · pane 2` or `● Auto · no pane`, off reads
 * `○ Auto`. The dot shape and the word are the second cue besides the rose wash. A pane that is not a positive integer counts
 * as none.
 */
export function autoButtonView(auto: boolean, pane: number | null): AutoButtonView {
  const has = typeof pane === "number" && Number.isInteger(pane) && pane >= 1;
  if (!auto) {
    return { text: "○ Auto", label: "Auto", pressed: false, title: "Messages go without a pane. Press to switch on." };
  }
  if (has) {
    return {
      text: `● Auto · pane ${pane}`,
      label: `Auto · pane ${pane}`,
      pressed: true,
      title: `Every message carries what pane ${pane} shows. Press to switch off.`,
    };
  }
  return {
    text: "● Auto · no pane",
    label: "Auto · no pane",
    pressed: true,
    title: "Focus a terminal or preview pane to send it with your messages. Press to switch off.",
  };
}

// ---- reading what the core answers ------------------------------------------------------------------------------------

/** Control, bidirectional and invisible formatting characters: removed from every text kept here. */
const UNSAFE = /[\p{Cc}\u{202A}-\u{202E}\u{2066}-\u{2069}\u{200B}-\u{200F}\u{2060}-\u{2064}\u{FEFF}\u{E0000}-\u{E007F}]/gu;

/** `text` without unsafe characters, with blanks collapsed and trimmed. */
function scrub(text: string): string {
  return text.replace(/\s+/gu, " ").replace(UNSAFE, "").replace(/ {2,}/g, " ").trim();
}

/** `text` cut at `max` characters; a cut text ends with an ellipsis within the bound when `ellipsis` is set. */
function cut(text: string, max: number, ellipsis: boolean): string {
  const chars = Array.from(text);
  if (chars.length <= max) return text;
  return ellipsis ? `${chars.slice(0, max - 1).join("")}…` : chars.slice(0, max).join("");
}

/** A string member cleaned and cut, or null when it is no string. */
function words(value: unknown, max: number, ellipsis = false): string | null {
  return typeof value === "string" ? cut(scrub(value), max, ellipsis) : null;
}

/** The own member `name` of `o`; never one from the prototype chain. */
function member(o: object, name: string): unknown {
  return Object.hasOwn(o, name) ? (o as Record<string, unknown>)[name] : undefined;
}

/** A plain object (not an array), else null. */
function record(raw: unknown): object | null {
  return typeof raw === "object" && raw !== null && !Array.isArray(raw) ? raw : null;
}

/** An integer from `low` to `high`, else null. */
function whole(value: unknown, low: number, high: number): number | null {
  return typeof value === "number" && Number.isInteger(value) && value >= low && value <= high ? value : null;
}

/** The known kinds of a list, in its order, without repeats and at most `MAX_KINDS`; null when it is no list. */
function kindList(raw: unknown): CtxKind[] | null {
  if (!Array.isArray(raw)) return null;
  const out: CtxKind[] = [];
  for (const item of raw) {
    const kind = CTX_KINDS.find((k) => k === item);
    if (kind !== undefined && !out.includes(kind) && out.length < MAX_KINDS) out.push(kind);
  }
  return out;
}

/**
 * Read the answer of `ai_context_state` or `ai_context_share`. Exactly the eight members are kept; the answer is null
 * when it is no object, a flag is no boolean or `kinds` is no list.
 */
export function readContextState(raw: unknown): CtxState | null {
  const o = record(raw);
  if (o === null) return null;
  const profile = words(member(o, "profile"), MAX_NAME);
  const model = words(member(o, "model"), MAX_NAME);
  const host = words(member(o, "host"), MAX_HOST);
  const locality = words(member(o, "locality"), MAX_PLACE);
  const required = member(o, "required");
  const shared = member(o, "shared");
  const persist = member(o, "persist");
  const kinds = kindList(member(o, "kinds"));
  if (profile === null || model === null || host === null || locality === null || kinds === null) return null;
  if (typeof required !== "boolean" || typeof shared !== "boolean" || typeof persist !== "boolean") return null;
  return { profile, model, host, locality, required, shared, kinds, persist };
}

/** Read one finding; null when it is not `{rule, count}`. */
function readFinding(raw: unknown): Finding | null {
  const o = record(raw);
  if (o === null) return null;
  const rule = words(member(o, "rule"), MAX_RULE);
  const count = whole(member(o, "count"), 0, MAX_COUNT);
  return rule === null || rule === "" || count === null ? null : { rule, count };
}

/** Read one item; null when any member is wrong. */
function readItem(raw: unknown): OfferItem | null {
  const o = record(raw);
  if (o === null) return null;
  const id = whole(member(o, "id"), 1, MAX_PICKS);
  const kind = CTX_KINDS.find((k) => k === member(o, "kind"));
  const label = words(member(o, "label"), MAX_LABEL, true);
  const bytes = whole(member(o, "bytes"), 0, MAX_COUNT);
  const list = member(o, "findings");
  if (id === null || kind === undefined || label === null || bytes === null) return null;
  if (!Array.isArray(list) || list.length > MAX_FINDINGS) return null;
  const findings: Finding[] = [];
  for (const entry of list) {
    const finding = readFinding(entry);
    if (finding === null) return null;
    findings.push(finding);
  }
  return { id, kind, label, bytes, findings };
}

/**
 * Read the answer of `ai_context_preview`. Exactly the ten members are kept. The offer id must be 32 lower-case hex
 * characters. One malformed item, a repeated id, more than eight items or none at all makes the whole offer unreadable
 * (null): the card never shows part of an offer.
 */
export function readOffer(raw: unknown): Offer | null {
  const o = record(raw);
  if (o === null) return null;
  const offer = member(o, "offer");
  if (typeof offer !== "string" || !/^[0-9a-f]{32}$/.test(offer)) return null;
  const profile = words(member(o, "profile"), MAX_NAME);
  const model = words(member(o, "model"), MAX_NAME);
  const host = words(member(o, "host"), MAX_HOST);
  const locality = words(member(o, "locality"), MAX_PLACE);
  const ttl = whole(member(o, "ttl_seconds"), 1, MAX_TTL);
  const bytes = whole(member(o, "bytes"), 0, Number.MAX_SAFE_INTEGER);
  const persist = member(o, "persist");
  const empty = kindList(member(o, "empty"));
  const list = member(o, "items");
  if (profile === null || model === null || host === null || locality === null || ttl === null || bytes === null) return null;
  if (typeof persist !== "boolean" || empty === null || !Array.isArray(list)) return null;
  if (list.length < 1 || list.length > MAX_PICKS) return null;
  const items: OfferItem[] = [];
  for (const entry of list) {
    const item = readItem(entry);
    if (item === null || items.some((x) => x.id === item.id)) return null;
    items.push(item);
  }
  return { offer, profile, model, host, locality, ttl_seconds: ttl, items, empty, bytes, persist };
}

// ---- what a send names -------------------------------------------------------------------------------------------------

/** The numbers of the items of `offer` that are not unticked (`off`), ascending. */
export function pickedOf(offer: Offer, off: ReadonlySet<number> | readonly number[]): number[] {
  const out = new Set(off);
  return offer.items
    .map((item) => item.id)
    .filter((id) => !out.has(id))
    .sort((a, b) => a - b);
}

/** What a send names for `offer` with the items `off` unticked; null when nothing is ticked. */
export function heldOf(offer: Offer, off: ReadonlySet<number> | readonly number[]): Held | null {
  const picked = pickedOf(offer, off);
  return picked.length === 0 ? null : { offer: offer.offer, picked };
}

/**
 * The two optional arguments of a send: both or neither. A pick list that is empty, longer than eight, not made of
 * integers from 1 up or not ascending, or an offer id that is not 32 lower-case hex characters, gives none.
 */
export function contextArguments(held: Held | null): { offer?: string; picked?: number[] } {
  if (held === null || !/^[0-9a-f]{32}$/.test(held.offer)) return {};
  const picked = held.picked;
  if (picked.length < 1 || picked.length > MAX_PICKS) return {};
  for (let i = 0; i < picked.length; i++) {
    const n = picked[i];
    if (n === undefined || !Number.isInteger(n) || n < 1) return {};
    const before = picked[i - 1];
    if (before !== undefined && n <= before) return {};
  }
  return { offer: held.offer, picked: [...picked] };
}

// ---- words -------------------------------------------------------------------------------------------------------------

/** The words of the five kinds; the keys are the names of the core's `ContextKind`. */
export const KIND_WORDS: Readonly<Record<CtxKind, string>> = Object.freeze({
  selection: "Selection",
  screen: "Visible screen",
  last_command: "Last command and its output",
  file: "File as saved",
  preview: "Preview as saved",
});

/** A size in bytes as words: `812 B`, `2.4 KiB`, `128 KiB`, `1.2 MiB`; anything that is no count reads `0 B`. */
export function sizeWords(bytes: number): string {
  const n = typeof bytes === "number" && Number.isInteger(bytes) && bytes >= 0 ? bytes : 0;
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) {
    const kib = n / 1024;
    return kib < 100 ? `${kib.toFixed(1)} KiB` : `${Math.round(kib)} KiB`;
  }
  return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
}

/**
 * The title of the card. `paneTitle` is the page's words for the pane (`pane 1 · shell`). A pane that is shared reads
 * `Shared · pane 1 · shell`; one that needs no share (the core said so) reads `Pane 1 · shell`.
 */
export function cardTitle(mode: Mode, state: CtxState | null, paneTitle: string, busy: "state" | "share" | "preview" | null = null): string {
  switch (mode) {
    case "ask":
      return "Consent needed";
    case "offer":
      return "Goes out with your next message";
    case "blocked":
      return "Cannot share";
    case "busy":
      return busy === "preview" ? "Reading…" : "Asking…";
    case "pick": {
      const first = Array.from(paneTitle);
      const plain = `${(first[0] ?? "").toUpperCase()}${first.slice(1).join("")}`;
      return state !== null && state.shared ? `Shared · ${paneTitle}` : plain;
    }
    default:
      return "";
  }
}

/** The sentences the page says under the transcript about the card. They tell a state; none of them is a promise. */
export const NOTES = Object.freeze({
  provider: "Sharing ended because the provider changed. Share again to attach context.",
  pane: "The chat now acts for another pane. The attached context was dropped.",
  settings: "A privacy setting changed. Add the context again.",
  expired: "The attached context expired. Add it again.",
  previewFirst: "Preview what you ticked, or untick it, before sending.",
  wait: "Wait for the context card to finish.",
  noPane: "Focus a terminal or preview pane first.",
  shareFirst: "Share this pane with the chat to send it along, or switch Auto off.",
  shared: "Pane shared. Press Send again to send it along.",
  unchanged: "This pane shows nothing new since it was last sent, so this message went without it.",
});

/** The third sentence of the consent card when Auto opened it. */
export const AUTO_ASK_WORDS =
  "With Auto on, this pane goes with every message you send, and the items are listed under each message. Switch Auto off to stop.";

/** The longest pane name kept for a title, in characters. */
const MAX_PANE_NAME = 40;

/** The words for a pane: `pane 1`, or `pane 1 · shell` with the name the page shows for it, cleaned and cut at 40 characters. */
export function paneWords(pane: number, name: string): string {
  const label = cut(scrub(name), MAX_PANE_NAME, true);
  return label === "" ? `pane ${pane}` : `pane ${pane} · ${label}`;
}

/** The part of a user message that tells what went out: labels and sizes of the items and where they went. */
export interface SentPart {
  items: readonly { label: string; bytes: number }[];
  locality: string;
}

/** `Context sent: Screen of pane 1 (2.4 KiB) → this computer`: labels and sizes joined with `; `, then the place. No text. */
export function sentLine(part: SentPart): string {
  const items = part.items.map((item) => `${cut(scrub(item.label), MAX_LABEL, true)} (${sizeWords(item.bytes)})`);
  return `Context sent: ${items.join("; ")} → ${localityWord(part.locality)}`;
}

/**
 * What the card says for each of the core's thirteen reasons of a context refusal. The keys are held to the core by a golden
 * the core's own test writes; a reason the core adds fails a test until it has a sentence here.
 */
export const REASON_WORDS: Readonly<Record<string, string>> = Object.freeze({
  not_shared: "This pane is not shared with the chat yet. Share it first.",
  stale_offer: "The attached context is no longer valid. Read it again.",
  bad_pick: "The chosen items do not match what was read. Read the context again.",
  bad_kinds: "That choice of content cannot be read. Tick one to five kinds.",
  unsupported_kind: "This pane cannot offer that kind of content.",
  nothing_to_share: "There is nothing in this pane that can be shared.",
  pane_gone: "The pane is closed or no longer shows what was shared.",
  denied: "This place may not be shared.",
  not_text: "This file is not text, so it cannot be shared.",
  unreadable: "The pane or file could not be read. Try again.",
  busy: "The pane did not answer within two seconds. Try again.",
  destination: "The address of this provider cannot be checked or is not allowed. Nothing was sent.",
  local_only: "Local only is on, and this provider is not on this computer.",
});

/** A refusal of a command, read: the core's code, the reason when the code is `context`, and the words to show. */
export interface ContextFault {
  code: string | null;
  reason: string | null;
  text: string;
}

/** What the card says about the same content sent twice in one chat. */
export const DUPLICATE_WORDS = "This content was already sent in this chat. Change it, or ask again after it changed.";

/**
 * A rejected command as the card tells it. The code and the reason are the core's; the text is the core's text, cleaned and
 * cut, or the page's words for the one code that has them. Nothing acts on the text.
 */
export function contextFault(raw: unknown): ContextFault {
  let code: string | null = null;
  let reason: string | null = null;
  if (typeof raw === "object" && raw !== null) {
    const r = raw as { code?: unknown; reason?: unknown };
    if (typeof r.code === "string") code = cut(scrub(r.code), MAX_RULE, false);
    if (code === "context" && typeof r.reason === "string") reason = cut(scrub(r.reason), MAX_RULE, false);
  }
  if (code === "context_duplicate") return { code, reason, text: DUPLICATE_WORDS };
  if (code === "context" && reason !== null && Object.hasOwn(REASON_WORDS, reason)) {
    return { code, reason, text: REASON_WORDS[reason] ?? faultText(raw) };
  }
  return { code, reason, text: faultText(raw) };
}

/** Where a refusal leaves the card: it cannot go on (`blocked`), a kind or a read is to be changed (`pick`), or it asks again. */
export type RefusalStep = "blocked" | "pick" | "again";

/**
 * The step a refused command leads to. A destination, a pane or a switch that stops everything blocks; a choice of kinds or
 * a read that failed leaves the pick list with the words; `not_shared` from a preview means the share ended, so the state is
 * asked once more. Any other code, and any unknown reason from a state or share call, blocks.
 */
export function refusalStep(fault: ContextFault, from: "state" | "share" | "preview"): RefusalStep {
  if (fault.code !== "context" || fault.reason === null) return "blocked";
  if (from !== "preview") return "blocked";
  switch (fault.reason) {
    case "not_shared":
      return "again";
    case "bad_kinds":
    case "unsupported_kind":
    case "busy":
    case "denied":
    case "not_text":
    case "unreadable":
      return "pick";
    default:
      return "blocked";
  }
}

/** What a rule's count counts: characters removed, matches, or nothing worth a number. */
export type RuleUnit = "chars" | "times" | "none";

/** The words of one redaction rule. */
export interface RuleWords {
  /** What the redactor did, without a full stop. */
  words: string;
  unit: RuleUnit;
  /** `warn` for what a person could not see in the terminal: the text carried more than it showed. */
  tone: "warn" | "plain";
}

/**
 * The words of the core's twenty-two redaction rules, in the order the core reports them. `hidden_text` counts removed
 * non-blank characters, the character rules count characters, a secret rule counts matches, and `truncated` is a cut, not a
 * count. The keys are held to the core by a golden the core's own test writes.
 */
export const RULE_WORDS: Readonly<Record<string, RuleWords>> = Object.freeze({
  hidden_text: { words: "hidden text removed", unit: "chars", tone: "warn" },
  escape_sequence: { words: "terminal escape sequences removed", unit: "times", tone: "plain" },
  control_chars: { words: "control characters removed", unit: "chars", tone: "plain" },
  bidi_control: { words: "text direction controls removed", unit: "chars", tone: "warn" },
  zero_width: { words: "invisible characters removed", unit: "chars", tone: "warn" },
  tag_chars: { words: "hidden tag characters removed", unit: "chars", tone: "warn" },
  private_key: { words: "private key hidden", unit: "times", tone: "plain" },
  anthropic_key: { words: "Anthropic key hidden", unit: "times", tone: "plain" },
  openai_key: { words: "OpenAI key hidden", unit: "times", tone: "plain" },
  google_api_key: { words: "Google API key hidden", unit: "times", tone: "plain" },
  github_token: { words: "GitHub token hidden", unit: "times", tone: "plain" },
  aws_access_key: { words: "AWS access key hidden", unit: "times", tone: "plain" },
  slack_token: { words: "Slack token hidden", unit: "times", tone: "plain" },
  jwt: { words: "JSON web token hidden", unit: "times", tone: "plain" },
  authorization: { words: "authorization header hidden", unit: "times", tone: "plain" },
  bearer_token: { words: "bearer token hidden", unit: "times", tone: "plain" },
  cookie: { words: "cookie hidden", unit: "times", tone: "plain" },
  url_credentials: { words: "password in an address hidden", unit: "times", tone: "plain" },
  secret_assignment: { words: "secret assignment hidden", unit: "times", tone: "plain" },
  high_entropy: { words: "random-looking string hidden", unit: "times", tone: "plain" },
  marker_escaped: { words: "look-alike markers defused", unit: "times", tone: "plain" },
  truncated: { words: "cut to fit the size limit", unit: "none", tone: "plain" },
});

/** What the page says about a change the redactor made that it has no words for: never dropped. */
const OTHER_CHANGE: RuleWords = { words: "other change", unit: "times", tone: "plain" };

/** The sentence beside every redaction report. The redactor is a heuristic and the page says so. */
export const BEST_EFFORT =
  "Best effort, not a guarantee: holzWave hides the secrets it recognises and cannot promise that none are left.";

/** The words of a rule: its table row, or `other change` for a rule the page does not know. */
export function ruleWords(rule: string): RuleWords {
  return Object.hasOwn(RULE_WORDS, rule) ? (RULE_WORDS[rule] ?? OTHER_CHANGE) : OTHER_CHANGE;
}

/** The rule as it may be written into an attribute: its own name when the page knows it, else `other`. */
export function ruleTag(rule: string): string {
  return Object.hasOwn(RULE_WORDS, rule) ? rule : "other";
}

/** One finding as words: `hidden text removed (15 characters)`, `Anthropic key hidden ×3`, `cut to fit the size limit`. */
export function findingWords(finding: Finding): string {
  const rule = ruleWords(finding.rule);
  const count = finding.count;
  switch (rule.unit) {
    case "chars":
      return `${rule.words} (${count} character${count === 1 ? "" : "s"})`;
    case "times":
      return count === 1 ? rule.words : `${rule.words} ×${count}`;
    default:
      return rule.words;
  }
}

/** A count written compactly, as the composer writes its tokens: 340, 1.2k, 12k, 1.5M. */
function compact(count: number): string {
  const n = Math.max(0, Math.floor(count));
  if (n < 1000) return String(n);
  if (n < 1_000_000) {
    const k = n < 10_000 ? Math.round(n / 100) / 10 : Math.round(n / 1000);
    if (k < 1000) return `${k}k`;
  }
  const m = n < 10_000_000 ? Math.round(n / 100_000) / 10 : Math.round(n / 1_000_000);
  return `${m}M`;
}

/** The core's estimate of the tokens in `bytes` of text (bytes divided by three, rounded up) as words; empty for none. */
export function tokenWordsOfBytes(bytes: number): string {
  if (typeof bytes !== "number" || !Number.isInteger(bytes) || bytes <= 0) return "";
  const tokens = Math.ceil(bytes / BYTES_PER_TOKEN);
  return `≈ ${compact(tokens)} token${tokens === 1 ? "" : "s"}`;
}

/** The total line of an offer: `2 items · 3.3 KiB · ≈ 1.1k tokens`. */
export function totalWords(count: number, bytes: number): string {
  const parts = [`${count} item${count === 1 ? "" : "s"}`, sizeWords(bytes)];
  const tokens = tokenWordsOfBytes(bytes);
  if (tokens !== "") parts.push(tokens);
  return parts.join(" · ");
}

/** Where the text would go: the host as the core sent it and the place in words. The place is the core's, never derived. */
export function destinationWords(where: { host: string; locality: string }): { host: string; place: string; leaves: boolean } {
  return { host: where.host, place: localityWord(where.locality), leaves: leavesComputer(where.locality) };
}

/** What the chat keeps of a sent context, as the core's `persist` says. */
export function persistWords(persist: boolean): string {
  return persist
    ? "This chat keeps the text that was sent, on this computer."
    : "This chat keeps a note of what was sent (labels and sizes), not the text.";
}

/** How long an offer is good: `Valid for 5 minutes.`, `Valid for 90 seconds.` */
export function validForWords(seconds: number): string {
  const n = typeof seconds === "number" && Number.isInteger(seconds) && seconds > 0 ? seconds : 0;
  if (n > 0 && n % 60 === 0) {
    const minutes = n / 60;
    return `Valid for ${minutes} minute${minutes === 1 ? "" : "s"}.`;
  }
  return `Valid for ${n} second${n === 1 ? "" : "s"}.`;
}
