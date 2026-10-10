// The Tauri bridge: no @tauri-apps/api, the global is enough for the commands and the events.
import type { View } from "./types.js";

interface TauriGlobal {
  core: { invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T>(name: string, cb: (e: { payload: T }) => void): Promise<() => void> };
}

export const tauri = (window as unknown as { __TAURI__: TauriGlobal }).__TAURI__;

/** Invoke a core command. Most layout commands answer with the new view, hence the default type. */
export const call = <T = View>(cmd: string, args?: Record<string, unknown>) => tauri.core.invoke<T>(cmd, args);

/** Subscribe to a core event; the callback gets the payload. Resolves to the unlisten function. */
export const listen = <T>(name: string, cb: (payload: T) => void) =>
  tauri.event.listen<T>(name, (e) => cb(e.payload));
