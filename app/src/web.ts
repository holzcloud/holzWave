// The browser build's own page code. `initWeb` is called once by `main.ts`, only when the page runs against a server (the
// web backend), after the listeners are in place and before the first layout is asked for. It is the one place where the
// modules that exist only in a browser are started: add their start-up call to `initWeb` below, in this file, and nowhere
// else, so the desktop page never loads any of them.
//
// Full screen and the keyboard lock live here. A browser keeps some chords for itself (close tab, new tab, new window, the
// tab switchers); in full screen a browser that has `navigator.keyboard.lock` can be asked to hand them to the page. The
// page never enters full screen by itself: only the palette entry `Toggle full screen`, run by a person, does. The lock is
// requested only after full screen was granted and released on every way out (the entry run again, the person holding
// Escape, the browser leaving), so keys are never held outside full screen.
import { registerAction, setAvailability } from "./actions.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";
import { initNotice } from "./webnotice.js";
import { DESKTOP_ONLY_ACTIONS, FULLSCREEN_NOTICE, LOCK_KEYS, canLockKeys } from "./webstate.js";

/** The part of `navigator.keyboard` the page uses (Chromium only). */
interface KeyboardLock {
  lock(codes: string[]): Promise<void>;
  unlock(): void;
}

const lockOf = (): KeyboardLock | null => (canLockKeys(navigator) ? (navigator as unknown as { keyboard: KeyboardLock }).keyboard : null);

/** A lock was requested for the current stay in full screen and has not been released. */
let locked = false;

/** Counts the stays in full screen, so that a lock answer that arrives late knows it belongs to a stay that is over. */
let stay = 0;

/** Let the keys go, once: the lock of this stay is over. A page that holds none does nothing. */
function releaseKeys(): void {
  stay++;
  if (!locked) return;
  locked = false;
  try {
    lockOf()?.unlock();
  } catch {
    // nothing to release: the browser dropped the lock itself
  }
}

/** Ask the browser to pass the locked keys to the page. Full screen is on; a refusal is silent and a late grant is undone. */
async function lockKeys(): Promise<void> {
  const keyboard = lockOf();
  if (keyboard === null) return; // a browser without a lock keeps its chords and the page says nothing about it
  const mine = ++stay;
  locked = true;
  try {
    await keyboard.lock([...LOCK_KEYS]);
  } catch {
    return; // the browser did not grant it
  }
  if (document.fullscreenElement === null) {
    // The page left full screen while the answer was on its way: the grant is of no use any more and is undone at once.
    locked = false;
    try {
      keyboard.unlock();
    } catch {
      // nothing to release
    }
    return;
  }
  if (mine !== stay) return; // another stay began meanwhile: its own request speaks for it
  showNotice(FULLSCREEN_NOTICE);
}

/** Enter full screen from the press that runs the action, then lock the keys if the browser granted it. */
function enter(): void {
  if (typeof document.documentElement.requestFullscreen !== "function") return;
  try {
    document.documentElement.requestFullscreen().then(
      () => {
        if (document.fullscreenElement !== null) void lockKeys();
      },
      () => {}, // the browser did not allow full screen: nothing to lock
    );
  } catch {
    // an engine that throws instead of rejecting: same as a refusal
  }
}

/** Leave full screen; the `fullscreenchange` that follows releases the keys. */
function leave(): void {
  try {
    void document.exitFullscreen().catch(() => {});
  } catch {
    // already out
  }
}

let started = false;

/** Start the browser-only code of the page. Called once, by `main.ts`, on the web backend only. */
export function initWeb(): void {
  if (started) return;
  started = true;
  registerAction({
    id: "window.fullscreen",
    title: "Toggle full screen",
    run: () => {
      if (document.fullscreenElement !== null) leave();
      else enter();
    },
  });
  // Every way out of full screen, including the browser's own (Escape held), lets the keys go.
  document.addEventListener("fullscreenchange", () => {
    if (document.fullscreenElement === null) releaseKeys();
  });
  // The controls that need a window of the desktop shell are not offered by the palette; run by a stored shortcut or a script
  // they answer in words (dragdrop.ts) and call nothing. The test asks the platform each time, like every web branch of the page.
  for (const id of DESKTOP_ONLY_ACTIONS) setAvailability(id, () => state.os !== "web");
  // The tab marks: the title and the icon while the page is hidden.
  void initNotice().catch(() => {});
  // The later browser-only modules are started here.
}
