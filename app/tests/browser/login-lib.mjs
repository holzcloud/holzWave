// Helpers for the tests of the login page and, later, of the account and people dialogs: a scripted server behind
// `page.route`, a recorder that finds every request to another origin, a stand-in for the browser's authenticator, and two
// readers for what a person sees (the error line, the focused control).
//
// The static files of the page (the document, its modules, styles and fonts) are served by the test server; everything
// else the page asks is answered from a table, so a request nobody scripted is a finding, not a silent 404.
import { fileURLToPath } from "node:url";

/** The answer of `GET /api/session` for a person who is not signed in. `over` replaces members; `methods` is merged. */
export function anonymous(over = {}) {
  const { methods, ...rest } = over;
  return {
    state: "anonymous",
    host: "holz.example.net",
    bootstrap: false,
    methods: { passkey: false, password: true, oidc: [], ...methods },
    ...rest,
  };
}

/** A JSON error answer of the server: the status and `{ code, ...more }`. */
export const refusal = (status, code, more = {}) => ({ status, body: { code, ...more } });

const STATIC = /\.(?:html|js|css|woff2|png|svg)$/;

/**
 * Script the server for `page`. `table` is a list of `{ method, path, status, body }`; `body` is a value or a function
 * `(request, n) => value` where `request` is `{ method, path, headers, text, json }` and `n` counts the earlier requests
 * to the same route. `status` may be a function of the same two arguments. `abort: true` fails the request as a lost network
 * would, `delay` waits that many milliseconds before answering. An entry may also carry `answers`, a list of such answers
 * `{ status, body, abort, delay }` used in turn (the last one repeats).
 *
 * Returns `{ log, foreign, unscripted, assets, count(method, path) }`:
 *   log        every scripted request in order, as `{ method, path, headers, text, json }`
 *   foreign    the URL of every request to an origin other than the page's (they are refused)
 *   unscripted `METHOD path` of every request that no entry answered (answered with 404)
 *   assets     the paths of the static files that were fetched
 */
export async function installRoutes(page, table, { origin }) {
  const log = [];
  const foreign = [];
  const unscripted = [];
  const assets = [];
  const seen = new Map();
  const routes = table.map((entry) => ({ ...entry, method: (entry.method ?? "GET").toUpperCase() }));
  const pageOrigin = () => origin;

  const find = (method, path) => routes.find((r) => r.method === method && r.path === path);

  await page.route("**/*", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.protocol === "data:" || url.protocol === "blob:" || url.protocol === "about:") return route.continue();
    if (url.origin !== pageOrigin()) {
      foreign.push(request.url());
      return route.abort("failed");
    }
    const method = request.method();
    const entry = find(method, url.pathname);
    if (entry === undefined) {
      if (method === "GET" && STATIC.test(url.pathname)) {
        assets.push(url.pathname);
        return route.continue();
      }
      unscripted.push(`${method} ${url.pathname}`);
      return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ code: "not_found" }) });
    }
    const text = request.postData() ?? "";
    let json;
    try {
      json = text === "" ? undefined : JSON.parse(text);
    } catch {
      json = undefined;
    }
    const entryLog = { method, path: url.pathname, headers: request.headers(), text, json };
    const n = log.filter((l) => l.method === method && l.path === url.pathname).length;
    log.push(entryLog);
    const pick = Array.isArray(entry.answers) ? entry.answers[Math.min(n, entry.answers.length - 1)] : entry;
    if (pick.delay) await new Promise((done) => setTimeout(done, pick.delay));
    if (pick.abort) return route.abort("failed");
    const status = typeof pick.status === "function" ? pick.status(entryLog, n) : (pick.status ?? 200);
    const body = typeof pick.body === "function" ? pick.body(entryLog, n) : pick.body;
    if (pick.html) return route.fulfill({ status, contentType: "text/html; charset=utf-8", body: String(body ?? "") });
    if (status === 204) return route.fulfill({ status });
    return route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body ?? {}) });
  });

  return {
    log,
    foreign,
    unscripted,
    assets,
    count: (method, path) => log.filter((l) => l.method === method.toUpperCase() && l.path === path).length,
    requests: (method, path) => log.filter((l) => l.method === method.toUpperCase() && l.path === path),
  };
}

/** A stub page for `GET /` so that a redirect to the app has somewhere to land. */
export const homeStub = { method: "GET", path: "/", html: true, status: 200, body: "<!doctype html><title>home</title><p>home</p>" };

/** The text of the first visible `role=alert` element, or the empty string. */
export const errorText = (page) =>
  page.evaluate(() => {
    for (const el of document.querySelectorAll('[role="alert"]')) {
      if (el.hidden || el.closest("[hidden]")) continue;
      if (el.getClientRects().length === 0) continue;
      const text = el.textContent.trim();
      if (text !== "") return text;
    }
    return "";
  });

/** The text of the line under the form (`.lg-line`), whatever its role: the sentence of the current state. */
export const lineText = (page) => page.evaluate(() => (document.querySelector(".lg-line")?.textContent ?? "").trim());

/** The accessible name of the focused element: its label, its aria-label or its own text. */
export const focusedName = (page) =>
  page.evaluate(() => {
    const el = document.activeElement;
    if (el === null || el === document.body) return "";
    const aria = el.getAttribute("aria-label");
    if (aria) return aria.trim();
    const labelled = el.getAttribute("aria-labelledby");
    if (labelled) return labelled.split(/\s+/).map((id) => document.getElementById(id)?.textContent ?? "").join(" ").trim();
    if (el.labels && el.labels.length > 0) return Array.from(el.labels).map((l) => l.textContent).join(" ").trim();
    return (el.textContent ?? "").trim();
  });

/**
 * Put a stand-in for the browser's authenticator on every page this tab loads. What it does is chosen by the `authn`
 * member of the address query, so a test picks it with the address and no other state is needed:
 *   prf       get() and create() answer a credential whose extension results carry a PRF output
 *   plain     the same without a PRF output
 *   cancel    the person closes the prompt: a `NotAllowedError`
 *   error     the browser fails: a `TypeError`
 * `nopk=1` in the query makes `PublicKeyCredential` undefined, as in a browser without WebAuthn. Every call is recorded
 * as `window.__authn` (`{ kind: "get" | "create", options }`) with the binary members as base64url text.
 */
export async function installAuthenticator(page) {
  await page.addInitScript(() => {
    const query = new URLSearchParams(location.search);
    const mode = query.get("authn");
    if (query.get("nopk") === "1") {
      Object.defineProperty(window, "PublicKeyCredential", { value: undefined, configurable: true, writable: true });
      return;
    }
    if (typeof window.PublicKeyCredential === "undefined") window.PublicKeyCredential = function PublicKeyCredential() {};
    if (mode === null) return;
    const bytes = (...values) => new Uint8Array(values).buffer;
    const text = (s) => new TextEncoder().encode(s).buffer;
    const b64 = (buf) => {
      let bin = "";
      for (const b of new Uint8Array(buf)) bin += String.fromCharCode(b);
      return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    };
    const seen = [];
    window.__authn = seen;
    const show = (v) => {
      if (v instanceof ArrayBuffer) return { binary: b64(v) };
      if (Array.isArray(v)) return v.map(show);
      if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, show(x)]));
      return v;
    };
    const results = () => (mode === "prf" ? { prf: { results: { first: bytes(...Array.from({ length: 32 }, (_, i) => i + 1)) } } } : {});
    const answer = (kind) => {
      if (mode === "cancel") throw new DOMException("The operation either timed out or was not allowed.", "NotAllowedError");
      if (mode === "error") throw new TypeError("the browser failed");
      const credential = {
        id: "Y3JlZC1pZA",
        rawId: bytes(99, 114, 101, 100, 45, 105, 100),
        type: "public-key",
        getClientExtensionResults: results,
        response:
          kind === "get"
            ? { authenticatorData: bytes(1, 2, 3), clientDataJSON: text('{"type":"webauthn.get"}'), signature: bytes(4, 5, 6), userHandle: null }
            : { attestationObject: bytes(7, 8, 9), clientDataJSON: text('{"type":"webauthn.create"}'), getTransports: () => ["internal"] },
      };
      return credential;
    };
    const get = async (options) => {
      seen.push({ kind: "get", options: show(options?.publicKey) });
      return answer("get");
    };
    const create = async (options) => {
      seen.push({ kind: "create", options: show(options?.publicKey) });
      return answer("create");
    };
    // The whole container is replaced (on the prototype, where the getter lives), so no engine can hand out the real one.
    const fake = { get, create, store: async () => undefined, preventSilentAccess: async () => undefined };
    Object.defineProperty(Navigator.prototype, "credentials", { get: () => fake, configurable: true });
  });
}

/** The tokens the tests use: the shapes the page accepts in an address fragment. */
export const INVITE_TOKEN = `hwi_${"A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v"}`;
export const SETUP_TOKEN = `hwb_${"Z9y8X7w6V5u4T3s2R1q0P9o8N7m6L5k4J3i2H1g0F9e"}`;

/** The address of the login page in the build. */
export const loginUrl = (url, suffix = "") => `${url}login.html${suffix}`;

/** The directory of the compiled app, for tests that read the shipped files. */
export const DIST = fileURLToPath(new URL("../../dist/", import.meta.url));
