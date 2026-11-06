// The pure model of the secret vault's page (docs/design.md section 6.6, the rules Lock chip, Unlock sheet, Locked answers,
// Sync while locked, No screen lock, Preferences row and Vault lost): the shape of the host's `vault_status` answer read
// defensively, the chip's model, the words of every place, the failure words of the host, and the queue that repeats an action
// once after an unlock. No DOM and no command call here, so node can test it.
//
// The words are one table, `STRINGS`. A unit test reads the spec and fails when a string here differs from the spec's text or
// a string the rules name is missing here, so the page and the spec cannot drift apart.

/** Every fixed text of the vault on the page. The spec's section 6.6 holds each of them verbatim. */
export const STRINGS = {
  // The lock chip: the name of a state, and the word on the chip.
  locked: "Locked",
  unlocked: "Unlocked",
  notProtected: "Not protected",
  labelLocked: "LOCKED",
  labelOpen: "OPEN",
  labelNone: "NO LOCK",
  // The unlock sheet.
  sheetTitle: "Secrets are locked",
  sheetLine: "Unlock with your fingerprint, face or screen lock to use saved passwords, keys and the AI key.",
  unlock: "Unlock",
  notNow: "Not now",
  notUnlocked: "Not unlocked",
  // The locked answers.
  answerConnect: "Locked: unlock to connect",
  answerSend: "Locked: unlock to send",
  // The sync while locked: the title-bar word, the line of its popover and the pause mark before it.
  syncButton: "☁ sync paused",
  syncLine: "Paused: secrets are locked",
  syncMark: "‖",
  // The notice for a device with no screen lock.
  notice:
    "This device has no screen lock. holzWave still keeps your secrets encrypted with a key of this device, but anyone who can open the phone can use them. Set a screen lock in the system settings to protect them.",
  dismiss: "Dismiss",
  useProtected: "Use protected mode",
  // The preferences row and the dialog that turns protection off.
  group: "Security",
  rowLabel: "Protect secrets with the screen lock",
  switchOn: "ON",
  switchOff: "OFF",
  rowHelp:
    "Asked once when you open holzWave and again after 5 minutes in the background. Live connections keep running while locked.",
  offLine: "Anyone who can open this phone can then use your saved passwords and keys.",
  offScope: "This applies to this device only",
  turnOff: "Turn off",
  keepOn: "Keep on",
  // The dialog for a vault whose key is gone.
  lostTitle: "Saved secrets can no longer be read",
  lostLine:
    "The key that protected them is gone from this device. Your passwords, keys and the AI key have to be entered again.",
  startFresh: "Start fresh",
  lostNote: "Nothing else is lost: layouts, history and settings stay.",
} as const;

/** What a store that is locked says; the host's text for it, and the text of the failure a command ends with. */
export const LOCKED_TEXT = "secrets are locked";

// ---- the status ------------------------------------------------------------------------------------

export type VaultState = "none" | "new" | "locked" | "unlocked" | "lost";
export type Protection = "auth" | "plain";

/** The answer of `vault_status`, checked. */
export interface VaultStatus {
  state: VaultState;
  protection: Protection;
  hasScreenLock: boolean;
  strongbox: boolean;
  /** A plain vault can be switched to the screen lock now. */
  canProtect: boolean;
  /** The device has no screen lock and the vault runs on the device key alone. */
  noLock: boolean;
  reason: "key_gone" | "corrupt" | null;
}

const STATES: readonly string[] = ["none", "new", "locked", "unlocked", "lost"];
const PROTECTIONS: readonly string[] = ["auth", "plain"];
const NOTICES: readonly string[] = ["none", "no_lock"];
const REASONS: readonly string[] = ["key_gone", "corrupt"];

/**
 * Read the answer of `vault_status`. Anything that is not exactly the documented shape (an unknown word, a missing field, a field of
 * the wrong type) is `null`; the function never throws and no field's text is shown anywhere, only matched against the words.
 */
export function parseStatus(raw: unknown): VaultStatus | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const o = raw as Record<string, unknown>;
  const state = o["state"];
  const protection = o["protection"];
  const notice = o["notice"];
  const reason = o["reason"];
  if (typeof state !== "string" || !STATES.includes(state)) return null;
  if (typeof protection !== "string" || !PROTECTIONS.includes(protection)) return null;
  if (typeof notice !== "string" || !NOTICES.includes(notice)) return null;
  if (typeof o["has_screen_lock"] !== "boolean" || typeof o["strongbox"] !== "boolean" || typeof o["can_protect"] !== "boolean") return null;
  if (reason !== undefined && reason !== null && (typeof reason !== "string" || !REASONS.includes(reason))) return null;
  return {
    state: state as VaultState,
    protection: protection as Protection,
    hasScreenLock: o["has_screen_lock"],
    strongbox: o["strongbox"],
    canProtect: o["can_protect"],
    noLock: notice === "no_lock",
    reason: typeof reason === "string" ? (reason as "key_gone" | "corrupt") : null,
  };
}

// ---- the chip --------------------------------------------------------------------------------------

export type ChipKind = "locked" | "open" | "none";
export type Glyph = "lock-closed" | "lock-open" | "lock-bar";
/** What a tap on the chip does. */
export type ChipAction = "unlock" | "lock" | "notice";

export interface Chip {
  kind: ChipKind;
  glyph: Glyph;
  /** The word on the chip, in capitals. */
  label: string;
  /** The name of the state, for assistive technology. */
  name: string;
  action: ChipAction;
  /** The border is dashed, so the state is told apart without colour. */
  dashed: boolean;
}

/**
 * The chip of a status, or null when there is nothing to show: no vault (`none`), or a new one that protects with the screen lock
 * and holds nothing yet. A locked or lost vault is `Locked`; a vault with protection `plain` is `Not protected`; the rest is `Unlocked`.
 * The three differ by glyph and by word, never by colour alone.
 */
export function chipModel(status: VaultStatus | null): Chip | null {
  if (status === null || status.state === "none") return null;
  if (status.state === "locked" || status.state === "lost") {
    return { kind: "locked", glyph: "lock-closed", label: STRINGS.labelLocked, name: STRINGS.locked, action: "unlock", dashed: false };
  }
  if (status.protection === "plain") {
    return { kind: "none", glyph: "lock-bar", label: STRINGS.labelNone, name: STRINGS.notProtected, action: "notice", dashed: true };
  }
  if (status.state === "new") return null;
  return { kind: "open", glyph: "lock-open", label: STRINGS.labelOpen, name: STRINGS.unlocked, action: "lock", dashed: false };
}

// ---- the words of the places -----------------------------------------------------------------------

/** The quiet line of a place that cannot run because the secrets are locked. */
export function lockedAnswer(action: "connect" | "ai"): string {
  return action === "connect" ? STRINGS.answerConnect : STRINGS.answerSend;
}

/** The sync's words while paused. `state` is the sync's state word; only `paused` has them. */
export function syncLine(state: string): string {
  return state === "paused" ? STRINGS.syncLine : "";
}

/**
 * Whether `failure` is the failure of a locked store: the text itself, an error that carries it, or the host's fault object
 * (`{code, reason, text}`) whose text is exactly it. A text that merely contains it is not.
 */
export function isLockedFailure(failure: unknown): boolean {
  if (typeof failure === "string") return failure === LOCKED_TEXT;
  if (failure instanceof Error) return failure.message === LOCKED_TEXT;
  if (typeof failure === "object" && failure !== null) return (failure as { text?: unknown }).text === LOCKED_TEXT;
  return false;
}

export type FailureWord = "cancelled" | "lost" | "no_screen_lock" | "failed" | "other";

/** The fixed word a vault command failed with; any other text is `other` and is never shown. */
export function failureWord(failure: unknown): FailureWord {
  const text = typeof failure === "string" ? failure : failure instanceof Error ? failure.message : "";
  return text === "cancelled" || text === "lost" || text === "no_screen_lock" || text === "failed" ? text : "other";
}

/** The event on `document` that the vault module fires when an action waits for an unlock; `detail` is `{ action, args }`. */
export const LOCKED_EVENT = "hw:locked";

/** The action a command stands for when it fails locked: a connection that opens, or a send to an AI. Other commands have none. */
export function actionOf(cmd: string): "connect" | "ai" | null {
  if (cmd === "connect") return "connect";
  if (cmd === "ai_send" || cmd === "ai_send_with_context") return "ai";
  return null;
}

// ---- the notice of a device with no screen lock ------------------------------------------------------

/** What the page remembers of the notice during one start of the app. */
export interface NoticeMemory {
  /** The vault reported `no_lock` once during this start. */
  seen: boolean;
  /** The person dismissed it during this start. */
  dismissed: boolean;
}

/** A fresh memory: each start of the app begins with it. */
export function freshNotice(): NoticeMemory {
  return { seen: false, dismissed: false };
}

/**
 * The notice for `status`: it is shown once per start when the vault reports `no_lock` (the chip keeps saying it after a dismissal),
 * stays while the vault is plain, and a tap on the `Not protected` chip (`asked`) shows it again. `Use protected mode` is offered
 * only when the system can switch now (a screen lock has been set in the meantime).
 */
export function noticeStep(
  memory: NoticeMemory,
  status: VaultStatus | null,
  asked: boolean,
): { memory: NoticeMemory; visible: boolean; offer: boolean } {
  const next = { ...memory };
  if (status !== null && status.protection === "plain" && status.state !== "none" && status.noLock) next.seen = true;
  const plain = status !== null && status.state !== "none" && status.protection === "plain";
  const visible = plain && (asked || (next.seen && !next.dismissed));
  return { memory: next, visible, offer: visible && status !== null && status.canProtect };
}

// ---- the repeat after an unlock ----------------------------------------------------------------------

/** One action waiting for the unlock. */
interface Waiting {
  retry: () => Promise<unknown>;
  original: unknown;
  promise: Promise<unknown>;
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
}

/** The queue of actions that waited for an unlock. `openSheet` is called when a first request needs the sheet. */
export interface UnlockQueue {
  /**
   * Ask for the unlock for the action named `key`, whose failure was `original` and which is `retry`-ed once after the unlock. The
   * answer is the retry's own answer, or the original failure when the unlock was cancelled. A second request with the same key
   * while the first waits is the first one's promise: one action, one retry. A request while the sheet is open opens no second sheet.
   */
  request<T>(key: string, retry: () => Promise<T>, original: unknown): Promise<T>;
  /** The vault is unlocked: every waiting action is repeated once, in the order it was asked. A retry that fails again fails for its caller; there is no second round. */
  unlocked(): void;
  /** The sheet was left without an unlock: every waiting action ends with its original failure. */
  cancelled(): void;
  /** How many actions wait. */
  pending(): number;
}

export function createUnlockQueue(openSheet: () => void): UnlockQueue {
  const waiting = new Map<string, Waiting>();
  let sheet = false;
  const take = (): Waiting[] => {
    const all = [...waiting.values()];
    waiting.clear();
    sheet = false;
    return all;
  };
  return {
    request<T>(key: string, retry: () => Promise<T>, original: unknown): Promise<T> {
      const known = waiting.get(key);
      if (known) return known.promise as Promise<T>;
      let resolve: (value: unknown) => void = () => {};
      let reject: (reason: unknown) => void = () => {};
      const promise = new Promise<unknown>((res, rej) => {
        resolve = res;
        reject = rej;
      });
      waiting.set(key, { retry, original, promise, resolve, reject });
      if (!sheet) {
        sheet = true;
        openSheet();
      }
      return promise as Promise<T>;
    },
    unlocked(): void {
      for (const w of take()) {
        // `retry` is the raw command: it does not come back through this queue, so a second locked answer ends here.
        let again: Promise<unknown>;
        try {
          again = w.retry();
        } catch (e) {
          again = Promise.reject(e);
        }
        again.then(w.resolve, w.reject);
      }
    },
    cancelled(): void {
      for (const w of take()) w.reject(w.original);
    },
    pending: () => waiting.size,
  };
}
