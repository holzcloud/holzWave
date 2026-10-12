// The Tauri bridge: no @tauri-apps/api, the global is enough for the commands and the events.
import type { View } from "./types.js";

/** The options of `listen` that the page uses: `target` names the window whose events are wanted. */
interface ListenOptions {
  target?: string;
}

interface TauriGlobal {
  core: { invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T>(name: string, cb: (e: { payload: T }) => void, options?: ListenOptions): Promise<() => void> };
}

export const tauri = (window as unknown as { __TAURI__: TauriGlobal }).__TAURI__;

/**
 * The label of this window (`main`, `window-2`, ...), as the shell gave it. Tauri puts it into
 * `__TAURI_INTERNALS__.metadata.currentWindow.label`; read once, it does not change. Empty when there is no such
 * metadata (a page outside the app), and then events are not narrowed.
 */
export const windowLabel: string = (() => {
  const internals = (window as unknown as { __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: unknown } } } })
    .__TAURI_INTERNALS__;
  const label = internals?.metadata?.currentWindow?.label;
  return typeof label === "string" ? label : "";
})();

/** Invoke a core command. Most layout commands answer with the new view, hence the default type. */
export const call = <T = View>(cmd: string, args?: Record<string, unknown>) => tauri.core.invoke<T>(cmd, args);

/**
 * Subscribe to a core event; the callback gets the payload. Resolves to the unlisten function.
 *
 * A listener without a target hears the events of every window (Tauri delivers an event addressed to one window to
 * all listeners that did not name a window), so each window draws the frames of every other window. The page names
 * its own label: it then gets what is addressed to it and what goes to all windows (`settings`, `exit`).
 */
export const listen = <T>(name: string, cb: (payload: T) => void) =>
  tauri.event.listen<T>(name, (e) => cb(e.payload), windowLabel ? { target: windowLabel } : undefined);
