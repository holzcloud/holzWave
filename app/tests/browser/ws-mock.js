// In-page stand-in for the server socket, injected after the mock core when a test runs on the web backend
// (`window.__HW_BACKEND__ === "ws"`, see lib.mjs). It replaces the browser's socket constructor by `FakeWebSocket`.
// A text frame the page sends is `{id, cmd, args}`; the fake calls `window.__MOCK__.invoke(cmd, args)`, the same
// function the Tauri global runs, and answers `{id, ok}` or `{id, err}` after a JSON round trip, so a value that does not
// survive JSON is lost here as it would be on the wire (an empty argument object is handed on as no arguments, which is
// what the page passed). An `ArrayBuffer` or typed array result goes back as the binary
// frame (8 bytes of id, big endian, then the bytes). Every `__MOCK__.emit` of a test arrives as an event frame
// `{event, payload}`. Frames reach the page in the same turn of the event loop that the Tauri mock would have used, so a
// test sees the same timing on both backends; only the opening of a socket takes one timer tick.
//
//   window.__WS_MOCK__
//     urls              every address the page opened, oldest first
//     sockets           every FakeWebSocket made, oldest first
//     sent              every text frame the page sent, oldest first, as the raw strings
//     push(name, payload, wanted)  send one event frame to every open socket when `wanted({ target })` accepts the page;
//                       answers (a promise) how many listeners the page has for `name`, 0 when nothing was sent. This is what
//                       `__MOCK__.emit` and `emitTo` call, so on this backend they answer a promise of the number
//     drop(code)        the server closes every open socket with `code` (1006 for a lost connection, 4401 or 4403 for the
//                       codes that end it)
//     refuseOpen(n)     the next `n` sockets fail to open and close with 1006
//     hold()            sockets stay connecting until `release()`
//     release()         open the sockets that `hold` kept waiting
//     silence(on)       while on (default true) the server sends nothing at all, answers included: a dead connection
//     reset()           forget the history and every control (the sockets stay as they are)
(() => {
  const mock = window.__MOCK__;
  const CONNECTING = 0;
  const OPEN = 1;
  const CLOSING = 2;
  const CLOSED = 3;

  const state = { urls: [], sockets: [], sent: [], refuse: 0, held: false, silent: false, waiting: [] };

  const be64 = (id) => {
    const bytes = new Uint8Array(8);
    new DataView(bytes.buffer).setBigUint64(0, BigInt(id), false);
    return bytes;
  };

  /** The bytes of a binary result as one ArrayBuffer behind the id. */
  const binaryFrame = (id, value) => {
    const view = value instanceof ArrayBuffer ? new Uint8Array(value) : new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    const out = new Uint8Array(8 + view.byteLength);
    out.set(be64(id), 0);
    out.set(view, 8);
    return out.buffer;
  };

  class FakeWebSocket {
    static CONNECTING = CONNECTING;
    static OPEN = OPEN;
    static CLOSING = CLOSING;
    static CLOSED = CLOSED;
    CONNECTING = CONNECTING;
    OPEN = OPEN;
    CLOSING = CLOSING;
    CLOSED = CLOSED;

    constructor(url) {
      this.url = String(url);
      this.readyState = CONNECTING;
      this.binaryType = "blob";
      this.bufferedAmount = 0;
      this.protocol = "";
      this.extensions = "";
      this.onopen = null;
      this.onmessage = null;
      this.onclose = null;
      this.onerror = null;
      this._listeners = { open: [], message: [], close: [], error: [] };
      state.urls.push(this.url);
      state.sockets.push(this);
      setTimeout(() => this._start(), 0);
    }

    addEventListener(type, fn) {
      this._listeners[type]?.push(fn);
    }

    removeEventListener(type, fn) {
      const list = this._listeners[type];
      const at = list ? list.indexOf(fn) : -1;
      if (at >= 0) list.splice(at, 1);
    }

    _fire(type, event) {
      const handler = this[`on${type}`];
      if (typeof handler === "function") handler.call(this, event);
      for (const fn of [...this._listeners[type]]) fn.call(this, event);
    }

    _start() {
      if (this.readyState !== CONNECTING) return;
      if (state.refuse > 0) {
        state.refuse--;
        this._end(1006);
      } else if (state.held) {
        state.waiting.push(this);
      } else {
        this._open();
      }
    }

    _open() {
      if (this.readyState !== CONNECTING) return;
      this.readyState = OPEN;
      this._fire("open", { type: "open" });
    }

    _end(code) {
      if (this.readyState === CLOSED) return;
      this.readyState = CLOSED;
      this._fire("error", { type: "error" });
      this._fire("close", { type: "close", code, reason: "", wasClean: code === 1000 });
    }

    /** One frame from the server to the page. */
    _deliver(data) {
      if (state.silent || this.readyState !== OPEN) return false;
      const wire = data instanceof ArrayBuffer ? (this.binaryType === "arraybuffer" ? data : new Blob([data])) : data;
      this._fire("message", { type: "message", data: wire });
      return true;
    }

    send(data) {
      if (this.readyState !== OPEN) throw new DOMException("the socket is not open", "InvalidStateError");
      if (typeof data !== "string") throw new TypeError("the page sends text frames only");
      state.sent.push(data);
      let frame;
      try {
        frame = JSON.parse(data);
      } catch {
        return;
      }
      const { id, cmd } = frame;
      // A call without arguments is `{}` on the wire; the mock core records it as the Tauri global does, without them.
      const args = frame.args && typeof frame.args === "object" && Object.keys(frame.args).length === 0 ? undefined : frame.args;
      Promise.resolve()
        .then(() => mock.invoke(cmd, args))
        .then(
          (result) => {
            if (result instanceof ArrayBuffer || ArrayBuffer.isView(result)) this._deliver(binaryFrame(id, result));
            else this._deliver(JSON.stringify({ id, ok: result ?? null })); // the server always writes the member
          },
          (e) => {
            const err = e instanceof Error ? e.message : e;
            this._deliver(JSON.stringify({ id, err }));
          },
        );
    }

    close(code = 1000) {
      if (this.readyState === CLOSED || this.readyState === CLOSING) return;
      this.readyState = CLOSING;
      setTimeout(() => {
        if (this.readyState === CLOSED) return;
        this.readyState = CLOSED;
        this._fire("close", { type: "close", code, reason: "", wasClean: true });
      }, 0);
    }
  }

  window.WebSocket = FakeWebSocket;

  window.__WS_MOCK__ = {
    get urls() {
      return state.urls;
    },
    get sockets() {
      return state.sockets;
    },
    get sent() {
      return state.sent;
    },
    push(name, payload, wanted) {
      if (!wanted({ target: mock.label })) return Promise.resolve(0);
      const text = JSON.stringify({ event: name, payload });
      let delivered = 0;
      for (const s of state.sockets) if (s._deliver(text)) delivered++;
      // The Tauri mock answers how many listeners got the event. The server cannot know that, so the page's own bridge
      // is asked (it counts its registrations; all of them name the one window `main`). The frame itself went out above,
      // in this turn, as it would have been delivered to the Tauri listeners.
      if (delivered === 0) return Promise.resolve(0);
      return import(new URL("bridge.js", location.href).href).then((bridge) => bridge.listenerCount(name));
    },
    drop(code) {
      for (const s of state.sockets) {
        if (s.readyState === OPEN) {
          s.readyState = CLOSED;
          s._fire("close", { type: "close", code, reason: "", wasClean: code === 1000 });
        }
      }
    },
    refuseOpen(n) {
      state.refuse = n;
    },
    hold() {
      state.held = true;
    },
    release() {
      state.held = false;
      for (const s of state.waiting.splice(0)) s._open();
    },
    silence(on = true) {
      state.silent = on;
    },
    reset() {
      state.urls.length = 0;
      state.sent.length = 0;
      state.refuse = 0;
      state.held = false;
      state.silent = false;
    },
  };
})();
