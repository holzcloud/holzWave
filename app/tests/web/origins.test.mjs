// The origin, cookie and content-security promises against hostile pages in a real browser. The victim is an administrator
// signed in with the harness server. Attacker pages are served by route fulfilments inside the victim's browser (no network
// is involved for them): one on another port of the same host (same site, other origin: the Strict cookie IS sent), one on
// another site (no cookie), and one on the numeric address, which is a host the server does not serve. Each runs a script that
// tries to sign the victim out, change an account, read an answer and open a socket. Afterwards the victim's session works,
// the account list is unchanged, the sockets closed with the codes below and received nothing, and no answer of the server
// carries an access-control header.
import { runTest, signIn, openPeople, personRow, oracle, check, equal, poll } from "./lib.mjs";

/** The script of an attacker page. `BASE` is the server's origin as a string. */
const script = (base, targetId) => `
(async () => {
  const BASE = ${JSON.stringify(base)};
  const out = {};
  const tryFetch = async (name, path, init) => {
    try { const r = await fetch(BASE + path, init); out[name] = r.status; }
    catch (e) { out[name] = "error " + e.name; }
  };
  const json = { "Content-Type": "application/json" };
  await tryFetch("logout", "/auth/logout", { method: "POST", credentials: "include", headers: json, body: "{}" });
  await tryFetch("admin", "/api/admin/accounts/${targetId}/disable", { method: "POST", credentials: "include", headers: json, body: "{}" });
  await tryFetch("logoutSimple", "/auth/logout", { method: "POST", mode: "no-cors", credentials: "include", headers: { "Content-Type": "text/plain" }, body: "{}" });
  await tryFetch("adminSimple", "/api/admin/accounts/${targetId}/disable", { method: "POST", mode: "no-cors", credentials: "include", headers: { "Content-Type": "text/plain" }, body: "{}" });
  await tryFetch("read", "/api/me", { credentials: "include" });
  out.socket = await new Promise((resolve) => {
    let opened = false, messages = 0, sawSession = false;
    const socket = new WebSocket(BASE.replace("https:", "wss:") + "/ws");
    const timer = setTimeout(() => resolve({ opened, code: null, messages, sawSession }), 8000);
    socket.onopen = () => { opened = true; };
    socket.onmessage = (e) => { messages++; if (String(e.data).includes("hw-session")) sawSession = true; };
    socket.onclose = (e) => { clearTimeout(timer); resolve({ opened, code: e.code, messages, sawSession }); };
  });
  window.__attackDone(out);
})();
`;

await runTest("origins", {}, async ({ harness, open }) => {
  const hostname = harness.hostname;
  const otherPort = harness.port > 60000 ? harness.port - 11 : harness.port + 11;
  const sameSite = `https://${hostname}:${otherPort}`;
  const crossSite = "https://attacker.test";
  const numeric = `https://127.0.0.1:${harness.port}`;

  const admin = await harness.account({ role: "admin", name: "po-admin" });
  const bystander = await harness.account({ role: "member", name: "po-bystander" });

  const victim = await open({ allow: [sameSite, crossSite, numeric, `http://${hostname}:${harness.port}`] });
  const answers = { checked: 0, withCors: [] };
  victim.context.on("response", async (response) => {
    if (!response.url().startsWith(harness.origin)) return;
    answers.checked++;
    const names = Object.keys(await response.allHeaders().catch(() => ({})));
    for (const name of names) if (name.toLowerCase().startsWith("access-control-")) answers.withCors.push(`${new URL(response.url()).pathname} ${name}`);
  });
  const home = await victim.context.newPage();
  await signIn(home, harness, admin);

  const api = victim.context.request;
  const accounts = async () => (await (await api.get(`${harness.origin}/api/admin/accounts`)).json()).accounts.map((a) => `${a.id}:${a.role}:${a.state}`).join(",");
  const before = await accounts();
  const target = (await (await api.get(`${harness.origin}/api/admin/accounts`)).json()).accounts.find((a) => a.username === bystander.username).id;

  // ---- the cookie ------------------------------------------------------------------------------------------------------------------
  const jar = await victim.context.cookies(harness.origin);
  const cookie = jar.find((c) => c.name === "__Host-hw_session");
  check(cookie !== undefined, "the victim has the session cookie");
  equal(cookie.httpOnly, true, "the session cookie is HttpOnly");
  equal(cookie.secure, true, "the session cookie is Secure");
  equal(cookie.sameSite, "Strict", "the session cookie's SameSite");
  equal(cookie.path, "/", "the session cookie's path");
  check(!jar.some((c) => c.name.toLowerCase().includes("hw_session") && c.name !== "__Host-hw_session"), "no other session cookie");
  const pageSeesCookie = await home.evaluate(() => document.cookie);
  equal(pageSeesCookie, "", "a script of the page sees no cookie");

  // ---- the attacker pages -----------------------------------------------------------------------------------------------------------
  const attacks = {};
  const attack = async (label, origin, path = "/") => {
    const page = await victim.context.newPage();
    victim.foreign(page);
    let report;
    const done = new Promise((resolve) => {
      report = resolve;
    });
    await page.exposeFunction("__attackDone", (out) => report(out));
    await victim.context.route(`${origin}${path}`, (route) =>
      route.fulfill({ status: 200, contentType: "text/html; charset=utf-8", body: `<!doctype html><title>x</title><script>${script(harness.origin, target)}</script>` }),
    );
    await page.goto(`${origin}${path}`);
    attacks[label] = await Promise.race([done, new Promise((_, reject) => setTimeout(() => reject(new Error(`the ${label} page did not report`)), 30_000))]);
    await page.close();
  };
  await attack("same site, other port", sameSite);
  await attack("other site", crossSite);
  await attack("numeric host", numeric, "/attacker");

  for (const [label, r] of Object.entries(attacks)) {
    equal(String(r.read).startsWith("error"), true, `${label}: a read of an answer from the hostile page fails (${r.read})`);
    check(r.logout === "error TypeError" || r.logout === 401 || r.logout === 403, `${label}: the sign-out try is refused (${r.logout})`);
    check(r.admin === "error TypeError" || [401, 403].includes(r.admin), `${label}: the account change is refused (${r.admin})`);
    equal(r.socket.sawSession, false, `${label}: the hostile socket never received the session message`);
    equal(r.socket.messages, 0, `${label}: the hostile socket received nothing`);
  }
  equal(attacks["same site, other port"].socket.code, 4403, "same site, other port: the close code of the socket");
  check([4401, 4403].includes(attacks["other site"].socket.code), `other site: the close code of the socket (${attacks["other site"].socket.code})`);
  equal(attacks["numeric host"].socket.code, 4403, "numeric host: the close code of the socket");

  // ---- the victim is untouched -------------------------------------------------------------------------------------------------------
  equal(await accounts(), before, "the account list after the attacks");
  const afterJar = await victim.context.cookies(harness.origin);
  check(afterJar.some((c) => c.name === "__Host-hw_session"), "the victim still has the session cookie");
  {
    const socket = await oracle(home);
    const layout = await socket.layout();
    check(Array.isArray(layout.panes) && layout.panes.length >= 1, "the victim's own socket opens and get_layout answers");
    await socket.close();
  }
  await openPeople(home);
  check((await personRow(home, bystander.username).locator(".ppl-state .ppl-word").innerText()).trim() === "active", "the bystander is still active in the list");

  // ---- hosts the server does not serve, and plain http ------------------------------------------------------------------------------------
  const numericPage = await victim.context.newPage();
  // The answer for a host the server does not serve is plain text under a strict policy; WebKit draws such text with an inline
  // style of its own, which the policy blocks. That page is not the application.
  victim.foreign(numericPage);
  const numericVisit = await numericPage.goto(`${numeric}/`).then(
    (r) => ({ status: r?.status() ?? null }),
    (e) => ({ error: String(e.message).split("\n")[0].slice(0, 80) }),
  );
  equal(await numericPage.locator("canvas#term, .lg-box").count(), 0, "the numeric address shows no page of the application");
  await numericPage.close();
  const plainPage = await victim.context.newPage();
  victim.foreign(plainPage);
  const plainVisit = await plainPage.goto(`http://${hostname}:${harness.port}/`, { timeout: 15_000 }).then(
    (r) => ({ status: r?.status() ?? null }),
    (e) => ({ error: String(e.message).split("\n")[0].slice(0, 80) }),
  );
  equal(await plainPage.locator("canvas#term, .lg-box").count(), 0, "plain http shows no page of the application");
  await plainPage.close();

  // ---- no answer of the server allows another origin -----------------------------------------------------------------------------
  check(answers.checked > 20, `answers of the server were checked (${answers.checked})`);
  equal(answers.withCors.join(", "), "", "answers with an access-control header (none expected)");

  // ---- a whole signed-in session reaches the harness origin and nothing else ----------------------------------------------------------------
  const clean = await open();
  const cleanPage = await clean.context.newPage();
  await signIn(cleanPage, harness, admin);
  const socket = await oracle(cleanPage);
  const pane = (await poll(async () => {
    const layout = await socket.layout();
    return layout.panes?.length > 0 ? layout : undefined;
  }, "the application shows a pane", 20_000)).focus;
  await cleanPage.locator("canvas#term").click();
  await cleanPage.keyboard.type("printf 'hw-%s\\n' origins");
  await cleanPage.keyboard.press("Enter");
  await poll(async () => ((await socket.screen(pane)).split("\n").some((l) => l.trim() === "hw-origins") ? true : undefined), "the typed command printed its line");
  await socket.close();
  await openPeople(cleanPage);
  clean.expectClean("a whole signed-in session");
  const hosts = [...clean.recorder.hosts].join(", ");
  equal(hosts, harness.host, "the hosts a whole session reached");

  victim.expectClean("the victim's context");
  console.log(
    `origins: same site (other port): sign-out ${attacks["same site, other port"].logout}, account change ${attacks["same site, other port"].admin}, blind form posts for the sign-out and the account change (the server answers cannot be read; the effect is checked), read ${attacks["same site, other port"].read}, socket closed ${attacks["same site, other port"].socket.code} with ${attacks["same site, other port"].socket.messages} messages; ` +
      `other site: socket closed ${attacks["other site"].socket.code}, read ${attacks["other site"].read}; numeric host: socket closed ${attacks["numeric host"].socket.code}; ` +
      `the victim's session, socket and account list were unchanged; numeric address page: ${JSON.stringify(numericVisit)}; plain http: ${JSON.stringify(plainVisit)}; ` +
      `${answers.checked} answers checked, access-control headers 0; ` +
      `cookie __Host-hw_session httpOnly=${cookie.httpOnly} secure=${cookie.secure} sameSite=${cookie.sameSite} path=${cookie.path}, document.cookie empty; hosts of a whole session: ${hosts}`,
  );
}, {
  // Chromium refuses a request from another origin to a loopback address by itself (Local Network Access). The attacker pages
  // here must reach the server to test it, so that one check is off for this file; Chromium and WebKit are otherwise as for the
  // other files. A user's browser has both layers.
  chromiumArgs: ["--disable-features=LocalNetworkAccessChecks,LocalNetworkAccessChecksWebSockets,LocalNetworkAccessChecksWebTransport,BlockInsecurePrivateNetworkRequests"],
});
