// The same-origin request function against an injected `send`: the path check, the headers, the CSRF token, the bodies,
// the failed answers. No network. Runs on dist/ like the other unit tests.
import test from "node:test";
import assert from "node:assert/strict";
import { HttpError, MAX_ERROR_MESSAGE, createHttp } from "../../dist/webhttp.js";

const ORIGIN = "https://hw.example.test";

const answer = (status, body, statusText = "") => ({
  ok: status >= 200 && status < 300,
  status,
  statusText,
  text: async () => (body === undefined ? "" : typeof body === "string" ? body : JSON.stringify(body)),
});

/** An http function over a scripted server: `routes` maps "METHOD path" to an answer or a function of the init. */
function setup(routes = {}, origin = ORIGIN) {
  const log = [];
  const send = async (url, init) => {
    log.push({ url, init });
    const path = url.slice(origin.length);
    const route = routes[`${init.method} ${path}`];
    if (route === undefined) return answer(404, { error: "no route" });
    return typeof route === "function" ? route(init) : route;
  };
  return { http: createHttp({ send, origin }), log };
}

const session = (csrf) => answer(200, csrf === undefined ? { state: "anonymous" } : { state: "signed_in", csrf });

test("a path on the page's origin is sent there with the fixed request options", async () => {
  const { http, log } = setup({ "GET /api/x": answer(200, { a: 1 }) });
  assert.deepEqual(await http("GET", "/api/x"), { a: 1 });
  assert.equal(log.length, 1);
  assert.equal(log[0].url, `${ORIGIN}/api/x`);
  assert.equal(log[0].init.method, "GET");
  assert.equal(log[0].init.credentials, "same-origin");
  assert.equal(log[0].init.redirect, "error");
  assert.equal(log[0].init.cache, "no-store");
  assert.equal(log[0].init.referrerPolicy, "no-referrer");
  // With `no-referrer`, a mode other than `cors` makes WebKit send `Origin: null` on a POST, which the server refuses.
  assert.equal(log[0].init.mode, "cors");
  assert.equal(log[0].init.headers["X-CSRF-Token"], undefined, "a GET sends no token");
});

test("a query string stays, a fragment does not travel", async () => {
  const { http, log } = setup({ "GET /api/x?a=1&b=2": answer(200, []) });
  await http("GET", "/api/x?a=1&b=2#frag");
  assert.equal(log[0].url, `${ORIGIN}/api/x?a=1&b=2`);
});

test("hostile paths throw a TypeError before anything is sent", async () => {
  const { http, log } = setup();
  const hostile = [
    "//evil.test/x", "https://evil.test/x", "http://evil.test/", "/\\evil.test", "\\\\evil.test", "api/x", "", " /api/x",
    "/api\n/x", "/\t/evil.test", "/api/x\r\nHost: evil", "/api/\u0000x", "javascript:alert(1)", "data:text/html,x", "?x=1", "#x", undefined, null, 5, {},
  ];
  for (const path of hostile) {
    await assert.rejects(http("GET", path), TypeError, JSON.stringify(path));
    await assert.rejects(http("POST", path, { a: 1 }), TypeError, JSON.stringify(path));
  }
  assert.equal(log.length, 0, "nothing was sent, not even a token request");
});

test("a method outside the five the page uses throws a TypeError", async () => {
  const { http, log } = setup();
  for (const method of ["TRACE", "CONNECT", "", "GET /x", undefined, "HEAD"]) await assert.rejects(http(method, "/api/x"), TypeError);
  await assert.rejects(http("GET", "/api/x", { a: 1 }), TypeError, "a GET has no body");
  assert.equal(log.length, 0);
});

test("a POST sends JSON and the token from GET /api/session, fetched once", async () => {
  const { http, log } = setup({
    "GET /api/session": session("tok-1"),
    "POST /api/a": answer(200, { done: true }),
    "POST /api/b": answer(200, { done: 2 }),
  });
  assert.deepEqual(await http("POST", "/api/a", { name: "x" }), { done: true });
  assert.deepEqual(await http("post", "/api/b", [1, 2]), { done: 2 });
  assert.deepEqual(log.map((l) => `${l.init.method} ${l.url.slice(ORIGIN.length)}`), ["GET /api/session", "POST /api/a", "POST /api/b"]);
  assert.equal(log[1].init.headers["Content-Type"], "application/json");
  assert.equal(log[1].init.headers["X-CSRF-Token"], "tok-1");
  assert.equal(log[1].init.body, '{"name":"x"}');
  assert.equal(log[2].init.body, "[1,2]");
  assert.equal(log[0].init.headers["X-CSRF-Token"], undefined);
});

test("two requests at once ask for the token once", async () => {
  const { http, log } = setup({ "GET /api/session": session("t"), "POST /api/a": answer(204) });
  await Promise.all([http("POST", "/api/a", {}), http("POST", "/api/a", {})]);
  assert.equal(log.filter((l) => l.url.endsWith("/api/session")).length, 1);
});

test("a Blob body is sent as it is, without a JSON content type", async () => {
  const { http, log } = setup({ "GET /api/session": session("t"), "PUT /api/upload": answer(200, { n: 3 }) });
  const blob = new Blob(["abc"], { type: "application/octet-stream" });
  assert.deepEqual(await http("PUT", "/api/upload", blob), { n: 3 });
  const put = log.at(-1);
  assert.equal(put.init.body, blob);
  assert.equal(put.init.headers["Content-Type"], undefined);
  assert.equal(put.init.headers["X-CSRF-Token"], "t");
});

test("a request without a body sends none", async () => {
  const { http, log } = setup({ "GET /api/session": session("t"), "DELETE /api/a/1": answer(204) });
  assert.equal(await http("DELETE", "/api/a/1"), undefined);
  assert.equal("body" in log.at(-1).init, false);
  assert.equal(log.at(-1).init.headers["Content-Type"], undefined);
});

test("a 403 drops the token and is not retried", async () => {
  let n = 0;
  const { http, log } = setup({
    "GET /api/session": () => session(`tok-${++n}`),
    "POST /api/a": answer(403, { error: "csrf" }),
    "POST /api/b": answer(200, {}),
  });
  await assert.rejects(http("POST", "/api/a", {}), (e) => e instanceof HttpError && e.status === 403 && e.message === "csrf");
  assert.equal(log.filter((l) => l.init.method === "POST").length, 1, "never retried");
  await http("POST", "/api/b", {});
  assert.equal(log.at(-1).init.headers["X-CSRF-Token"], "tok-2", "a new token was asked for");
  assert.equal(log.filter((l) => l.url.endsWith("/api/session")).length, 2);
});

test("an anonymous session has no token and that is kept", async () => {
  const { http, log } = setup({ "GET /api/session": session(undefined), "POST /api/login": answer(200, { ok: 1 }) });
  await http("POST", "/api/login", { name: "a" });
  await http("POST", "/api/login", { name: "a" });
  assert.equal(log.filter((l) => l.url.endsWith("/api/session")).length, 1);
  assert.equal(log.at(-1).init.headers["X-CSRF-Token"], undefined);
  assert.equal("X-CSRF-Token" in log.at(-1).init.headers, false);
});

test("a failed answer throws HttpError with the status, a bounded message and the JSON object", async () => {
  const long = "x".repeat(500);
  const { http } = setup({
    "GET /api/a": answer(401, { error: "wrong name or password", code: "bad_login", field: "password", retry_seconds: 30 }),
    "GET /api/b": answer(429, { code: "rate_limited", retry_seconds: 12 }),
    "GET /api/c": answer(500, "<html>boom</html>", "Internal Server Error"),
    "GET /api/d": answer(400, { error: long }),
    "GET /api/e": answer(409, [1, 2]),
    "GET /api/f": answer(400, { error: "e", pad: "y".repeat(5000) }),
    "GET /api/g": answer(502, ""),
  });
  const caught = async (path) => http("GET", path).then(() => assert.fail("no error"), (e) => e);
  const a = await caught("/api/a");
  assert.ok(a instanceof HttpError && a instanceof Error);
  assert.equal(a.status, 401);
  assert.equal(a.message, "wrong name or password");
  assert.deepEqual(a.body, { error: "wrong name or password", code: "bad_login", field: "password", retry_seconds: 30 });
  const b = await caught("/api/b");
  assert.equal(b.message, "rate_limited");
  assert.equal(b.body.retry_seconds, 12);
  const c = await caught("/api/c");
  assert.equal(c.message, "500 Internal Server Error");
  assert.equal(c.body, undefined);
  const d = await caught("/api/d");
  assert.equal(d.message.length, MAX_ERROR_MESSAGE);
  const e = await caught("/api/e");
  assert.equal(e.body, undefined, "an array is not an object body");
  const f = await caught("/api/f");
  assert.equal(f.body, undefined, "over 4 KiB is not kept");
  assert.equal(f.message, "400", "a body that is not kept does not name the error either");
  const g = await caught("/api/g");
  assert.equal(g.message, "502");
  assert.equal(Object.hasOwn(a, "headers"), false);
});

test("a 204 answers undefined, and a 200 that is not JSON is an HttpError", async () => {
  const { http } = setup({ "GET /api/a": answer(204), "GET /api/b": answer(200, "<html>"), "GET /api/c": answer(200, "") });
  assert.equal(await http("GET", "/api/a"), undefined);
  await assert.rejects(http("GET", "/api/b"), (e) => e instanceof HttpError && e.status === 200);
  await assert.rejects(http("GET", "/api/c"), (e) => e instanceof HttpError);
});

test("a request that cannot reach the server is an HttpError with status 0 and no detail", async () => {
  const http = createHttp({
    origin: ORIGIN,
    send: async () => {
      throw new TypeError("Load failed: secret detail");
    },
  });
  await assert.rejects(http("GET", "/api/a"), (e) => e instanceof HttpError && e.status === 0 && !e.message.includes("secret"));
});

test("an http page works the same on its own origin and refuses the https one", async () => {
  const origin = "http://127.0.0.1:4000";
  const { http, log } = setup({ "GET /api/a": answer(200, {}) }, origin);
  await http("GET", "/api/a");
  assert.equal(log[0].url, "http://127.0.0.1:4000/api/a");
  await assert.rejects(http("GET", "//127.0.0.1:4000/api/a"), TypeError);
});

test("building the function never fails, even for a page without an origin", async () => {
  const http = createHttp({ origin: "null", send: async () => answer(200, {}) });
  await assert.rejects(http("GET", "/api/a"));
});
