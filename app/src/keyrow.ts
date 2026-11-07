// The key row of a phone or a tablet (docs/design.md section 6.6): `esc tab ctrl alt ↑ ↓` above the on-screen keyboard, with `ctrl` and
// `alt` sticky. A phone's keyboard has none of these keys and a terminal cannot do without them. This module draws the row and
// turns taps into keys; what a key carries (the sticky modifiers, one printable character from the soft keyboard, the source of the
// keys) is decided in keyrowstate.ts and kept by keyboard.ts, so a key of the row, a key of a hardware keyboard and a character
// of the soft keyboard all take the armed modifier through one place.
//
// On a desktop and in a browser this module does nothing and creates nothing: only the platform `android` takes a touch layout.
// Built to what Android keyboards are known to do and not run on a real one (see keyboard.ts).
import { el, loadStyles, setAttr } from "./dom.js";
import {
  focusInput,
  hardwareKeyboardNow,
  onTouchInputChange,
  sendKeyPayload,
  setTouchInput,
  stickyNow,
  takeStickyMods,
  tapSticky,
} from "./keyboard.js";
import type { KeyPayload } from "./keymap.js";
import type { Layout } from "./layoutclass.js";
import { type Modifier, type PlainName, keyEvents, plainKey, rowVisible } from "./keyrowstate.js";
import { layoutNow } from "./phone.js";
import { fit } from "./scene.js";
import { state } from "./state.js";

type KeyName = PlainName | Modifier;

/** The row in the order of the design: label and the name a screen reader gets. */
const KEYS: readonly { name: KeyName; label: string; aria: string }[] = [
  { name: "esc", label: "esc", aria: "Escape" },
  { name: "tab", label: "tab", aria: "Tab" },
  { name: "ctrl", label: "ctrl", aria: "Control" },
  { name: "alt", label: "alt", aria: "Alt" },
  { name: "up", label: "↑", aria: "Arrow up" },
  { name: "down", label: "↓", aria: "Arrow down" },
];

const isSticky = (name: KeyName): name is Modifier => name === "ctrl" || name === "alt";

/** What `build` made. */
interface Row {
  root: HTMLElement;
  keys: Map<KeyName, HTMLButtonElement>;
}

let row: Row | null = null;
let unwatch: (() => void) | null = null;

/** The release of every plain key that is held, by pointer: it goes out when the finger lifts or the touch is cancelled. */
const held = new Map<number, { pane: number; release: KeyPayload }>();

// ---- the elements ------------------------------------------------------------------------------------

/** Press a plain key: its press goes out now, with the sticky modifiers, and its release when the pointer ends. */
function pressPlain(name: PlainName, pointer: number | null): void {
  const pane = state.view?.focus;
  if (pane === undefined) return;
  const k = plainKey(name);
  const { press, release } = keyEvents({ ...k, mods: takeStickyMods(k.mods) });
  sendKeyPayload(pane, press);
  if (pointer === null) sendKeyPayload(pane, release);
  else held.set(pointer, { pane, release });
}

function releasePointer(pointer: number): void {
  const h = held.get(pointer);
  if (!h) return;
  held.delete(pointer);
  sendKeyPayload(h.pane, h.release);
}

/** A tap must not take the keyboard from the terminal input: it would close the on-screen keyboard. */
function keepFocus(): void {
  if (document.activeElement !== document.getElementById("ime")) focusInput();
}

function build(): Row {
  const root = el("div");
  root.id = "keyrow";
  root.setAttribute("role", "toolbar");
  root.setAttribute("aria-label", "Keys");
  root.hidden = true;
  const keys = new Map<KeyName, HTMLButtonElement>();
  for (const { name, label, aria } of KEYS) {
    const b = el("button", "keyrow-key", root);
    b.type = "button";
    b.dataset.key = name;
    b.setAttribute("aria-label", aria);
    el("span", "keyrow-label", b).textContent = label;
    if (isSticky(name)) {
      b.dataset.state = "idle";
      b.setAttribute("aria-pressed", "false");
    }
    // The tap is the pointer's press, not the click: a keyboard that stays open and a key that is down while the finger is.
    // `mousedown` is cancelled as well, because the focus moves on it in some engines even when `pointerdown` was cancelled.
    b.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      if (isSticky(name)) {
        tapSticky(name, e.timeStamp);
      } else {
        try {
          b.setPointerCapture(e.pointerId); // the release must reach this key even if the finger slides off it
        } catch {
          // a pointer that no longer exists: the release then comes from the cancel below
        }
        pressPlain(name, e.pointerId);
      }
      keepFocus();
    });
    b.addEventListener("pointerup", (e) => releasePointer(e.pointerId));
    b.addEventListener("pointercancel", (e) => releasePointer(e.pointerId));
    b.addEventListener("lostpointercapture", (e) => releasePointer(e.pointerId));
    b.addEventListener("mousedown", (e) => e.preventDefault());
    // A click with no pointer behind it (`detail` 0) is a screen reader's or a keyboard's activation: the same action, whole.
    b.addEventListener("click", (e) => {
      if (e.detail !== 0) return;
      if (isSticky(name)) tapSticky(name, e.timeStamp);
      else pressPlain(name, null);
      keepFocus();
    });
    keys.set(name, b);
  }
  return { root, keys };
}

/** Put the row where the design has it: directly above the bottom bar on the phone, directly above the status line on the tablet.
 *  The row is moved in the document, not only drawn there with `order`, so the reading and tab order is the order on the screen. */
function place(layout: Layout): void {
  const app = document.getElementById("app");
  if (!app || !row) return;
  const nav = document.getElementById("phone-nav");
  if (layout === "phone" && nav) {
    if (row.root.parentElement !== app || row.root.nextElementSibling !== nav) app.insertBefore(row.root, nav);
  } else {
    const status = document.getElementById("status");
    if (status && status.parentElement === app) {
      if (row.root.parentElement !== app || row.root.nextElementSibling !== status) app.insertBefore(row.root, status);
    } else if (row.root.parentElement !== app || row.root !== app.lastElementChild) {
      app.append(row.root);
    }
  }
}

// ---- drawing -----------------------------------------------------------------------------------------

/** Show or hide the row, and draw the state of the sticky keys. */
function refresh(): void {
  if (!row) return;
  const sticky = stickyNow();
  for (const name of ["ctrl", "alt"] as const) {
    const b = row.keys.get(name) as HTMLButtonElement;
    const mode = sticky[name].mode;
    setAttr(b, "data-state", mode);
    setAttr(b, "aria-pressed", mode === "idle" ? "false" : "true");
    const aria = (KEYS.find((k) => k.name === name) as { aria: string }).aria;
    setAttr(b, "aria-label", mode === "idle" ? aria : `${aria}, ${mode}`);
    // The filled square of a locked key is an element of its own, so it is there for a test and for a screen reader's tree to skip.
    let mark = b.querySelector(".keyrow-mark");
    if (mode === "locked" && !mark) {
      mark = el("span", "keyrow-mark");
      mark.setAttribute("aria-hidden", "true");
      b.prepend(mark);
    } else if (mode !== "locked" && mark) {
      mark.remove();
    }
  }
  const visible = rowVisible({
    layout: layoutNow(),
    imeFocused: document.activeElement === document.getElementById("ime"),
    hardware: hardwareKeyboardNow(),
  });
  if (row.root.hidden === visible) {
    row.root.hidden = !visible;
    measureAgain(); // the stage became taller or shorter: the panes and the shells must know
  }
}

/** The focus moves in two steps (the terminal input gives it up and takes it back): look once it has settled. */
function refreshSoon(): void {
  setTimeout(refresh, 0);
}

let measuring = false;

/** Measure the stage again, at most once per frame; nothing is measured before the page has a view. */
function measureAgain(): void {
  if (measuring) return;
  measuring = true;
  requestAnimationFrame(() => {
    measuring = false;
    if (state.view) fit();
  });
}

// ---- the visual viewport -------------------------------------------------------------------------------

/** Follow the on-screen keyboard: the height of the visual viewport is the page's height, so the row sits above the keyboard. */
function onViewport(): void {
  const vv = window.visualViewport;
  if (!vv) return;
  const height = `${Math.round(vv.height)}px`;
  const root = document.documentElement;
  if (root.style.getPropertyValue("--hw-vvh") === height) return;
  root.style.setProperty("--hw-vvh", height);
  measureAgain();
}

let viewportOn = false;

function watchViewport(on: boolean): void {
  const vv = window.visualViewport;
  if (!vv || on === viewportOn) return;
  viewportOn = on;
  if (on) {
    vv.addEventListener("resize", onViewport);
    vv.addEventListener("scroll", onViewport);
    onViewport();
  } else {
    vv.removeEventListener("resize", onViewport);
    vv.removeEventListener("scroll", onViewport);
    document.documentElement.style.removeProperty("--hw-vvh");
    measureAgain();
  }
}

// ---- the layout class ----------------------------------------------------------------------------------

/** Take the row away and everything it set: the listeners, the variable, a locked modifier. */
function teardown(): void {
  for (const pointer of [...held.keys()]) releasePointer(pointer);
  setTouchInput(false);
  watchViewport(false);
  unwatch?.();
  unwatch = null;
  document.removeEventListener("focusin", refreshSoon);
  document.removeEventListener("focusout", refreshSoon);
  if (row) {
    row.root.remove();
    row = null;
    measureAgain();
  }
}

async function follow(next: Layout): Promise<void> {
  if (next === "desktop") {
    teardown();
    return;
  }
  setTouchInput(true);
  await loadStyles("./keyrow.css").catch(() => {});
  if (layoutNow() !== next) return; // the window changed again while the stylesheet loaded; that call has the last word
  if (!row) {
    row = build();
    unwatch = onTouchInputChange(refresh);
    document.addEventListener("focusin", refreshSoon);
    document.addEventListener("focusout", refreshSoon);
  }
  place(next);
  watchViewport(true);
  refresh();
}

/**
 * Follow the layout class of the page (the `hw:layout` event and `layoutNow`). Call once at start-up, from `initKeyboard`.
 * Nothing happens unless the platform is Android: no element, no listener, no variable and no stylesheet.
 */
export function initKeyRow(): void {
  if (state.os !== "android") return;
  document.addEventListener("hw:layout", () => void follow(layoutNow()));
  void follow(layoutNow());
}
