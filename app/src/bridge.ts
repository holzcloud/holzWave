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
  // The one request function of the page: same-origin paths only (see webhttp.ts), and the desktop build never calls it.
  send: (url, init) => fetch(url, init as RequestInit),
  origin: location.origin,
});

/** The listeners the Tauri backend holds, by event name: the same diagnostics the web transport gives. */
const tauriListeners = new Map<string, number>();

/** Invoke a core command. Most layout commands answer with the new view, hence the default type. */
export const call = <T = View>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  tauri ? tauri.core.invoke<T>(cmd, args) : web.call<T>(cmd, args);

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
