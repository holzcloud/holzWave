// Shared set-up of the confirmation card tests (not a test itself: the runner only runs *.test.mjs). A mock core answers
// `integration_prepare`, `integration_confirm` and `integration_cancel` from `window.__IC__`, so a test can change what the core
// says while the page runs, and the calls of the page are read back with `callsOf`. The page is the one of the block tests (a
// shell and a pods block), so a row action can start a card, or a test calls `requestAction` of the module itself.
import { KUBE, LAYOUT, openBlock } from "./integrate-lib.mjs";

export { KUBE, LAYOUT };

export const TICKET = "0123456789abcdef0123456789abcdef";
export const HASH = "9f3a41c2d7be0123456789abcdef0123456789abcdef0123456789abcdef0123";
export const HOSTILE = "<img src=x onerror=alert(1)>";

/** A card as the core spells it: the fields are built by the rule the core builds them by, in the core's order. */
export function cardOf(over = {}) {
  const argv = over.argv ?? ["delete", "pod", "--context=prod-eu", "--namespace=payments", "--wait=false", "--request-timeout=10s", "--", "api-0"];
  const target = over.target ?? [["context", "prod-eu"], ["namespace", "payments"], ["pod", "api-0"]];
  const production = "production" in over ? over.production : null;
  const effect = over.effect ?? "write";
  const tool = over.tool ?? "kube";
  const verb = over.verb ?? "delete_pod";
  const program = over.program ?? "kubectl";
  const fields = [
    ["tool", tool], ["verb", verb], ["effect", effect], ["program", program],
    ...argv.map((a, i) => [`arg ${i + 1}`, a]),
    ...target,
    ["guard", production === null ? "none" : "production"],
    ...(production === null ? [] : [["production_name", production]]),
  ];
  return {
    ticket: over.ticket ?? TICKET, tool, verb, effect, program, argv, target, guard: { production }, fields, hash: over.hash ?? HASH,
    expires_in_secs: over.expires_in_secs ?? 120,
  };
}

/** The confirm answer of a verb that ran. */
export const RAN = { kind: "ran", pane: null, note: 'pod "api-0" deleted', view: null };

/** The confirm answer of a verb that opened a pane: the layout has a third pane (5), a terminal beside the others. */
export const OPENED = (view) => ({ kind: "launched", pane: 5, note: "Exec in pod api-0", view });

export const ticketFault = (reason, text = "The action is not waiting.") => ({ code: "ticket", reason, text });

/**
 * Script the core. `config` is merged into `window.__IC__`:
 *   cards     the card by `tool/verb`: an object, or an array handed out one by one (the last one repeats)
 *   prepareFault   a fault by `tool/verb`, thrown instead of a card
 *   steps     what `integration_confirm` does, one entry for each call: `{answer}` or `{fault}`; the last repeats; default RAN
 *   hold      true: a confirm waits until `window.__IC_RELEASE__()` is called
 */
export async function installCore(page, config = {}) {
  const base = { cards: { "kube/delete_pod": cardOf() }, prepareFault: {}, steps: [{ answer: RAN }], hold: false, confirms: 0, prepares: 0 };
  await page.addInitScript((cfg) => {
    const M = window.__MOCK__;
    window.__IC__ = cfg;
    M.on("integration_prepare", (a) => {
      const ic = window.__IC__;
      const key = `${a.tool}/${a.verb}`;
      ic.prepares += 1;
      if (ic.prepareFault[key]) throw ic.prepareFault[key];
      const entry = ic.cards[key];
      if (entry === undefined) throw { code: "refused", reason: "verb", text: "The verb is not known." };
      const card = Array.isArray(entry) ? (entry.length > 1 ? entry.shift() : entry[0]) : entry;
      return structuredClone(card);
    });
    M.on("integration_confirm", async () => {
      const ic = window.__IC__;
      ic.confirms += 1;
      if (ic.hold) await new Promise((resolve) => (window.__IC_RELEASE__ = resolve));
      const step = ic.steps.length > 1 ? ic.steps.shift() : ic.steps[0];
      if (step.fault) throw step.fault;
      return structuredClone(step.answer);
    });
    M.on("integration_cancel", () => null);
  }, { ...base, ...config });
}

/** Open the page with the shell and the pods block, and the core scripted. `block` is merged into the block set-up. */
export async function openConfirm(page, url, config = {}, block = {}, layout = LAYOUT) {
  await installCore(page, config);
  await openBlock(page, url, block, layout);
}

/** Ask for a card through the page's own function, without waiting for the decision; the outcome lands in `window.__RES__`. */
export async function request(page, tool = "kube", verb = "delete_pod", pane = null, args = { pod: "api-0" }) {
  await page.evaluate(([t, v, p, a]) => {
    window.__RES__ = undefined;
    void import("./integrateconfirm.js").then((m) => m.requestAction(t, v, p, a)).then((out) => {
      window.__RES__ = out;
    });
  }, [tool, verb, pane, args]);
}

/** The outcome of the last `request`, once it has one. */
export async function outcome(page) {
  await page.waitForFunction(() => window.__RES__ !== undefined);
  return page.evaluate(() => JSON.parse(JSON.stringify(window.__RES__)));
}

export const calls = (page, cmd) => page.evaluate((c) => window.__MOCK__.callsOf(c), cmd);
export const cardNode = (page) => page.locator("section.ic");
export const textOf = (page, selector) => page.locator(`section.ic ${selector}`).textContent();
export const gone = (page) => page.waitForFunction(() => document.querySelector("section.ic") === null);
export const wait = (page, ms) => page.waitForTimeout(ms);
