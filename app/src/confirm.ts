// The confirmation of a close button. A × on a tab, a workspace chip or a pane header always asks first: what closes
// (named), how many panes and shells go with it, and when it is everything in the window, that the window closes or
// the app ends. The pure functions have no DOM at import, so node can test them; `confirmClose` builds the dialog
// from `dom.ts` helpers and puts every name in with `textContent`, never as markup. A shortcut (Cmd+W) asks nothing:
// only the buttons come through here.
import { el, loadStyles } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import type { View } from "./types.js";

export type CloseKind = "pane" | "tab" | "workspace";

/** What the person is asked about. */
export interface CloseAsk {
  kind: CloseKind;
  /** The name of what closes, as the page shows it. */
  name: string;
  /** How many panes, with their shells, go. */
  panes: number;
  /** What else ends: nothing, the window (there are other windows) or the app (this is the only window). */
  ends: "nothing" | "window" | "app";
}

/** How many panes the window holds, over every workspace. A view without the counts holds the shown panes. */
export function windowPanes(view: View): number {
  const counted = view.workspaces.reduce((n, w) => n + (typeof w.panes === "number" ? w.panes : 0), 0);
  return counted > 0 ? counted : view.panes.length;
}

/** What closing `panes` panes ends besides them: everything in the window closes the window, or the app when it is alone. */
export function endsWith(view: View, panes: number): CloseAsk["ends"] {
  if (panes < windowPanes(view)) return "nothing";
  return view.windows > 1 ? "window" : "app";
}

/** The ask for the tab at `index` of the shown workspace, named `name`; null when there is no such tab. */
export function tabAsk(view: View, index: number, name: string): CloseAsk | null {
  const tab = view.tabs[index];
  if (!tab) return null;
  const panes = typeof tab.panes === "number" && tab.panes > 0 ? tab.panes : 1;
  return { kind: "tab", name, panes, ends: endsWith(view, panes) };
}

/** The ask for the workspace at `index`, named `name`; null when there is no such workspace. */
export function workspaceAsk(view: View, index: number, name: string): CloseAsk | null {
  const workspace = view.workspaces[index];
  if (!workspace) return null;
  const panes = typeof workspace.panes === "number" && workspace.panes > 0 ? workspace.panes : 1;
  return { kind: "workspace", name, panes, ends: endsWith(view, panes) };
}

/** The ask for one pane of this window, named `name`. */
export function paneAsk(view: View, name: string): CloseAsk {
  return { kind: "pane", name, panes: 1, ends: endsWith(view, 1) };
}

/** The longest name the title shows; the full name is no use in a question. */
const NAME_COLUMNS = 40;

function shortName(name: string): string {
  const points = Array.from(name);
  return points.length > NAME_COLUMNS ? `${points.slice(0, NAME_COLUMNS).join("")}…` : name;
}

/** The words of the dialog: the question, the sentence about what ends, and the label of the confirming button. */
export function closeWording(ask: CloseAsk): { title: string; body: string; ok: string } {
  const what = ask.name === "" ? ask.kind : `${ask.kind} “${shortName(ask.name)}”`;
  const shells = ask.panes === 1 ? "The shell in it ends." : `The ${ask.panes} shells in it end.`;
  const more =
    ask.ends === "app"
      ? " It is everything holzWave has open, so holzWave quits."
      : ask.ends === "window"
        ? " It is everything in this window, so the window closes."
        : "";
  return { title: `Close ${what}?`, body: `${shells}${more}`, ok: "Close" };
}

// ---- the overlay ---------------------------------------------------------------------------------

let open = false;

/** A press on the backdrop this soon after the dialog opened is the second click of a double click on the ×, not a Cancel. */
const SETTLE_MS = 500;

/** A close confirmation is on screen: a paste is ignored meanwhile, like during the paste confirmation. */
export function isConfirmingClose(): boolean {
  return open;
}

/**
 * Ask whether to close. A modal dialog with the question, one sentence and Cancel and Close; Enter or Close resolve
 * true, Escape, Cancel or a click outside resolve false. While it is open every key goes to it (the key handler
 * stack), so nothing reaches the terminal. Focus is not restored here: the caller does that.
 */
export async function confirmClose(ask: CloseAsk): Promise<boolean> {
  if (open) return false; // one question at a time
  open = true;
  try {
    await loadStyles("./dialogs.css");
  } catch {
    // the dialog still works without its style sheet
  }
  const words = closeWording(ask);

  return new Promise<boolean>((resolve) => {
    const backdrop = el("div", "dlg-backdrop", document.body);
    const box = el("div", "dlg dlg-close", backdrop);
    box.setAttribute("role", "alertdialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "dlg-close-title");
    box.setAttribute("aria-describedby", "dlg-close-body");

    const title = el("h2", "dlg-title", box);
    title.id = "dlg-close-title";
    title.textContent = words.title;
    const body = el("p", "dlg-why", box);
    body.id = "dlg-close-body";
    body.textContent = words.body;

    const actions = el("div", "dlg-actions", box);
    const cancel = el("button", "dlg-cancel", actions);
    cancel.type = "button";
    cancel.textContent = "Cancel";
    const ok = el("button", "dlg-ok", actions);
    ok.type = "button";
    ok.textContent = words.ok;

    let pop = (): void => {};
    const finish = (result: boolean): void => {
      if (!open) return;
      open = false;
      pop();
      backdrop.remove();
      resolve(result);
    };
    pop = pushKeyHandler((e) => {
      // A Cmd chord belongs to the system menu (quit, hide): not the terminal's, so it is consumed, and its
      // default is left alone.
      if (e.metaKey) return true;
      // A held Enter or Space that opened the dialog from a focused × must not also answer it.
      if (e.repeat) {
        e.preventDefault();
        return true;
      }
      const focused = document.activeElement;
      if (e.key === "Escape") finish(false);
      else if (e.key === "Enter") finish(focused !== cancel);
      else if (e.key === " ") {
        if (focused === ok) finish(true);
        else if (focused === cancel) finish(false);
      } else if (e.key === "Tab") (focused === ok ? cancel : ok).focus();
      e.preventDefault();
      return true;
    });
    cancel.addEventListener("click", () => finish(false));
    ok.addEventListener("click", () => finish(true));
    const shownAt = performance.now();
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop && performance.now() - shownAt > SETTLE_MS) finish(false);
    });
    ok.focus();
  });
}
