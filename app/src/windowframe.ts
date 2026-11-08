// The window buttons of the own window frame (docs/design.md section 6.33). On Windows the shell builds every window without the
// system title bar and tells the page what to draw through one global, written by an initialization script before the page
// runs: `window.__HW_FRAME__ = {"os":"windows","style":"flat","side":"right","buttons":[...]}` (app/src-tauri/src/windowframe.rs).
// The page treats that global as untrusted input (`readFrame`): anything that is not exactly that shape is ignored and nothing
// is drawn, so a browser, macOS, Android and a window with the system frame keep the bar as it was. Dragging, the double click
// that maximises and the resize edges come from the window runtime, not from here; the page draws no edge, never listens to the
// close request of the window (a listener in the page would keep the window from closing) and never changes the frame of its
// window. Elements are made with `el` and named by properties; nothing is parsed as markup.
import { backend, call } from "./bridge.js";
import { el, loadStyles } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { BUTTONS, maximizeRow, readFrame, windowState, type ButtonRow, type WindowState } from "./windowframestate.js";

/** The only window commands a press may send: a row of the table that names anything else is not sent. */
const PRESS: ReadonlySet<string> = new Set(["plugin:window|minimize", "plugin:window|toggle_maximize", "plugin:window|close"]);

/** Put the words of a row on a button: the name is the title and the accessible name, the glyph word selects the drawing. */
function show(button: HTMLButtonElement, row: ButtonRow): void {
  button.title = row.name;
  button.setAttribute("aria-label", row.name);
  const glyph = button.querySelector<HTMLElement>(".wf-glyph");
  if (glyph !== null) glyph.dataset.glyph = row.glyph;
}

/**
 * Draw the window buttons in `bar` when the shell asked for an own frame. Returns once the style sheet is there, so the first
 * measurement of the page already includes the group. Does nothing in a browser or when the facts are not the shell's.
 */
export async function initWindowFrame(bar: HTMLElement): Promise<void> {
  if (backend !== "tauri") return;
  const facts = readFrame((window as unknown as { __HW_FRAME__?: unknown }).__HW_FRAME__);
  if (facts === null) return;

  const root = document.documentElement;
  root.classList.add("frame-own", "frame-flat", "frame-right");
  // Not a drag region: a press on a button is a press on the button.
  const group = el("div", "bar-window", bar);
  const buttons = new Map<string, HTMLButtonElement>();
  let state: WindowState = { maximized: false, fullscreen: false };
  let counter = 0; // the number of the newest question: an older answer arriving later is dropped
  let frame = 0; // the pending animation frame of a resize, 0 when none

  /** Draw what the window is: the maximise button, the group's mark and the full screen rule. */
  const apply = (): void => {
    group.toggleAttribute("data-maximized", state.maximized);
    const maximize = buttons.get("maximize");
    if (maximize !== undefined) show(maximize, maximizeRow(state.maximized));
    group.hidden = state.fullscreen; // in full screen the system draws no bar and the group is not drawn
  };

  /** Ask the window for its state; only the newest answer is applied, and a failed read keeps what was shown. */
  const refresh = async (): Promise<void> => {
    counter += 1;
    const mine = counter;
    const next = await windowState((command) => call(command), state);
    if (mine !== counter) return;
    state = next;
    apply();
  };

  for (const row of BUTTONS) {
    if (!facts.buttons.includes(row.word)) continue;
    const button = el("button", "wf-btn", group);
    button.type = "button";
    button.dataset.button = row.word;
    const glyph = el("span", "wf-glyph", button);
    glyph.setAttribute("aria-hidden", "true");
    show(button, row);
    buttons.set(row.word, button);
    button.addEventListener("click", () => {
      const command = `plugin:window|${row.press}`;
      if (!PRESS.has(command)) return;
      // The runtime acts on the window it is called from; a refusal leaves the window as it is.
      const sent = call(command).catch(() => {});
      // The window has changed by the time the command is answered: ask for the state it is in now.
      if (row.word === "maximize") void sent.then(refresh);
      focusInput(); // a bar button must not keep the keyboard
    });
  }

  // One question per frame, however many resize events come: a drag of the window edge is a burst.
  const abort = new AbortController();
  const cancel = (): void => {
    if (frame !== 0) cancelAnimationFrame(frame);
    frame = 0;
  };
  window.addEventListener(
    "resize",
    () => {
      if (frame !== 0) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        void refresh();
      });
    },
    { signal: abort.signal },
  );
  document.addEventListener("visibilitychange", () => (document.hidden ? cancel() : void refresh()), { signal: abort.signal });
  window.addEventListener(
    "pagehide",
    () => {
      cancel();
      abort.abort();
    },
    { once: true },
  );
  apply();
  void refresh();

  await loadStyles("./windowframe.css").catch(() => {
    // the buttons still work without their style sheet
  });
}
