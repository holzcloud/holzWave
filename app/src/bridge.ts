// The bridge between the page and the core. Every command and every event of the page goes through `call` and `listen`
// here, and this file is the one place that knows what stands behind them: the Tauri global of the desktop app (no
// @tauri-apps/api, the global is enough) or, in a browser without it, one socket to the server of the page's own host.
import type { View } from "./types.js";
import { createTransport, socketUrl } from "./webtransport.js";
import { createHttp, HttpError } from "./webhttp.js";
import type { ConnectionInfo, SessionInfo, SocketLike } from "./webtransport.js";

export type { ConnectionInfo, ConnectionState, SessionInfo } from "./webtransport.js";
export { HttpError };

/** The options of `listen` that the page uses: `target` names the window whose events are wanted. */
interface ListenOptions {
  target?: string;
}

interface TauriGlobal {
  core: { invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T>(name: string, cb: (e: { payload: T }) => void, options?: ListenOptions): Promise<() => void> };
}

const tauri = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__;

/** Which side of the bridge this page runs on: the desktop app (`tauri`) or a browser talking to a server (`web`). */
export const backend: "tauri" | "web" = tauri ? "tauri" : "web";

/**
 * The label of this window (`main`, `window-2`, ...). On the desktop the shell gives it: Tauri puts it into
 * `__TAURI_INTERNALS__.metadata.currentWindow.label`; read once, it does not change. It is empty when there is no such
 * metadata (a page outside the app), and then events are not narrowed. A browser page is always `main`.
 */
export const windowLabel: string = (() => {
  if (backend === "web") return "main";
  const internals = (window as unknown as { __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: unknown } } } })
    .__TAURI_INTERNALS__;
  const label = internals?.metadata?.currentWindow?.label;
  return typeof label === "string" ? label : "";
})();

/** The web backend: made at the first use, so a page that never calls or listens opens no socket. */
const web = createTransport({
  // The one socket of the page. The address comes from `location` only: the same host, `wss` for an https page.
  open: () => new WebSocket(socketUrl(location.protocol, location.host)) as unknown as SocketLike,
  now: () => Date.now(),
  setTimer: (fn, ms) => setTimeout(fn, ms),
  clearTimer: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  random: () => Math.random(),
});

const webHttp = createHttp({
  // The one request function of the page: paths on the page's origin only. The mode is `cors`, so the browser does not enforce that;
  // `resolve()` in webhttp.ts does. The desktop build never calls it.
  send: (url, init) => fetch(url, init as RequestInit),
  origin: location.origin,
});

/** The listeners the Tauri backend holds, by event name: the same diagnostics the web transport gives. */
const tauriListeners = new Map<string, number>();

const invoke = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  tauri ? tauri.core.invoke<T>(cmd, args) : web.call<T>(cmd, args);

/** A command that failed because the store of secrets is locked: what the hook gets to decide what happens next. */
export interface LockedCall {
  cmd: string;
  args: Record<string, unknown> | undefined;
  /** The failure, as the command made it. */
  error: unknown;
  /** The same command again, straight to the core: it does not pass the hook, so a second locked answer ends with its caller. */
  retry: () => Promise<unknown>;
}

/**
 * What the vault module sets on Android. It looks at the failure: for the locked text it answers with the retry's answer (or rejects
 * with the failure when the unlock is left); for any other failure it answers null and the failure goes on to the caller as it was.
 * The words of the locked text live in the vault module, so this file, which the login page also loads, imports nothing of it.
 */
type LockedHook = (call: LockedCall) => Promise<unknown> | null;

let lockedHook: LockedHook | null = null;

/** Set (or, with null, remove) the hook for a command that fails with the locked text. Only the vault module sets it, and only on Android. */
export function setLockedHook(hook: LockedHook | null): void {
  lockedHook = hook;
}

/** Commands that are sent for every key, wheel step or resize and never touch a secret: they do not wait on the hook. */
const HOT: ReadonlySet<string> = new Set(["key", "text", "wheel", "mouse", "resize", "paste", "send_edit", "request_full", "scroll"]);

/**
 * Invoke a core command. Most layout commands answer with the new view, hence the default type. Without a hook (every platform
 * but Android) the command's own promise is returned as it is; with one, a failure goes to the hook, which takes the ones that say the secrets are locked.
 */
export const call = <T = View>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
  const first = invoke<T>(cmd, args);
  if (lockedHook === null || HOT.has(cmd)) return first;
  return first.catch((error: unknown) => {
    const hook = lockedHook;
    const taken = hook === null || cmd.startsWith("vault_") ? null : hook({ cmd, args, error, retry: () => invoke<T>(cmd, args) });
    if (taken === null) throw error;
    return taken as Promise<T>;
  });
};

/**
 * Subscribe to a core event; the callback gets the payload. Resolves to the unlisten function.
 *
 * A listener without a target hears the events of every window (Tauri delivers an event addressed to one window to
 * all listeners that did not name a window), so each window draws the frames of every other window. The page names
 * its own label: it then gets what is addressed to it and what goes to all windows (`settings`, `exit`).
 */
export const listen = <T>(name: string, cb: (payload: T) => void): Promise<() => void> => {
  if (!tauri) return Promise.resolve(web.listen<T>(name, cb));
  return tauri.event
    .listen<T>(name, (e) => cb(e.payload), windowLabel ? { target: windowLabel } : undefined)
    .then((unlisten) => {
      tauriListeners.set(name, (tauriListeners.get(name) ?? 0) + 1);
      let gone = false;
      return () => {
        if (gone) return;
        gone = true;
        tauriListeners.set(name, Math.max(0, (tauriListeners.get(name) ?? 1) - 1));
        unlisten();
      };
    });
};

/** Hear the state of the connection to the server. Never fires on the desktop. Returns the function that stops it. */
export const onConnection = (cb: (info: ConnectionInfo) => void): (() => void) =>
  tauri ? () => {} : web.onConnection(cb);

/** Hear the session events of the server (`hw-session`). Never fires on the desktop. Returns the function that stops it. */
export const onSession = (cb: (info: SessionInfo) => void): (() => void) => (tauri ? () => {} : web.onSession(cb));

/** How many listeners an event has right now: diagnostics and the browser tests. */
export const listenerCount = (name: string): number => (tauri ? (tauriListeners.get(name) ?? 0) : web.listenerCount(name));

/**
 * Ask the page's own server over HTTP: `http("POST", "/api/login", { name, password })`. Only a path on the page's origin
 * is accepted; JSON in, parsed JSON out, `HttpError` for a failed answer. The desktop has no server, so there it fails.
 */
export const http = async (method: string, path: string, body?: unknown): Promise<unknown> => {
  if (tauri) throw new Error("http is for the web backend");
  return webHttp(method, path, body);
};
