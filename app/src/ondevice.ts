// The pure half of the on-device model's row: how the member `on_device` of the answer of `ai_profiles` is read, when the row
// joins the list of providers, the fixed sentence of a send that cannot go. No DOM, no command call, no timer, and nothing is
// imported. Every text in this file is the page's own: the shell's words are keys that select among them and are never shown
// as they are.
//
// The model exists only on a Mac whose system says it can answer. The shell says so with one state word, and only the exact
// word `available` makes the row selectable; any other word, in any spelling, is a state the page cannot offer.

/** The id of the on-device provider, the one the core reserves. */
export const DEVICE_ID = "apple";
/** The only model the on-device provider has. */
export const DEVICE_MODEL = "system";
/** What the provider is called in every place. */
export const DEVICE_NAME = "Apple on-device";
/** The one state word that makes the row selectable. */
export const AVAILABLE = "available";
/** What a send says when the chat is pinned to the model and the model cannot answer. Nothing goes to another provider. */
export const DEVICE_OFF = "Apple on-device is not available. Choose another provider, or see the provider settings for the reason.";

/** The longest state or advice word the page reads, in characters. */
const MAX_WORD = 48;
/** A word of the shell: lower-case letters and underscores, starting with a letter. */
const WORD = /^[a-z][a-z_]*$/;

/** What the shell said about the model: a state word and an advice word, both valid keys or `other` and `unknown`. */
export interface DeviceView {
  state: string;
  advice: string;
}

/** The row of the model in the list of providers: the members of the chat's own `ProfileChoice`. */
export interface DeviceChoice {
  id: string;
  name: string;
  model: string;
  host: string;
}

/** `value` when it is a key of the shell, else null. */
function word(value: unknown): string | null {
  return typeof value === "string" && value.length <= MAX_WORD && WORD.test(value) ? value : null;
}

/** An own member of `object`, never an inherited one. */
function own(object: object, key: string): unknown {
  return Object.hasOwn(object, key) ? (object as Record<string, unknown>)[key] : undefined;
}

/**
 * Read the member `on_device` of the whole answer of `ai_profiles`. It must be the answer's own member and a plain object
 * whose `id` is exactly `apple` and whose `model` is exactly `system`; anything else gives null. The four members are named
 * one by one and nothing is copied by the name it came with, so a member a newer shell adds, a key-like name and an own
 * `__proto__` never reach the page. A state or advice that is not a short lower-case word reads as `other` and `unknown`
 * (a word that is no word never survives, so the result can be used as a table key and a markup string is never kept).
 */
export function readDevice(raw: unknown): DeviceView | null {
  if (typeof raw !== "object" || raw === null) return null;
  const member = own(raw, "on_device");
  if (typeof member !== "object" || member === null || Array.isArray(member)) return null;
  if (own(member, "id") !== DEVICE_ID || own(member, "model") !== DEVICE_MODEL) return null;
  const state = word(own(member, "state"));
  if (state === null) return { state: "other", advice: "unknown" };
  return { state, advice: word(own(member, "advice")) ?? "unknown" };
}

/** Whether the model can answer: true for the exact state `available` and for nothing else. */
export function deviceAvailable(view: DeviceView | null): boolean {
  return view !== null && view.state === AVAILABLE;
}

/** The row of the model for the list of providers: no host, because it has no address. */
export function deviceChoice(): DeviceChoice {
  return { id: DEVICE_ID, name: DEVICE_NAME, model: DEVICE_MODEL, host: "" };
}

/** A copy of `list` that ends with the row of the model when the model is available, and is a plain copy otherwise. */
export function withDevice(list: readonly DeviceChoice[], view: DeviceView | null): DeviceChoice[] {
  const copy = list.slice();
  if (deviceAvailable(view)) copy.push(deviceChoice());
  return copy;
}
