// The group `Host shell` of the Settings dialog (docs/design.md section 6.32): the one switch that lets a Flatpak start new
// panes on the computer itself instead of in the sandbox. The page asks the shell through two commands and draws what the
// shell answered.
//
//   host_shell_status()          the view `{applies, state}`; `applies` is false outside a Flatpak
//   host_shell_set({enabled})    the same view after the choice was stored; a refusal is a string
//
// Every text is the page's own (hostshellstate.ts); the shell's state word only selects among them. Text goes in with
// `textContent` and nothing is built from a string the shell sent. The group is built only when the shell says it applies, so
// outside a Flatpak, on macOS, on Windows and in the browser build nothing of it is in the document and nothing is called.
// There is no timer and no focus read: the permission cannot change while the app runs, and the switch changes only the
// panes that are started afterwards.
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { GROUP, describeHostShell, readHostShell } from "./hostshellstate.js";
import type { StateRow } from "./hostshellstate.js";

/** The longest reason of the shell that is shown, in characters. */
const MAX_REASON = 240;

/** The reason of a refused command as a line of text: control characters are blanks and the length is bounded. */
function reasonOf(failure: unknown): string {
  const raw = typeof failure === "string" ? failure : failure instanceof Error ? failure.message : "";
  const clean = raw.replace(/[\u0000-\u001f\u007f-\u009f]/g, " ").trim().slice(0, MAX_REASON);
  return clean === "" ? GROUP.failed : clean;
}

/**
 * Ask the shell for the view once and, when the group applies, build it in `parent` before `before` (the end when null).
 * Returns at once with the function that removes the group (or stops it from ever appearing). If the call fails, the answer is
 * not a view, or `applies` is false, nothing is added to the document. Call this only on Linux in the desktop app.
 */
export function mountHostShellGroup(parent: Element, before: Element | null): () => void {
  let disposed = false;
  let remove = (): void => {
    disposed = true;
  };

  void (async () => {
    let raw: unknown;
    try {
      raw = await call<unknown>("host_shell_status");
    } catch {
      return;
    }
    const first = readHostShell(raw);
    if (disposed || first === null || !first.applies) return;
    try {
      await loadStyles("./hostshell.css");
    } catch {
      // the group still works without its style sheet
    }
    if (disposed || !parent.isConnected) return;
    build(parent, before, describeHostShell(first), (dispose) => {
      remove = dispose;
    });
  })();

  return () => remove();
}

/** Build the group around the first view and hand the function that removes it back to the caller. */
function build(parent: Element, before: Element | null, first: StateRow, give: (dispose: () => void) => void): void {
  const set = el("fieldset", "set-group set-hs");
  parent.insertBefore(set, before !== null && before.parentNode === parent ? before : null);
  set.setAttribute("role", "group");
  set.setAttribute("aria-labelledby", "set-hs-legend");
  const legend = el("legend", "", set);
  legend.id = "set-hs-legend";
  legend.textContent = GROUP.legend;

  const row = el("div", "set-hs-row", set);
  row.tabIndex = -1;
  const label = el("label", "set-hs-name", row);
  const box = el("input", "", label);
  box.type = "checkbox";
  label.append(GROUP.name);
  el("span", "set-hs-meta", row).textContent = GROUP.meta;
  const stateNode = el("span", "set-hs-state", row);
  const markNode = el("span", "set-hs-mark", stateNode);
  markNode.setAttribute("aria-hidden", "true");
  const wordNode = el("span", "set-hs-word", stateNode);
  const adviceNode = el("p", "set-hs-advice", row);
  const line = el("p", "set-hs-line", set);
  line.setAttribute("role", "status");

  let disposed = false;
  /** Counts every start of a press: an older answer that comes back late is dropped. */
  let generation = 0;
  let busy = false;
  let shown: StateRow = first;

  /** Draw the row for `entry`. A box that stops being pressable gives the keyboard to the row. */
  const draw = (entry: StateRow): void => {
    shown = entry;
    row.dataset.state = entry.state;
    row.dataset.tone = entry.tone;
    box.checked = entry.checked;
    if (!entry.enabled && document.activeElement === box) row.focus({ preventScroll: true });
    box.disabled = !entry.enabled;
    setText(markNode, entry.mark);
    setText(wordNode, entry.word);
    setText(adviceNode, entry.advice);
  };

  const dispose = (): void => {
    disposed = true;
    set.remove();
  };

  /** Send the choice. The box shows what the shell holds afterwards: the new state, or the old one when it refused. */
  const press = async (): Promise<void> => {
    if (busy || disposed) return;
    const enabled = box.checked;
    const mine = ++generation;
    busy = true;
    set.setAttribute("aria-busy", "true");
    let raw: unknown;
    try {
      raw = await call<unknown>("host_shell_set", { enabled });
    } catch (failure) {
      if (disposed || mine !== generation) return;
      busy = false;
      set.removeAttribute("aria-busy");
      box.checked = shown.checked;
      setText(line, reasonOf(failure));
      return;
    }
    if (disposed || mine !== generation) return;
    busy = false;
    set.removeAttribute("aria-busy");
    const view = readHostShell(raw);
    if (view !== null && !view.applies) {
      dispose();
      return;
    }
    if (view === null) {
      box.checked = shown.checked;
      setText(line, GROUP.failed);
      return;
    }
    draw(describeHostShell(view));
    setText(line, "");
  };

  box.addEventListener("change", () => void press());
  draw(first);
  give(dispose);
}
