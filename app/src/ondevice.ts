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
/** The legend of the group of the providers dialog. */
export const DEVICE_LEGEND = "On device";
/** The dim line beside the name in the group. */
export const DEVICE_META = "system model";
/** The button that reads the state again. */
export const DEVICE_CHECK = "Check again";
/** What the dialog says after the button read the state once more. */
export const DEVICE_CHECKED = "Checked.";
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

/**
 * What the group says for each state word of the shell. The keys are the seven the shell can send (pinned to the golden the
 * bridge crate writes, `tests/fixtures/ai_device.json`); `available` is the sentence that stands where the advice goes, and
 * `other` is what any other word reads as. No word of the shell is ever shown: it only selects a sentence here.
 */
export const DEVICE_REASONS: Readonly<Record<string, string>> = Object.freeze({
  available: "The model runs on this Mac.",
  apple_intelligence_not_enabled: "Apple Intelligence is switched off.",
  device_not_eligible: "This Mac cannot run Apple Intelligence.",
  model_not_ready: "The model is not ready yet.",
  os_too_old: "This needs macOS 26 or later.",
  sdk_lacks_framework: "This build of holzWave was made without Apple's model framework.",
  other: "The on-device model is not available.",
});

/** What the group advises for each advice word of the shell; `none` has nothing to say, and `unknown` is what any other reads as. */
export const DEVICE_ADVICE: Readonly<Record<string, string>> = Object.freeze({
  none: "",
  enable_apple_intelligence: "Turn it on in System Settings, then choose Check again.",
  wait_for_model: "It may still be downloading. Try again in a few minutes.",
  not_supported: "Choose another provider.",
  update_macos: "Update macOS, then choose Check again.",
  not_in_this_build: "Use a build made with Xcode 26 or later.",
  unknown: "The reason is not known. Check again later.",
});

/** The sentences of one state: the reason, the advice and the tone (`ok` only for the exact state `available`). */
export interface DeviceWords {
  reason: string;
  advice: string;
  tone: "ok" | "warn";
}

/**
 * The words of `view`. The state word selects the reason (a word that is in no table reads as `other`) and the advice word
 * selects the advice (`unknown` for a word that is in no table). A state that is not available always has something to say:
 * when its advice is empty the sentence of `unknown` stands in, so the person is never left with a reason and no next step.
 */
export function deviceWords(view: DeviceView): DeviceWords {
  const state = Object.hasOwn(DEVICE_REASONS, view.state) ? view.state : "other";
  const advice = Object.hasOwn(DEVICE_ADVICE, view.advice) ? view.advice : "unknown";
  const available = view.state === AVAILABLE;
  const told = DEVICE_ADVICE[advice] ?? "";
  return {
    reason: DEVICE_REASONS[state] ?? "",
    advice: available ? "" : told !== "" ? told : (DEVICE_ADVICE["unknown"] ?? ""),
    tone: available ? "ok" : "warn",
  };
}
