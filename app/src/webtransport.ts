// The web transport: the page talks to its server over one socket instead of the Tauri global. This file is pure: it
// holds no window, no document, no global timer and no network function. Everything it touches is handed in through
// `TransportDeps`, so the unit tests drive it with a fake socket and a manual clock, and `bridge.ts` is the only file
// that names the real ones.
//
// Wire (one JSON text frame each):
//   page -> server   { "id": 7, "cmd": "split", "args": { ... } }
//   server -> page   { "id": 7, "ok": <result> }  or  { "id": 7, "err": <error> }
//                    { "event": "layout", "payload": <payload> }
//   server -> page   one binary frame: 8 bytes of id (big endian), then the bytes; it answers the call of that id
//
// Everything the server sends is untrusted. A frame that does not fit the shapes above is dropped without a throw and
// without any effect (it still counts as a sign of life). The page never queues a command for a later connection: a key
// pressed while the connection was down must not run when it returns.

/** The part of a socket the transport uses; the browser's socket has all of it. */
export interface SocketLike {
  readyState: number;
  binaryType: string;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code: number }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}

/** What the transport needs from its surroundings. */
export interface TransportDeps {
  /** Open a new socket to the server. */
  open(): SocketLike;
  /** Milliseconds, any origin. */
  now(): number;
  /** Run `fn` once after `ms` milliseconds. */
  setTimer(fn: () => void, ms: number): unknown;
  /** Cancel a timer made by `setTimer`. */
  clearTimer(handle: unknown): void;
  /** A number in [0, 1). */
  random(): number;
}

/** The state a connection callback hears. */
export type ConnectionState = "connecting" | "open" | "reconnecting" | "ended";

/** What `onConnection` callbacks get. */
export interface ConnectionInfo {
  state: ConnectionState;
  /** The number of the attempt, on `reconnecting`; 0 otherwise. */
  attempt: number;
  /** True on an `open` after the page had been connected before. */
  resumed: boolean;
  /** The close code on `ended`; 0 otherwise. */
  code: number;
}

/** What `onSession` callbacks get: the payload of the web-only event `hw-session`. */
export interface SessionInfo {
  state: string;
  seconds?: number;
}

/** The transport the bridge builds. */
export interface Transport {
  call<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(name: string, cb: (payload: T) => void): () => void;
  onConnection(cb: (info: ConnectionInfo) => void): () => void;
  onSession(cb: (info: SessionInfo) => void): () => void;
  listenerCount(name: string): number;
}

/** The longest message the page accepts from the server, in bytes; a longer one closes the socket (16 MiB). */
export const MAX_MESSAGE_BYTES = 16 * 1024 * 1024;
/** The longest request the page sends, in bytes of the JSON text: 8 MiB for a paste or an upload chunk plus 4 KiB of frame. */
export const MAX_REQUEST_BYTES = 8 * 1024 * 1024 + 4 * 1024;
/** The longest event name the page accepts, in bytes of UTF-8. */
export const MAX_EVENT_NAME_BYTES = 64;
/** How long the calls of the page's start wait for the first connection (15 s). */
export const FIRST_CONNECT_MS = 15_000;
/** Silence after which the connection counts as dead (90 s); the server sends a session event at least every 30 s. */
export const SILENCE_MS = 90_000;
/** The delay before the first re-connection attempt (0.5 s); it doubles with every attempt. */
export const BACKOFF_FIRST_MS = 500;
/** The longest delay between two attempts (30 s). */
export const BACKOFF_MAX_MS = 30_000;
/** The share of the delay that is random, to either side (20 percent), so many pages do not return together. */
export const JITTER = 0.2;
/** The close code of the server for a session that is invalid or has ended: the connection ends, no retry. */
export const CLOSE_SESSION_ENDED = 4401;
/** The close code of the server for a refused origin or host: the connection ends, no retry. */
export const CLOSE_REFUSED = 4403;
/** The close code the page uses for a message that is too big. */
export const CLOSE_TOO_BIG = 1009;

/** The address of the server socket for a page: the same host, the path `/ws`, `wss` for https and `ws` for http. */
export function socketUrl(protocol: string, host: string): string {
  if (host === "" || /[\s/\\@?#]/.test(host)) throw new Error("the page has no usable host");
  if (protocol === "https:") return `wss://${host}/ws`;
  if (protocol === "http:") return `ws://${host}/ws`;
  throw new Error(`no server socket for a page of the kind ${protocol}`);
}

const OPEN = 1;
const MAX_SAFE_HIGH_WORD = 0x1fffff;

/** The web-only event of the server about the session; it goes to `onSession` and never to `listen`. */
const SESSION_EVENT = "hw-session";

/** The number of bytes `text` takes in UTF-8, without making the bytes. */
function utf8Length(text: string): number {
  let bytes = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c < 0x80) bytes += 1;
    else if (c < 0x800) bytes += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < text.length && (text.charCodeAt(i + 1) & 0xfc00) === 0xdc00) {
      bytes += 4;
      i++;
    } else bytes += 3;
  }
  return bytes;
}

/** Whether `text` is longer than `limit` bytes in UTF-8; the exact count is made only where it can matter. */
function overBytes(text: string, limit: number): boolean {
  if (text.length > limit) return true; // one UTF-16 unit is at least one byte
  if (text.length * 3 <= limit) return false; // and at most three
  return utf8Length(text) > limit;
}

const endedMessage = (code: number) => (code === CLOSE_SESSION_ENDED ? "the session has ended" : "the server refused this page");

interface Waiter {
  id: number;
  text: string;
  resolve(value: unknown): void;
  reject(reason: unknown): void;
}

/** Build a transport over `deps`. Nothing is opened until the first `call` or `listen`. */
export function createTransport(deps: TransportDeps): Transport {
  let socket: SocketLike | null = null;
  let started = false;
  let startedAt = 0;
  let everOpened = false;
  let endedWith = 0;
  let attempt = 0;
  let nextId = 1;
  let lastMessageAt = 0;
  let firstTimer: unknown;
  let silenceTimer: unknown;
  const pending = new Map<number, Waiter>();
  const startWaiters: Waiter[] = [];
  const listeners = new Map<string, Array<{ cb: (payload: never) => void }>>();
  const connectionCallbacks: Array<(info: ConnectionInfo) => void> = [];
  const sessionCallbacks: Array<(info: SessionInfo) => void> = [];

  /** Run a callback of the page; an error of it goes to the page's error reporting from a timer and stops nothing here. */
  const safely = (fn: () => void) => {
    try {
      fn();
    } catch (e) {
      deps.setTimer(() => {
        throw e;
      }, 0);
    }
  };

  const announce = (info: ConnectionInfo) => {
    for (const cb of [...connectionCallbacks]) safely(() => cb(info));
  };

  const cancel = (handle: unknown) => {
    if (handle !== undefined) deps.clearTimer(handle);
  };

  const rejectWaiters = (reason: string) => {
    for (const w of startWaiters.splice(0)) w.reject(reason);
  };

  /** The delay before attempt `n` (1, 2, ...): 0.5 s doubling to 30 s, 20 percent either way. */
  const backoff = (n: number) => {
    const base = Math.min(BACKOFF_MAX_MS, BACKOFF_FIRST_MS * 2 ** Math.min(n - 1, 30));
    return base * (1 + JITTER * (2 * deps.random() - 1));
  };

  const detach = (s: SocketLike) => {
    s.onopen = null;
    s.onmessage = null;
    s.onclose = null;
    s.onerror = null;
  };

  /** The socket is gone (closed by the server, by us, or never opened): settle what depended on it and plan the next step. */
  const lost = (code: number) => {
    socket = null;
    cancel(silenceTimer);
    silenceTimer = undefined;
    for (const w of [...pending.values()]) w.reject("connection lost");
    pending.clear();
    if (code === CLOSE_SESSION_ENDED || code === CLOSE_REFUSED) {
      endedWith = code;
      cancel(firstTimer);
      firstTimer = undefined;
      rejectWaiters(endedMessage(code));
      announce({ state: "ended", attempt: 0, resumed: false, code });
      return;
    }
    attempt += 1;
    announce({ state: "reconnecting", attempt, resumed: false, code: 0 });
    deps.setTimer(openSocket, backoff(attempt));
  };

  /** Give up a socket that is silent or sends too much: close it, do not wait for its answer, and go on as for a lost one. */
  const abandon = (s: SocketLike, code: number) => {
    detach(s);
    try {
      if (code === CLOSE_TOO_BIG) s.close(CLOSE_TOO_BIG);
      else s.close();
    } catch {
      try {
        s.close(); // a browser only lets a page close with 1000 or 3000 to 4999
      } catch {
        // already closed
      }
    }
    lost(code);
  };

  const armSilence = (ms: number) => {
    silenceTimer = deps.setTimer(() => {
      silenceTimer = undefined;
      const quiet = deps.now() - lastMessageAt;
      const s = socket;
      if (!s) return;
      if (quiet >= SILENCE_MS) abandon(s, 1006);
      else armSilence(SILENCE_MS - quiet);
    }, ms);
  };

  const transmit = (s: SocketLike, w: Waiter) => {
    pending.set(w.id, w);
    try {
      s.send(w.text);
    } catch {
      pending.delete(w.id);
      w.reject("connection lost");
    }
  };

  const onOpen = (s: SocketLike) => {
    const resumed = everOpened;
    everOpened = true;
    attempt = 0;
    cancel(firstTimer);
    firstTimer = undefined;
    lastMessageAt = deps.now();
    armSilence(SILENCE_MS);
    for (const w of startWaiters.splice(0)) transmit(s, w);
    announce({ state: "open", attempt: 0, resumed, code: 0 });
  };

  const routeEvent = (f: Record<string, unknown>) => {
    const name = f.event;
    if (typeof name !== "string" || name === "" || overBytes(name, MAX_EVENT_NAME_BYTES)) return;
    if (!Object.hasOwn(f, "payload")) return;
    const payload = f.payload;
    if (name === SESSION_EVENT) {
      for (const cb of [...sessionCallbacks]) safely(() => cb(payload as SessionInfo));
      return;
    }
    for (const entry of [...(listeners.get(name) ?? [])]) safely(() => entry.cb(payload as never));
  };

  const settleText = (f: Record<string, unknown>) => {
    const id = f.id;
    if (typeof id !== "number" || !Number.isSafeInteger(id) || id < 1) return;
    const w = pending.get(id);
    if (!w) return;
    const hasOk = Object.hasOwn(f, "ok");
    if (hasOk === Object.hasOwn(f, "err")) return; // both, or neither
    pending.delete(id);
    if (hasOk) w.resolve(f.ok ?? null);
    else w.reject(f.err);
  };

  const onText = (text: string) => {
    let frame: unknown;
    try {
      frame = JSON.parse(text);
    } catch {
      return;
    }
    if (typeof frame !== "object" || frame === null || Array.isArray(frame)) return;
    const f = frame as Record<string, unknown>;
    if (Object.hasOwn(f, "event")) {
      if (!Object.hasOwn(f, "id")) routeEvent(f);
    } else if (Object.hasOwn(f, "id")) {
      settleText(f);
    }
  };

  const onBinary = (data: ArrayBuffer) => {
    if (data.byteLength < 9) return;
    const view = new DataView(data);
    const high = view.getUint32(0, false);
    if (high > MAX_SAFE_HIGH_WORD) return;
    const id = high * 0x1_0000_0000 + view.getUint32(4, false);
    if (id < 1) return;
    const w = pending.get(id);
    if (!w) return;
    pending.delete(id);
    w.resolve(data.slice(8));
  };

  const onMessage = (s: SocketLike, data: unknown) => {
    lastMessageAt = deps.now();
    if (typeof data === "string") {
      if (overBytes(data, MAX_MESSAGE_BYTES)) abandon(s, CLOSE_TOO_BIG);
      else onText(data);
    } else if (data instanceof ArrayBuffer) {
      if (data.byteLength > MAX_MESSAGE_BYTES) abandon(s, CLOSE_TOO_BIG);
      else onBinary(data);
    }
  };

  const openSocket = () => {
    let s: SocketLike;
    try {
      s = deps.open();
    } catch {
      lost(1006);
      return;
    }
    socket = s;
    s.binaryType = "arraybuffer";
    s.onopen = () => {
      if (socket === s) onOpen(s);
    };
    s.onmessage = (ev) => {
      if (socket === s) onMessage(s, ev.data);
    };
    s.onclose = (ev) => {
      if (socket !== s) return;
      detach(s);
      lost(typeof ev?.code === "number" ? ev.code : 1006);
    };
    s.onerror = () => {}; // the close event follows and carries the code
  };

  const start = () => {
    if (started) return;
    started = true;
    startedAt = deps.now();
    announce({ state: "connecting", attempt: 0, resumed: false, code: 0 });
    firstTimer = deps.setTimer(() => {
      firstTimer = undefined;
      if (!everOpened) rejectWaiters("cannot reach the server");
    }, FIRST_CONNECT_MS);
    openSocket();
  };

  const remove = <T>(list: T[], item: T) => {
    const at = list.indexOf(item);
    if (at >= 0) list.splice(at, 1);
  };

  return {
    call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
      return new Promise<T>((resolve, reject) => {
        if (endedWith) {
          reject(endedMessage(endedWith));
          return;
        }
        start();
        const id = nextId++;
        let text: string;
        try {
          text = JSON.stringify({ id, cmd, args: args ?? {} });
        } catch (e) {
          reject(e instanceof Error ? e.message : "the request cannot be sent");
          return;
        }
        if (overBytes(text, MAX_REQUEST_BYTES)) {
          reject("the request is too large");
          return;
        }
        const w: Waiter = { id, text, resolve: resolve as (v: unknown) => void, reject };
        if (socket && socket.readyState === OPEN) transmit(socket, w);
        else if (!everOpened && deps.now() - startedAt < FIRST_CONNECT_MS) startWaiters.push(w);
        else reject("not connected to the server");
      });
    },
    listen<T>(name: string, cb: (payload: T) => void): () => void {
      start();
      const entry = { cb: cb as (payload: never) => void };
      const list = listeners.get(name) ?? [];
      list.push(entry);
      listeners.set(name, list);
      return () => {
        const now = listeners.get(name);
        if (now) remove(now, entry);
      };
    },
    onConnection(cb) {
      connectionCallbacks.push(cb);
      return () => remove(connectionCallbacks, cb);
    },
    onSession(cb) {
      sessionCallbacks.push(cb);
      return () => remove(sessionCallbacks, cb);
    },
    listenerCount(name) {
      return listeners.get(name)?.length ?? 0;
    },
  };
}
