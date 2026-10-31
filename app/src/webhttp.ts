// The page's one way to ask its own server over HTTP (the login page and the account dialogs use it; the terminal
// itself talks over the socket). This file is pure: the request function, the origin and the reading of the token are
// handed in, so the unit tests need no network, and `bridge.ts` is the only file that names the real request function.
//
// A path must start with a single slash and stay on the page's origin; anything else is a `TypeError` before anything is
// sent. Every request is same-origin, uncached, without redirects and without a referrer. The request mode is `cors`, not
// `same-origin`: for a request that changes state the Fetch standard serializes the `Origin` header as `null` when the mode
// is not `cors` and the referrer policy is `no-referrer`, and WebKit does so, which the server rightly refuses; with `cors`
// every engine sends the real origin. The address is still checked to be on the page's origin before anything is sent, so
// the mode opens no other door. The CSRF token comes from
// `GET /api/session`, is kept in this closure only, is sent on every request that is not a GET and is dropped after a 403.
// Nothing is ever retried: a request that changes state must not run twice.

/** The longest message of an `HttpError`, in characters. */
export const MAX_ERROR_MESSAGE = 200;
/** The longest failed answer whose JSON is kept as `HttpError.body`, in characters (4 KiB). */
export const MAX_ERROR_BODY = 4096;

/** The part of a response the function uses; the browser's response has all of it. */
export interface ResponseLike {
  ok: boolean;
  status: number;
  statusText?: string;
  text(): Promise<string>;
}

/** The request options the function sets; the browser's request function takes them as they are. */
export interface RequestInitLike {
  method: string;
  headers: Record<string, string>;
  body?: unknown;
  credentials: "same-origin";
  redirect: "error";
  cache: "no-store";
  referrerPolicy: "no-referrer";
  mode: "cors";
}

/** What `createHttp` needs from its surroundings. */
export interface HttpDeps {
  /** Make one request. */
  send(url: string, init: RequestInitLike): Promise<ResponseLike>;
  /** The origin of the page, `https://host` or `http://host:port`. */
  origin: string;
  /** Read the CSRF token out of the parsed answer of `GET /api/session`; the default reads its string member `csrf`. */
  readToken?(session: unknown): string | undefined;
}

/** A failed request: the status (0 when nothing arrived), a message of at most 200 characters, and the parsed JSON object of the answer. */
export class HttpError extends Error {
  readonly status: number;
  readonly body: Record<string, unknown> | undefined;
  constructor(status: number, message: string, body?: Record<string, unknown>) {
    super(message.slice(0, MAX_ERROR_MESSAGE));
    this.name = "HttpError";
    this.status = status;
    this.body = body;
  }
}

/** What `http` is: the method, a path on the page's origin and an optional body. */
export type Http = (method: string, path: string, body?: unknown) => Promise<unknown>;

const METHODS = new Set(["GET", "POST", "PUT", "PATCH", "DELETE"]);
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f]/;

const defaultReadToken = (session: unknown): string | undefined => {
  if (typeof session !== "object" || session === null) return undefined;
  const csrf = (session as Record<string, unknown>).csrf;
  return typeof csrf === "string" && csrf !== "" ? csrf : undefined;
};

/** The parsed JSON object of `text` when it is one of at most 4 KiB, else undefined. */
function errorBody(text: string): Record<string, unknown> | undefined {
  if (text.length > MAX_ERROR_BODY) return undefined;
  try {
    const parsed: unknown = JSON.parse(text);
    return typeof parsed === "object" && parsed !== null && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : undefined;
  } catch {
    return undefined;
  }
}

/** Build the request function over `deps`. */
export function createHttp(deps: HttpDeps): Http {
  const readToken = deps.readToken ?? defaultReadToken;
  /** The origin of the page, read at the first request so that building the function can never fail (a page outside a server has none). */
  let known: string | undefined;
  const originOf = (): string => {
    known ??= new URL(deps.origin).origin;
    return known;
  };
  let token: string | undefined;
  let tokenKnown = false;
  let tokenRequest: Promise<void> | undefined;

  /** The URL on the page's origin for `path`, or a TypeError. */
  const resolve = (path: unknown): string => {
    if (typeof path !== "string" || path[0] !== "/" || path[1] === "/" || path[1] === "\\" || CONTROL.test(path)) {
      throw new TypeError("a path must start with a single slash");
    }
    const pageOrigin = originOf();
    let url: URL;
    try {
      url = new URL(path, pageOrigin);
    } catch {
      throw new TypeError("not a valid path");
    }
    if (url.origin !== pageOrigin) throw new TypeError("a path must stay on the page's origin");
    return `${pageOrigin}${url.pathname}${url.search}`;
  };

  const init = (method: string, headers: Record<string, string>, body?: unknown): RequestInitLike => {
    const out: RequestInitLike = {
      method,
      headers,
      credentials: "same-origin",
      redirect: "error",
      cache: "no-store",
      referrerPolicy: "no-referrer",
      mode: "cors",
    };
    if (body !== undefined) out.body = body;
    return out;
  };

  /** One round trip: the answer text and status, or an `HttpError` for a network failure. */
  const exchange = async (url: string, options: RequestInitLike): Promise<{ res: ResponseLike; text: string }> => {
    let res: ResponseLike;
    let text = "";
    try {
      res = await deps.send(url, options);
      if (res.status !== 204) text = await res.text();
    } catch {
      throw new HttpError(0, "cannot reach the server");
    }
    return { res, text };
  };

  const fail = (res: ResponseLike, text: string): HttpError => {
    const body = errorBody(text);
    const named = body && (typeof body.error === "string" ? body.error : typeof body.code === "string" ? body.code : "");
    const message = named || `${res.status} ${res.statusText ?? ""}`.trim();
    return new HttpError(res.status, message, body);
  };

  const parse = (res: ResponseLike, text: string): unknown => {
    if (res.status === 204) return undefined;
    try {
      return JSON.parse(text);
    } catch {
      throw new HttpError(res.status, "the answer is not JSON");
    }
  };

  const loadToken = async (): Promise<void> => {
    const { res, text } = await exchange(`${originOf()}/api/session`, init("GET", { Accept: "application/json" }));
    if (!res.ok) throw fail(res, text);
    token = readToken(parse(res, text));
    tokenKnown = true;
  };

  const csrf = async (): Promise<string | undefined> => {
    if (!tokenKnown) {
      tokenRequest ??= loadToken().finally(() => {
        tokenRequest = undefined;
      });
      await tokenRequest;
    }
    return token;
  };

  return async (method, path, body) => {
    const verb = typeof method === "string" ? method.toUpperCase() : "";
    if (!METHODS.has(verb)) throw new TypeError("not a method the page uses");
    const url = resolve(path);
    if (verb === "GET" && body !== undefined) throw new TypeError("a GET has no body");

    const headers: Record<string, string> = { Accept: "application/json" };
    let payload: unknown;
    // The server checks the content type of every change before it looks at the route: JSON for the API (also for a change that has
    // no body, such as a DELETE) and a byte stream for an upload. A file carries its own type (`image/png`), which the server would
    // refuse, so the type is named here and the browser takes the header before the type of the Blob.
    if (body === undefined) {
      payload = undefined;
      if (verb !== "GET") headers["Content-Type"] = "application/json";
    } else if (typeof Blob !== "undefined" && body instanceof Blob) {
      payload = body;
      headers["Content-Type"] = "application/octet-stream";
    } else {
      headers["Content-Type"] = "application/json";
      payload = JSON.stringify(body);
    }
    if (verb !== "GET") {
      const t = await csrf();
      if (t) headers["X-CSRF-Token"] = t;
    }

    const { res, text } = await exchange(url, init(verb, headers, payload));
    if (!res.ok) {
      if (res.status === 403) {
        token = undefined;
        tokenKnown = false;
      }
      throw fail(res, text);
    }
    return parse(res, text);
  };
}
