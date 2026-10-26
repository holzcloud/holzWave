// The web transport: the page talks to its server over one socket instead of the Tauri global. This file is pure: it
// holds no window, no document, no global timer and no network function. Everything it touches is handed in through
// `TransportDeps`, so the unit tests drive it with a fake socket and a manual clock, and `bridge.ts` is the only file
// that names the real ones.
//
// Wire (one JSON text frame each):
//   page -> server   { "id": 7, "cmd": "split", "args": { ... } }
//   server -> page   { "id": 7, "ok": <result> }  or  { "id": 7, "err": <error> }
//                    { "event": "layout", "payload": <payload> }

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
  /** True on an `open` after a lost connection. */
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

/** The address of the server socket for a page: the same host, the path `/ws`, `wss` for https and `ws` for http. */
export function socketUrl(protocol: string, host: string): string {
  if (host === "" || /[\s/\\@?#]/.test(host)) throw new Error("the page has no usable host");
  if (protocol === "https:") return `wss://${host}/ws`;
  if (protocol === "http:") return `ws://${host}/ws`;
  throw new Error(`no server socket for a page of the kind ${protocol}`);
}

const OPEN = 1;

/** The session event the server adds; it never reaches `listen`. */
const SESSION_EVENT = "hw-session";

type Waiter = { resolve(value: unknown): void; reject(reason: unknown): void };

/** Build a transport over `deps`. Nothing is opened until the first `call` or `listen`. */
export function createTransport(deps: TransportDeps): Transport {
  let socket: SocketLike | null = null;
  let nextId = 1;
  const pending = new Map<number, Waiter>();
  const listeners = new Map<string, Array<{ cb: (payload: never) => void }>>();
  const connectionCallbacks: Array<(info: ConnectionInfo) => void> = [];
  const sessionCallbacks: Array<(info: SessionInfo) => void> = [];
  const startWaiters: Array<() => void> = [];

  const announce = (info: ConnectionInfo) => {
    for (const cb of [...connectionCallbacks]) cb(info);
  };

  const connect = () => {
    if (socket) return;
    announce({ state: "connecting", attempt: 0, resumed: false, code: 0 });
    const s = deps.open();
    socket = s;
    s.binaryType = "arraybuffer";
    s.onopen = () => {
      announce({ state: "open", attempt: 0, resumed: false, code: 0 });
      for (const go of startWaiters.splice(0)) go();
    };
    s.onmessage = (ev) => dispatch(ev.data);
  };

  const dispatch = (data: unknown) => {
    if (typeof data !== "string") return;
    let frame: unknown;
    try {
      frame = JSON.parse(data);
    } catch {
      return;
    }
    if (typeof frame !== "object" || frame === null) return;
    const f = frame as Record<string, unknown>;
    if (typeof f.event === "string") {
      if (f.event === SESSION_EVENT) {
        for (const cb of [...sessionCallbacks]) cb(f.payload as SessionInfo);
        return;
      }
      for (const entry of [...(listeners.get(f.event) ?? [])]) entry.cb(f.payload as never);
      return;
    }
    if (typeof f.id === "number") {
      const waiter = pending.get(f.id);
      if (!waiter) return;
      pending.delete(f.id);
      if ("ok" in f) waiter.resolve(f.ok ?? null);
      else waiter.reject(f.err);
    }
  };

  const send = (id: number, cmd: string, args: Record<string, unknown> | undefined) => {
    socket?.send(JSON.stringify({ id, cmd, args: args ?? {} }));
  };

  return {
    call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
      connect();
      return new Promise<T>((resolve, reject) => {
        const id = nextId++;
        const go = () => {
          pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
          send(id, cmd, args);
        };
        if (socket?.readyState === OPEN) go();
        else startWaiters.push(go);
      });
    },
    listen<T>(name: string, cb: (payload: T) => void): () => void {
      connect();
      const entry = { cb: cb as (payload: never) => void };
      const list = listeners.get(name) ?? [];
      list.push(entry);
      listeners.set(name, list);
      return () => {
        const now = listeners.get(name);
        if (!now) return;
        const at = now.indexOf(entry);
        if (at >= 0) now.splice(at, 1);
      };
    },
    onConnection(cb) {
      connectionCallbacks.push(cb);
      return () => {
        const at = connectionCallbacks.indexOf(cb);
        if (at >= 0) connectionCallbacks.splice(at, 1);
      };
    },
    onSession(cb) {
      sessionCallbacks.push(cb);
      return () => {
        const at = sessionCallbacks.indexOf(cb);
        if (at >= 0) sessionCallbacks.splice(at, 1);
      };
    },
    listenerCount(name) {
      return listeners.get(name)?.length ?? 0;
    },
  };
}
