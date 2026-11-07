// The file hand-over on the page (docs/design.md section 6.6, rule File hand-over): on Android two entries of the command palette
// and two rows of the phone's sheet copy one file at a time between the system's file dialog and the shell's own folder. Nothing
// here exists on any other platform: `initHandover` does nothing there, so a desktop and a browser get no action, no row, no
// stylesheet and no request, and the module is not even fetched (phone.ts loads it on demand).
//
// The page decides nothing about files. `handover_in` shows the system's dialog and copies what was chosen into `~/handover/`;
// `handover_out` takes the path the person typed (from the shell's folder), checks it in the host, shows the system's dialog to
// name the target and copies. Both answer one of a few fixed kinds (handoverstate.ts), and the line the person reads is made here
// from those and from a cleaned file name, which goes in with `textContent` only. There is no folder to browse and no mirror.
import { ACTION_IN, ACTION_OUT, STRINGS, WARN_MARK, messageFor, parseOutcome } from "./handoverstate.js";
import type { Direction, Line } from "./handoverstate.js";
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { NOTICE_MS } from "./labels.js";
import { addSheetRow } from "./phone.js";
import { state } from "./state.js";

/** How long a line stays when nobody taps it away: a little longer than the status line's notice, because it may be a refusal. */
const LINE_MS = NOTICE_MS + 3000;

let started = false;
/** A hand-over is running: the system's dialog is open or a copy is being made. A second tap meanwhile does nothing. */
let busy = false;

// ---- the line --------------------------------------------------------------------------------------

let line: HTMLElement | null = null;
let lineTimer: ReturnType<typeof setTimeout> | undefined;

/** The place of the line: over the top of the stage, where the notice of the vault is, so nothing reflows. */
function place(): HTMLElement | null {
  const stage = document.getElementById("stage");
  if (!stage) return null;
  let node = document.getElementById("handover-top");
  if (!node) {
    node = el("div", "", stage);
    node.id = "handover-top";
  }
  return node;
}

function clearLine(): void {
  clearTimeout(lineTimer);
  line?.remove();
  line = null;
}

/** Say `text` in the notice style; a refusal or a failure carries a `!` mark and an underline besides its colour. A tap takes it away. */
function showLine(shown: Line): void {
  const top = place();
  if (!top) return;
  clearLine();
  const card = el("button", "handover-line", top);
  card.type = "button";
  card.setAttribute("role", "status");
  card.dataset.tone = shown.tone;
  if (shown.tone === "warn") {
    const mark = el("span", "handover-mark", card);
    mark.textContent = WARN_MARK;
    mark.setAttribute("aria-hidden", "true");
  }
  setText(el("span", "handover-text", card), shown.text);
  card.addEventListener("click", clearLine);
  line = card;
  lineTimer = setTimeout(clearLine, LINE_MS);
}

/** What an answer of the host says, shown; a cancelled dialog says nothing. An answer the page cannot read is a failure. */
function tell(raw: unknown, direction: Direction): void {
  const outcome = parseOutcome(raw) ?? ({ kind: "failed" } as const);
  const shown = messageFor(outcome, direction);
  if (shown) showLine(shown);
}

// ---- the question of a copy out --------------------------------------------------------------------

/**
 * Ask which file to copy out, in a one-line box of the palette's own shape: the prompt, one text field with the question as its
 * label, Enter to send, Escape or a tap outside to leave. Resolves with the typed text, or `null` when the person left. An
 * empty answer is not an answer: the box stays and nothing is sent. The text is sent as it was typed; the host decides what it names.
 */
function askPath(): Promise<string | null> {
  return new Promise((resolve) => {
    const backdrop = el("div", "palette-backdrop open handover-ask-backdrop", document.body);
    const box = el("div", "palette handover-ask", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-label", STRINGS.askPath);
    const row = el("div", "handover-ask-row", box);
    const prompt = el("span", "handover-prompt", row);
    prompt.textContent = "❯";
    prompt.setAttribute("aria-hidden", "true");
    const input = el("input", "palette-input handover-input", row);
    input.type = "text";
    input.placeholder = STRINGS.askPath;
    input.setAttribute("aria-label", STRINGS.askPath);
    input.setAttribute("autocomplete", "off");
    input.setAttribute("autocorrect", "off");
    input.setAttribute("autocapitalize", "off");
    input.setAttribute("enterkeyhint", "go");
    input.spellcheck = false;

    let pop: () => void = () => {};
    const finish = (answer: string | null): void => {
      pop();
      backdrop.remove();
      if (answer === null) focusInput();
      resolve(answer);
    };
    pop = pushKeyHandler((e) => {
      if (e.isComposing) return true;
      if (e.key === "Escape") {
        e.preventDefault();
        finish(null);
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (input.value.trim() !== "") finish(input.value);
      } else if (e.key === "Tab") {
        e.preventDefault(); // one control: the focus stays in the field
      }
      return true; // typing: the field takes the character, nothing behind it sees the key
    });
    backdrop.addEventListener("click", (e) => {
      if (e.target === backdrop) finish(null);
    });
    input.focus({ preventScroll: true });
  });
}

// ---- the two actions -------------------------------------------------------------------------------

/** Run one hand-over: only one at a time, and whatever happens the page is free again afterwards. */
async function handOver(direction: Direction, command: () => Promise<unknown>): Promise<void> {
  if (busy) return;
  busy = true;
  clearLine(); // a line of an earlier copy is not about this one
  try {
    tell(await command(), direction);
  } catch {
    // The host answers every failure as a kind, so a rejected call is the bridge itself failing.
    tell(null, direction);
  } finally {
    busy = false;
  }
}

/** Copy a file into the shell folder: the system's dialog, then one line. */
function copyIn(): void {
  void handOver("in", () => call<unknown>("handover_in"));
}

/** Copy a file out of the shell folder: the path first, then the system's dialog, then one line. */
function copyOut(): void {
  if (busy) return;
  busy = true; // taken while the question is open, so a second tap does not open a second box
  void askPath().then((path) => {
    busy = false;
    if (path !== null) void handOver("out", () => call<unknown>("handover_out", { path }));
  });
}

/**
 * Start the hand-over on Android: the stylesheet, the two palette entries and the two rows of the sheet. On any other platform it
 * does nothing. Call once at start-up.
 */
export function initHandover(): void {
  if (started || state.os !== "android") return;
  started = true;
  void loadStyles("./handover.css").catch(() => {});
  registerAction({ id: ACTION_IN, title: STRINGS.copyIn, run: copyIn });
  registerAction({ id: ACTION_OUT, title: STRINGS.copyOut, run: copyOut });
  addSheetRow(STRINGS.files, STRINGS.copyIn, copyIn);
  addSheetRow(STRINGS.files, STRINGS.copyOut, copyOut);
}
