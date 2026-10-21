// The macOS Local Network permission in the page (docs/design.md section 6.24): the notice of the first start and the group of
// the Settings dialog. The system asks the person only after `Allow` was pressed here; the page never touches the network
// itself, it asks the shell through three commands and draws what the shell answered.
//
//   local_network_status()          the state (and whether the notice is still due); with no `Allow` yet it answers
//                                   `not_asked` without touching the network
//   local_network_answer({allow})   `true` raises the system question, `false` ends the notice for good on this computer
//   local_network_open_settings()   opens Privacy & Security in System Settings (the shell's own fixed opener)
//
// Every text is the page's own (localnetstate.ts); the shell's state word only selects among them. Text goes in with
// `textContent` and nothing is built from a string. On Windows and Linux, and in a window that is not the first, nothing here
// runs and no command is called.
import { call, windowLabel } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { GROUP, NOTICE, describeLocalNetwork, readLocalNetwork } from "./localnetstate.js";
import type { LocalNetworkView, StateRow } from "./localnetstate.js";
import { state } from "./state.js";

/** The longest reason of the shell that is shown, in characters. */
const MAX_REASON = 240;
/** The shortest time between two reads that a window focus causes, in milliseconds. */
const FOCUS_GAP_MS = 1000;

/** The reason of a refused command as a line of text: control characters are blanks and the length is bounded. */
function reasonOf(failure: unknown): string {
  const raw = typeof failure === "string" ? failure : failure instanceof Error ? failure.message : "";
  const clean = raw.replace(/[\u0000-\u001f\u007f-\u009f]/g, " ").trim().slice(0, MAX_REASON);
  return clean === "" ? "The command failed." : clean;
}

// ---- the notice of the first start -----------------------------------------------------------------------------------

let noticeOpen = false;

/**
 * Start-up: on a Mac, in the first window only, ask the shell for the state once and show the notice when the system has the
 * setting and the person was not asked yet. Anywhere else this returns at once and calls nothing. A refusal of the shell shows
 * nothing: the group of the Settings dialog is the way to look at it.
 */
export async function initLocalNetwork(): Promise<void> {
  if (state.os !== "macos" || windowLabel !== "main") return;
  let raw: unknown;
  try {
    raw = await call<unknown>("local_network_status");
  } catch {
    return;
  }
  const view = readLocalNetwork(raw);
  if (view !== null && view.supported && view.noticeDue) await openNotice();
}

/**
 * The notice: a modal dialog with the reason and two buttons. `Allow` sends `allow: true` and closes when the shell accepted
 * it; a refusal stays in the dialog as a line of text, so the person can try again or decline. `Not now`, Escape and a click
 * outside send `allow: false` once and close, whatever the shell answers. While it is open every key goes to it (the key
 * handler stack), so nothing reaches the terminal; the keyboard goes back to the terminal input when it closes.
 */
export async function openNotice(): Promise<void> {
  if (noticeOpen) return;
  noticeOpen = true;
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./localnet.css");
  } catch {
    // the dialog still works without its style sheets
  }
  return new Promise<void>((resolve) => {
    const backdrop = el("div", "dlg-backdrop", document.body);
    const box = el("div", "dlg localnet", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "localnet-title");

    const title = el("h2", "dlg-title", box);
    title.id = "localnet-title";
    title.textContent = NOTICE.title;
    el("p", "localnet-lead", box).textContent = NOTICE.lead;
    const list = el("ul", "localnet-list", box);
    for (const item of NOTICE.items) el("li", "", list).textContent = item;
    el("p", "localnet-note", box).textContent = NOTICE.note;
    const status = el("p", "localnet-status", box);
    status.setAttribute("role", "status");

    const actions = el("div", "dlg-actions", box);
    const notNow = el("button", "localnet-notnow", actions);
    notNow.type = "button";
    notNow.textContent = NOTICE.notNow;
    const allow = el("button", "localnet-allow dlg-ok", actions);
    allow.type = "button";
    allow.textContent = NOTICE.allow;

    let pop = (): void => {};
    let busy = false;
    let done = false;
    const close = (): void => {
      if (done) return;
      done = true;
      pop();
      backdrop.remove();
      noticeOpen = false;
      focusInput();
      resolve();
    };
    /** End the notice for good on this computer. The shell's answer changes nothing here. */
    const decline = (): void => {
      if (busy || done) return;
      void call("local_network_answer", { allow: false }).catch(() => {});
      close();
    };
    /** Raise the system question. A refusal of the shell (the choice could not be saved) stays on screen. */
    const accept = async (): Promise<void> => {
      if (busy || done) return;
      busy = true;
      box.setAttribute("aria-busy", "true");
      setText(status, "");
      try {
        await call("local_network_answer", { allow: true });
      } catch (failure) {
        busy = false;
        box.removeAttribute("aria-busy");
        setText(status, reasonOf(failure));
        allow.focus(); // a click does not focus a button in every engine: the keyboard stays in the notice
        return;
      }
      close();
    };

    pop = pushKeyHandler((e) => {
      // A Cmd chord belongs to the system menu (quit, hide): not the terminal's, so it is consumed and its default is left alone.
      if (e.metaKey) return true;
      const focused = document.activeElement;
      if (e.key === "Escape") decline();
      else if (e.key === "Enter" || e.key === " ") {
        if (focused === notNow) decline();
        else if (focused === allow) void accept();
        else allow.focus(); // the keyboard was nowhere in the notice: a press only brings it back
      } else if (e.key === "Tab") (focused === allow ? notNow : allow).focus();
      e.preventDefault();
      return true;
    });
    notNow.addEventListener("click", decline);
    allow.addEventListener("click", () => void accept());
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) decline();
    });
    allow.focus();
  });
}

// ---- the group of the Settings dialog --------------------------------------------------------------------------------

/**
 * Build the group `Local network` in `parent`, before `before` (the end when null), and return the function that removes it.
 * It shows `Checking…` and no state word until the shell answered, removes itself when the shell says the system has no such
 * setting, and draws the row from the page's own table for the state word. The state is read when the group is built, after a
 * button, and when the window gains focus (at most once a second, never while a read runs); there is no timer. The three
 * buttons: `Open System Settings` always, `Allow` while macOS has not been asked, `Check again` while the state is denied or not
 * known. Call this only on macOS.
 */
export function mountLocalNetworkGroup(parent: Element, before: Element | null): () => void {
  void loadStyles("./localnet.css").catch(() => {});
  const set = el("fieldset", "set-group set-net");
  parent.insertBefore(set, before);
  set.setAttribute("role", "group");
  set.setAttribute("aria-labelledby", "set-net-legend");
  const legend = el("legend", "", set);
  legend.id = "set-net-legend";
  legend.textContent = GROUP.legend;

  const row = el("div", "set-net-row", set);
  row.tabIndex = -1;
  el("span", "set-net-name", row).textContent = GROUP.name;
  el("span", "set-net-meta", row).textContent = GROUP.meta;
  const wordNode = el("span", "set-net-state", row);
  const adviceNode = el("p", "set-net-advice", row);
  const buttons = el("div", "set-net-buttons", row);
  const line = el("p", "set-net-line", set);
  line.setAttribute("role", "status");

  const button = (cls: string, label: string, onPress: () => void): HTMLButtonElement => {
    const b = el("button", cls, buttons);
    b.type = "button";
    b.textContent = label;
    b.addEventListener("click", onPress);
    return b;
  };

  let disposed = false;
  /** Counts every start of a read or an answer: an older one that comes back late is dropped. */
  let generation = 0;
  let running = false;
  let lastFocusRead = -Infinity;
  let shown: StateRow | null = null;
  let openFailed = false;
  let allowButton: HTMLButtonElement | null = null;
  let checkButton: HTMLButtonElement | null = null;

  const remove = (): void => {
    disposed = true;
    window.removeEventListener("focus", onFocus);
    set.remove();
  };

  /** Draw the row for `entry`. A button that is not wanted any more goes, and the keyboard moves to the row if it was on it. */
  const draw = (entry: StateRow): void => {
    shown = entry;
    row.dataset.state = entry.state;
    row.dataset.tone = entry.tone;
    setText(wordNode, entry.word);
    setText(adviceNode, entry.advice);
    if (entry.allow && allowButton === null) {
      allowButton = button("set-net-allow", GROUP.allow, () => void ask());
    } else if (!entry.allow && allowButton !== null) {
      if (document.activeElement === allowButton) row.focus({ preventScroll: true });
      allowButton.remove();
      allowButton = null;
    }
    if (entry.check && checkButton === null) {
      checkButton = button("set-net-check", GROUP.check, () => void read(GROUP.checking, GROUP.checked));
    } else if (!entry.check && checkButton !== null) {
      if (document.activeElement === checkButton) row.focus({ preventScroll: true });
      checkButton.remove();
      checkButton = null;
    }
  };

  /** Show the shell's answer. False when it says the system has no such setting: the group is gone then. */
  const show = (raw: unknown): boolean => {
    const view: LocalNetworkView | null = readLocalNetwork(raw);
    if (view !== null && !view.supported) {
      remove();
      return false;
    }
    draw(describeLocalNetwork(view));
    return true;
  };

  /**
   * Read the state. `during` is the line while it runs (null: leave the line), `after` the line when the answer is drawn
   * (undefined: clear the line only when the state changed, which is what a quiet refresh does).
   */
  const read = async (during: string | null, after: string | undefined): Promise<void> => {
    const mine = ++generation;
    running = true;
    if (during !== null) setText(line, during);
    let raw: unknown;
    try {
      raw = await call<unknown>("local_network_status");
    } catch (failure) {
      if (disposed || mine !== generation) return;
      running = false;
      if (shown === null) draw(describeLocalNetwork(null));
      setText(line, reasonOf(failure));
      return;
    }
    if (disposed || mine !== generation) return;
    running = false;
    const before = shown?.state;
    if (!show(raw)) return;
    if (after !== undefined) setText(line, after);
    else if (before !== shown?.state) setText(line, "");
  };

  /** `Allow`: the shell stores the choice and the system question appears; the answer never says `denied`, so the line asks for a second read. */
  const ask = async (): Promise<void> => {
    const mine = ++generation;
    running = true;
    setText(line, GROUP.checking);
    let raw: unknown;
    try {
      raw = await call<unknown>("local_network_answer", { allow: true });
    } catch (failure) {
      if (disposed || mine !== generation) return;
      running = false;
      setText(line, reasonOf(failure));
      return;
    }
    if (disposed || mine !== generation) return;
    running = false;
    if (show(raw)) setText(line, GROUP.asked);
  };

  const open = (): void => {
    void call("local_network_open_settings").then(
      () => {
        if (disposed || !openFailed) return;
        openFailed = false;
        setText(line, "");
      },
      (failure: unknown) => {
        if (disposed) return;
        openFailed = true;
        setText(line, reasonOf(failure));
      },
    );
  };

  function onFocus(): void {
    if (disposed || running) return;
    const now = performance.now();
    if (now - lastFocusRead < FOCUS_GAP_MS) return;
    lastFocusRead = now;
    void read(null, undefined);
  }

  button("set-net-open", GROUP.open, open);
  window.addEventListener("focus", onFocus);
  void read(GROUP.checking, "");
  return remove;
}
