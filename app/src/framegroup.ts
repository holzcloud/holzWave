// The group `Window frame` of the Settings dialog (docs/design.md section 6.33): the person's way to choose the frame (`Auto`,
// `Own`, `System`) and, on Linux, the button style (`Auto`, `Flat`, `Round`), and the way out when a Wayland window cannot be
// moved. The page asks the shell through two commands and draws what the shell answered.
//
//   window_frame_status()                    the view `{applies, frame, buttons, pending}`; `applies` is false where the group is not built
//   window_frame_set({frame, buttons})       the same view after the choice was stored; a refusal is a string
//
// Every text is the page's own (framegroupstate.ts); the shell's words only select a radio of the page's own tables. Text goes in
// with `textContent` and nothing is built from a string the shell sent. The group is built only when the shell says it applies,
// so on macOS, on Android and in the browser build nothing of it is in the document and nothing is called. There is no timer, no
// polling and no focus read: a frame cannot change on an open window, so the status line says when a choice waits for the next
// start, and the one status read happens when the dialog opens.
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { GROUP, describeFrameGroup, readFrameView } from "./framegroupstate.js";
import type { FrameDescription, FrameView } from "./framegroupstate.js";

/** The longest reason of the shell that is shown, in characters. */
const MAX_REASON = 240;

/** The reason of a refused command as a line of text: control characters are blanks and the length is bounded. */
function reasonOf(failure: unknown): string {
  const raw = typeof failure === "string" ? failure : failure instanceof Error ? failure.message : "";
  const clean = raw.replace(/[\u0000-\u001f\u007f-\u009f]/g, " ").trim().slice(0, MAX_REASON);
  return clean === "" ? GROUP.failed : clean;
}

/**
 * Ask the shell for the view once and, when the group applies on `os` (`windows` or `linux`), build it at the end of `parent`.
 * Returns at once with the function that removes the group (or stops it from ever appearing). If the call fails, the answer is
 * not a view, or `applies` is false, nothing is added to the document and no style sheet is loaded. Call this only on Windows
 * and Linux in the desktop app.
 */
export function mountFrameGroup(parent: Element, os: string): () => void {
  let disposed = false;
  let remove = (): void => {
    disposed = true;
  };
  if (os !== "windows" && os !== "linux") return remove;

  void (async () => {
    let raw: unknown;
    try {
      raw = await call<unknown>("window_frame_status");
    } catch {
      return;
    }
    const first = readFrameView(raw);
    if (disposed || first === null || describeFrameGroup(first, os) === null) return;
    try {
      await loadStyles("./framegroup.css");
    } catch {
      // the group still works without its style sheet
    }
    if (disposed || !parent.isConnected) return;
    build(parent, os, first, (dispose) => {
      remove = dispose;
    });
  })();

  return () => remove();
}

/** Build the group around the first view and hand the function that removes it back to the caller. */
function build(parent: Element, os: string, first: FrameView, give: (dispose: () => void) => void): void {
  const description = describeFrameGroup(first, os) as FrameDescription;
  const set = el("fieldset", "set-group set-fr", parent);
  set.setAttribute("role", "group");
  set.setAttribute("aria-labelledby", "set-fr-legend");
  const legend = el("legend", "", set);
  legend.id = "set-fr-legend";
  legend.textContent = description.legend;

  /** One entry per drawn row, in the order of the description. */
  const rows = description.rows.map((row) => {
    const fieldset = el("fieldset", "set-choice set-fr-row", set);
    const rowLegend = el("legend", "", fieldset);
    el("span", "set-fr-name", rowLegend).textContent = row.name;
    rowLegend.append(" "); // the accessible name reads `Frame Windows and Linux`, not one word
    el("span", "set-fr-meta", rowLegend).textContent = row.meta;
    const list = el("div", "set-options", fieldset);
    const radios = row.options.map(([value, label], i) => {
      const item = el("label", "set-option", list);
      const radio = el("input", "set-fr-radio", item);
      radio.type = "radio";
      radio.name = `set-${row.key}`;
      radio.id = `set-${row.key}-${i}`;
      radio.value = value;
      el("span", "", item).textContent = label;
      return radio;
    });
    const advice = el("p", "set-fr-advice", fieldset);
    advice.id = `set-fr-advice-${row.key}`;
    advice.textContent = row.advice.join(" ");
    fieldset.setAttribute("aria-describedby", advice.id);
    return { key: row.key, radios };
  });
  const line = el("p", "set-fr-line", set);
  line.setAttribute("role", "status");

  const radiosOf = (key: "frame" | "buttons"): HTMLInputElement[] => rows.find((r) => r.key === key)?.radios ?? [];
  const all = (): HTMLInputElement[] => rows.flatMap((r) => r.radios);

  let disposed = false;
  /** Counts every start of a press: an older answer that comes back late is dropped. */
  let generation = 0;
  let busy = false;
  /** The last view the shell answered; a refused or failed press puts the radios back to it. */
  let shown: FrameView = first;

  /** Draw `view`: the selected radio of each row, and the status line. */
  const draw = (view: FrameView, description: FrameDescription): void => {
    shown = view;
    for (const row of description.rows) {
      for (const radio of radiosOf(row.key)) radio.checked = radio.value === row.selected;
    }
    setText(line, description.status);
  };

  const dispose = (): void => {
    disposed = true;
    set.remove();
  };

  /** Put the radios of both rows back to the last answer. */
  const restore = (): void => {
    const again = describeFrameGroup(shown, os);
    if (again === null) return;
    for (const row of again.rows) {
      for (const radio of radiosOf(row.key)) radio.checked = radio.value === row.selected;
    }
  };

  /**
   * Hold or give back the radios. The radio that has the keyboard stays enabled while a command is in flight: a disabled element
   * loses the focus, and WebKit then draws no ring on the radio the keyboard is given back to, so a person on the keyboard would
   * lose sight of where they are. The others cannot be pressed meanwhile, and `busy` drops a change of this one.
   */
  const lock = (on: boolean, keep?: HTMLInputElement): void => {
    for (const radio of all()) radio.disabled = on && radio !== keep;
  };

  /** Send the choice: the selected words of both rows. On Windows the second word is the one the shell last sent. */
  const press = async (): Promise<void> => {
    if (busy || disposed) return;
    const picked = (key: "frame" | "buttons"): string | undefined => radiosOf(key).find((r) => r.checked)?.value;
    const frame = picked("frame") ?? shown.frame;
    const buttons = os === "linux" ? (picked("buttons") ?? shown.buttons) : shown.buttons;
    const focused = all().find((r) => r === document.activeElement);
    const mine = ++generation;
    busy = true;
    set.setAttribute("aria-busy", "true");
    lock(true, focused);
    /** Leave the in-flight state. */
    const settle = (): void => {
      busy = false;
      set.removeAttribute("aria-busy");
      lock(false);
    };
    let raw: unknown;
    try {
      raw = await call<unknown>("window_frame_set", { frame, buttons });
    } catch (failure) {
      if (disposed || mine !== generation) return;
      settle();
      restore();
      setText(line, reasonOf(failure));
      return;
    }
    if (disposed || mine !== generation) return;
    settle();
    const view = readFrameView(raw);
    if (view === null) {
      restore();
      setText(line, GROUP.failed);
      return;
    }
    const next = describeFrameGroup(view, os);
    if (next === null) {
      dispose();
      return;
    }
    draw(view, next);
  };

  for (const radio of all()) radio.addEventListener("change", () => void press());
  draw(first, description);
  give(dispose);
}
