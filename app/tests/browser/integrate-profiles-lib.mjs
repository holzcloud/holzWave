// Shared set-up of the integration profiles tests (not a test itself: the runner only runs *.test.mjs). A mock core answers
// `integration_profiles`, `integration_profile_save`, `integration_profile_remove`, `integration_secret_set` and
// `integration_secret_clear` from `window.__PF__`, with the rules the real core is described by in the golden: a save takes the
// file's shape (`secret_env` is a list of names), the list answers `secret_env` as `[{name, has_value}]`, a value goes in only
// through `integration_secret_set` and the mock keeps a flag and never the value. The page is the one of the context tests (the
// sidebar is open), so the dialog is opened from the Context section as a person opens it.
import { ALL_TOOLS, KUBE_ROW, ROWS, openContext, sectionShown } from "./integrate-context-lib.mjs";

export { ALL_TOOLS, KUBE_ROW, ROWS, openContext, sectionShown };

export const HOSTILE = "<img src=x onerror=alert(1)>";
export const CANARY = "zq-profile-secret-5d81c3e9a7";

/** A profile as the core lists it. */
export const profile = (over = {}) => ({
  id: "prod-eu", tool: "kube", name: "Production EU", context: "prod-eu", namespace: "payments", subscription: null, section: null,
  node: null, production: true, isolated: false, env: {}, secret_env: [], ...over,
});

export const PROD = profile({ env: { AWS_PROFILE: "prod" }, secret_env: [{ name: "AWS_SECRET_ACCESS_KEY", has_value: true }] });
export const STAGING = profile({ id: "staging", name: "Staging", context: "stg", namespace: "default", production: false });
export const AZURE_PROFILE = profile({
  id: "holz-azure", tool: "azure", name: "Holz Azure", context: null, namespace: null, subscription: "00000000-0000-0000-0000-000000000000",
  production: false, isolated: true,
});

/**
 * Script the core. `config` is merged into `window.__PF__`:
 *   profiles    the profiles the core lists (core shape)
 *   notes       the sentences of the core about its file
 *   saveFaults  faults thrown by `integration_profile_save`, one for each call, in order (then it works)
 *   setFault / clearFault / removeFault   a fault thrown by that command
 *   listFault   a fault thrown by `integration_profiles`
 *   setDelay    milliseconds `integration_secret_set` waits before it answers
 * The mock keeps `window.__PF__.values`, a map `profile:name` to true for a name that has a value, and never a value.
 */
export async function installProfiles(page, config = {}) {
  const base = { profiles: [PROD, STAGING], notes: [], saveFaults: [], setFault: null, clearFault: null, removeFault: null, listFault: null, setDelay: 0, values: {} };
  await page.addInitScript((cfg) => {
    const M = window.__MOCK__;
    window.__PF__ = cfg;
    for (const p of cfg.profiles) for (const s of p.secret_env) if (s.has_value) cfg.values[`${p.id}:${s.name}`] = true;
    const view = () => {
      const pf = window.__PF__;
      return {
        notes: structuredClone(pf.notes),
        profiles: structuredClone(pf.profiles).map((p) => ({
          ...p,
          secret_env: p.secret_env.map((s) => ({ name: s.name, has_value: pf.values[`${p.id}:${s.name}`] === true })),
        })),
      };
    };
    const fault = (text, reason = "invalid") => ({ code: "profile", reason, text });
    M.on("integration_profiles", () => {
      if (window.__PF__.listFault) throw window.__PF__.listFault;
      return view();
    });
    M.on("integration_profile_save", (a) => {
      const pf = window.__PF__;
      const next = pf.saveFaults.shift();
      if (next) throw next;
      const given = a.profile;
      const known = Object.keys(given).sort().join(",");
      if (known !== "context,env,id,isolated,name,namespace,node,production,secret_env,section,subscription,tool") {
        throw fault("The profile is not valid.");
      }
      const entry = { ...given, secret_env: given.secret_env.map((name) => ({ name, has_value: false })) };
      const at = pf.profiles.findIndex((p) => p.id === given.id);
      if (at < 0) pf.profiles.push(entry);
      else pf.profiles[at] = entry;
      return view();
    });
    M.on("integration_profile_remove", (a) => {
      const pf = window.__PF__;
      if (pf.removeFault) throw pf.removeFault;
      pf.profiles = pf.profiles.filter((p) => p.id !== a.id);
      for (const k of Object.keys(pf.values)) if (k.startsWith(`${a.id}:`)) delete pf.values[k];
      return view();
    });
    M.on("integration_secret_set", async (a) => {
      const pf = window.__PF__;
      if (pf.setDelay > 0) await new Promise((r) => setTimeout(r, pf.setDelay));
      if (pf.setFault) throw pf.setFault;
      const p = pf.profiles.find((q) => q.id === a.profile);
      if (!p || !p.secret_env.some((s) => s.name === a.name)) throw fault(`profile ${a.profile}: ${a.name} is not a secret variable of the profile`, "secret");
      pf.values[`${a.profile}:${a.name}`] = true;
      return null;
    });
    M.on("integration_secret_clear", (a) => {
      const pf = window.__PF__;
      if (pf.clearFault) throw pf.clearFault;
      delete pf.values[`${a.profile}:${a.name}`];
      return null;
    });
  }, { ...base, ...config });
}

/** Open the page with the mock core and the sidebar shown, and wait for the Context section. */
export async function openPage(page, url, config = {}, context = {}) {
  await installProfiles(page, config);
  await openContext(page, url, { tools: ALL_TOOLS, rows: ROWS, ...context });
  await sectionShown(page);
}

/** Press `Profiles…` and wait until the dialog has read the list. */
export async function openDialog(page) {
  await page.locator(".cx-profiles").click();
  await page.waitForSelector(".dlg.ip-box");
  await page.waitForFunction(() => document.querySelector(".dlg.ip-box")?.contains(document.activeElement));
  await page.waitForFunction(() => document.querySelector(".dlg.ip-box [aria-busy]") === null);
}

export const callsOf = (page, cmd) => page.evaluate((c) => window.__MOCK__.callsOf(c), cmd);
export const countOf = async (page, cmd) => (await callsOf(page, cmd)).length;

/** The text of the page and everything that could carry a value: text, attribute values and the field values themselves. */
export const pageText = (page) =>
  page.evaluate(() => {
    const out = [document.body.innerText, document.documentElement.outerHTML];
    for (const node of document.querySelectorAll("input, textarea")) out.push(node.value);
    return out.join("\n");
  });
