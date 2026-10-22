// What the page knows about the card of an integration action (src/integratestate.ts, built into dist/): the reader of the
// card the core prepared, the words of every mode and the first characters of the hash. No DOM is needed. The golden card is the
// one the core's own test writes, so the page and the core cannot drift apart.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  CARD_WORDS, HALF_SECOND, actionLabel, asksFirst, hashShown, heldText, localVerbFor, modeWords, nextMode, readCard, readTools, refusalOf,
} from "../../dist/integratestate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const HASH = "9f3a41c2d7be0123456789abcdef0123456789abcdef0123456789abcdef0123";
const TICKET = "0123456789abcdef0123456789abcdef";

/** A card as the core spells it, built by the same rule the core builds its fields by. */
function card(over = {}) {
  const argv = over.argv ?? ["delete", "pod", "--context=prod-eu", "--namespace=payments", "--wait=false", "--request-timeout=10s", "--", "api-0"];
  const target = over.target ?? [["context", "prod-eu"], ["namespace", "payments"], ["pod", "api-0"]];
  const production = "production" in over ? over.production : null;
  const effect = over.effect ?? "write";
  const fields = [
    ["tool", over.tool ?? "kube"], ["verb", over.verb ?? "delete_pod"], ["effect", effect], ["program", over.program ?? "kubectl"],
    ...argv.map((a, i) => [`arg ${i + 1}`, a]),
    ...target,
    ["guard", production === null ? "none" : "production"],
    ...(production === null ? [] : [["production_name", production]]),
  ];
  return {
    ticket: TICKET, tool: over.tool ?? "kube", verb: over.verb ?? "delete_pod", effect, program: over.program ?? "kubectl", argv, target,
    guard: { production }, fields: over.fields ?? fields, hash: HASH, expires_in_secs: 120, ...("raw" in over ? over.raw : {}),
  };
}

test("the reader keeps the golden card whole, in the core's order", () => {
  const read = readCard(golden.answers.integration.card);
  assert.ok(read);
  assert.equal(read.ticket, golden.answers.integration.card.ticket);
  assert.equal(read.hash, golden.answers.integration.card.hash);
  assert.deepEqual([read.tool, read.verb, read.effect, read.program], ["kube", "delete_pod", "write", "kubectl"]);
  assert.deepEqual(read.argv, ["delete", "pod", "--wait=false", "--request-timeout=10s", "--", "api-0"]);
  assert.deepEqual(read.target, [["context", "prod"], ["namespace", "payments"], ["pod", "api-0"]]);
  assert.equal(read.production, "Production");
  assert.equal(read.expires, 120);
});

test("the reader keeps a card without a production guard and a Local card with its changes pair", () => {
  const plain = readCard(card());
  assert.ok(plain);
  assert.equal(plain.production, null);
  const local = readCard(
    card({
      tool: "kube", verb: "use_context", effect: "local", argv: ["config", "use-context", "--", "dev"],
      target: [["context", "dev"], ["changes", "the kubeconfig files named by KUBECONFIG, by default ~/.kube/config"]],
    }),
  );
  assert.ok(local);
  assert.deepEqual(local.target.map(([name]) => name), ["context", "changes"]);
});

test("the reader gives null for anything that is not the contract", () => {
  const good = card();
  const without = (key) => ({ ...good, [key]: undefined });
  for (const bad of [null, undefined, "card", 4, [], {}, without("ticket"), without("hash"), without("fields"), without("argv"), without("target")]) {
    assert.equal(readCard(bad), null, JSON.stringify(bad)?.slice(0, 60));
  }
  assert.equal(readCard({ ...good, ticket: "" }), null, "an empty ticket");
  assert.equal(readCard({ ...good, ticket: 7 }), null, "a ticket that is no text");
  assert.equal(readCard({ ...good, hash: HASH.slice(1) }), null, "a hash of 63 characters");
  assert.equal(readCard({ ...good, hash: HASH.toUpperCase() }), null, "a hash that is not lower case hex");
  assert.equal(readCard({ ...good, hash: `${HASH.slice(0, 63)}g` }), null, "a hash that is not hex");
  assert.equal(readCard({ ...good, effect: "read" }), null, "a read has no card");
  assert.equal(readCard({ ...good, effect: "nuke" }), null, "an effect the page does not know");
  assert.equal(readCard({ ...good, fields: good.fields.map(([a]) => a) }), null, "fields that are no pairs");
  assert.equal(readCard({ ...good, fields: good.fields.map(([a, b]) => [a, b, "x"]) }), null, "fields of the wrong shape");
  assert.equal(readCard({ ...good, fields: good.fields.map(([a, b]) => [a, typeof b === "string" ? 4 : b]) }), null, "values that are no text");
  assert.equal(readCard({ ...good, argv: ["a", 4] }), null, "an argument that is no text");
  assert.equal(readCard({ ...good, target: [["context"]] }), null, "a target pair of the wrong shape");
  assert.equal(readCard({ ...good, guard: null }), null, "no guard object");
  assert.equal(readCard({ ...good, guard: { production: "" } }), null, "a production guard without a name");
  assert.equal(readCard({ ...good, guard: { production: 4 } }), null, "a production name that is no text");
});

test("the reader refuses more than 64 fields and a value over 8192 characters", () => {
  const argv = Array.from({ length: 70 }, (_, i) => `a${i}`);
  assert.equal(readCard(card({ argv })), null, "70 arguments make more than 64 fields");
  const sixty = Array.from({ length: 52 }, (_, i) => `a${i}`);
  assert.ok(readCard(card({ argv: sixty })), "52 arguments and the rest are 64 fields or fewer");
  assert.equal(readCard(card({ argv: ["delete", "x".repeat(8193)] })), null, "a value over 8192 characters");
  assert.ok(readCard(card({ argv: ["delete", "x".repeat(8192)] })), "a value of exactly 8192 characters");
});

test("the members must agree with the fields the hash covers", () => {
  const good = card();
  // The program or an argument that differs from its field, a target that is not in the fields, a missing guard field.
  assert.equal(readCard({ ...good, program: "other" }), null);
  assert.equal(readCard({ ...good, argv: [...good.argv.slice(0, -1), "api-1"] }), null);
  assert.equal(readCard({ ...good, target: [["context", "other"]] }), null);
  assert.equal(readCard({ ...good, fields: good.fields.filter(([name]) => name !== "guard") }), null);
  assert.equal(readCard({ ...good, tool: "talos" }), null);
  assert.equal(readCard({ ...good, guard: { production: "Production" } }), null, "a guard the fields do not carry");
});

test("the hash is shown by its first twelve characters", () => {
  assert.equal(hashShown(HASH), "9f3a41c2d7be");
  assert.equal(hashShown(HASH).length, 12);
  assert.equal(hashShown(golden.answers.integration.card.hash), "0123456789ab");
});

test("every mode has words, and the words of an outcome come from the core", () => {
  assert.equal(CARD_WORDS.head, "Confirm needed");
  assert.equal(modeWords("ask"), "");
  assert.equal(modeWords("armed"), "Armed. The next press allows this on production. Nothing has run yet.");
  assert.equal(modeWords("sending"), "Allowed · running…");
  assert.equal(modeWords("done", { kind: "ran", note: 'pod "api-0" deleted', pane: null }), 'Done: pod "api-0" deleted');
  assert.equal(modeWords("done", { kind: "launched", note: "Exec in pod api-0", pane: 5 }), "Opened pane 5");
  assert.equal(modeWords("done", { kind: "launched", note: "x", pane: null }), "Opened a pane");
  assert.equal(modeWords("done", { kind: "ran", note: "", pane: null }), "Done.", "a verb that said nothing");
  assert.equal(modeWords("ended"), "That action is no longer waiting.");
  assert.equal(modeWords("changed"), "The request changed. Nothing ran. Read the card again.");
  assert.equal(modeWords("cannot"), "This request could not be read in full. It can only be denied.");
});

// ---- the machine of the modes -------------------------------------------------------------------------------------------------

const MODES = ["ask", "armed", "sending", "done", "ended", "changed", "cannot"];
const allow = (over = {}) => ({ type: "allow", now: 10_000, shownAt: 0, armedAt: null, trusted: true, production: false, ...over });

test("an ordinary card is allowed by one real press after the half second", () => {
  assert.equal(HALF_SECOND, 500);
  assert.deepEqual(nextMode("ask", allow()), { mode: "sending", act: "confirm" });
  assert.deepEqual(nextMode("ask", allow({ now: 500 })), { mode: "sending", act: "confirm" }, "exactly half a second counts");
  assert.deepEqual(nextMode("ask", allow({ now: 499 })), { mode: "ask", act: "none" }, "one millisecond too early");
  assert.deepEqual(nextMode("ask", allow({ trusted: false })), { mode: "ask", act: "none" }, "a press a script made");
});

test("a production card arms on the first press and is allowed by the second", () => {
  assert.deepEqual(nextMode("ask", allow({ production: true })), { mode: "armed", act: "arm" });
  assert.deepEqual(nextMode("ask", allow({ production: true, now: 499 })), { mode: "ask", act: "none" });
  assert.deepEqual(nextMode("ask", allow({ production: true, trusted: false })), { mode: "ask", act: "none" });
  assert.deepEqual(nextMode("armed", allow({ production: true, armedAt: 5000 })), { mode: "sending", act: "confirm" });
  assert.deepEqual(nextMode("armed", allow({ production: true, armedAt: 9501 })), { mode: "armed", act: "none" }, "a double click arms and cannot allow");
  assert.deepEqual(nextMode("armed", allow({ production: true, armedAt: 9500 })), { mode: "sending", act: "confirm" });
  assert.deepEqual(nextMode("armed", allow({ production: true, armedAt: 5000, trusted: false })), { mode: "armed", act: "none" });
});

test("only an unarmed ordinary card or an armed card can reach a confirm, in every combination", () => {
  let confirms = 0;
  for (const mode of MODES) {
    for (const trusted of [true, false]) {
      for (const production of [true, false]) {
        for (const now of [0, 499, 500, 10_000]) {
          for (const armedAt of [null, now, now - 499, now - 500]) {
            const next = nextMode(mode, allow({ mode, trusted, production, now, shownAt: 0, armedAt }));
            if (next.act !== "confirm") {
              if (mode !== "sending") assert.notEqual(next.mode, "sending", "sending only through a confirm");
              continue;
            }
            confirms += 1;
            assert.equal(trusted, true, "a script never confirms");
            assert.ok(now >= 500, "never within the half second");
            assert.ok(mode === "armed" || (mode === "ask" && !production), `${mode} production=${production}`);
            if (mode === "armed") assert.ok(armedAt === null || now - armedAt >= 500, "never within half a second of arming");
          }
        }
      }
    }
  }
  assert.ok(confirms > 0, "the loop reached a confirm");
});

test("deny counts for a real press in the modes that have the button, whenever it is made", () => {
  for (const mode of ["ask", "armed", "cannot", "changed"]) {
    assert.deepEqual(nextMode(mode, { type: "deny", trusted: true }), { mode: "ended", act: "cancel" }, mode);
    assert.deepEqual(nextMode(mode, { type: "deny", trusted: false }), { mode, act: "none" }, `${mode} by a script`);
  }
  for (const mode of ["sending", "done", "ended"]) {
    assert.deepEqual(nextMode(mode, { type: "deny", trusted: true }), { mode, act: "none" }, mode);
  }
});

test("the answers of the core move a card that is sending, and no other", () => {
  assert.deepEqual(nextMode("sending", { type: "ran" }), { mode: "done", act: "none" });
  assert.deepEqual(nextMode("sending", { type: "refused", refusal: "ended" }), { mode: "ended", act: "none" });
  assert.deepEqual(nextMode("sending", { type: "refused", refusal: "changed" }), { mode: "changed", act: "none" });
  assert.deepEqual(nextMode("sending", { type: "refused", refusal: "other" }), { mode: "ask", act: "none" }, "disarmed: a new decision is needed");
  for (const mode of MODES.filter((m) => m !== "sending")) {
    assert.deepEqual(nextMode(mode, { type: "ran" }), { mode, act: "none" }, mode);
    assert.deepEqual(nextMode(mode, { type: "refused", refusal: "ended" }), { mode, act: "none" }, mode);
  }
  assert.deepEqual(nextMode("changed", { type: "fresh" }), { mode: "ask", act: "none" });
  for (const mode of MODES.filter((m) => m !== "changed")) assert.deepEqual(nextMode(mode, { type: "fresh" }), { mode, act: "none" }, mode);
});

test("a ticket that runs out ends a card that waits, never one that is sending or finished", () => {
  for (const mode of ["ask", "armed", "changed", "cannot"]) assert.deepEqual(nextMode(mode, { type: "expired" }), { mode: "ended", act: "none" }, mode);
  for (const mode of ["sending", "done", "ended"]) assert.deepEqual(nextMode(mode, { type: "expired" }), { mode, act: "none" }, mode);
});

test("the refusals of the core are told apart by code and reason", () => {
  const ticket = (reason) => ({ code: "ticket", reason, text: "x" });
  assert.equal(refusalOf(ticket("unknown")), "ended");
  assert.equal(refusalOf(ticket("expired")), "ended");
  assert.equal(refusalOf(ticket("changed")), "changed");
  assert.equal(refusalOf(ticket("mismatch")), "changed");
  assert.equal(refusalOf(ticket("refused")), "other");
  assert.equal(refusalOf(ticket("")), "other");
  assert.equal(refusalOf({ code: "failed", reason: "unknown", text: "x" }), "other", "the reason alone says nothing");
  assert.equal(refusalOf({ code: "refused", reason: "expired", text: "x" }), "other");
});

// ---- the row actions, the Make default entries and the held header ----------------------------------------------------------

const verb = (id, label, effect, args = []) => ({ id, label, effect, output: effect === "read" ? "view" : "done", refresh_secs: null, exit_is_state: false, args });
const arg = (name, kind, required = true) => ({ name, kind, required });
const entry = (id, verbs) => readTools([{ id, label: id, state: "found", path: "/x", binary: null, reason: null, version: null, searched: [], selects: [], verbs }])[0];

test("the tool list keeps the arguments of a verb with their kinds", () => {
  const tool = entry("kube", [verb("use_context", "Use context", "local", [arg("context", "context"), arg("junk", 5), "x"]), verb("pods", "Pods", "read")]);
  assert.deepEqual(tool.verbs[0].args, [{ name: "context", kind: "context", required: true }]);
  assert.deepEqual(tool.verbs[1].args, []);
});

test("the Local verb of a kind is found from the core's list by the kind of its first argument, by no name", () => {
  const kube = entry("kube", [
    verb("contexts", "Contexts", "read"),
    verb("switch_ctx", "Switch context", "local", [arg("context", "context")]),
    verb("set_namespace", "Set namespace", "local", [arg("namespace", "namespace")]),
    verb("delete_pod", "Delete pod", "write", [arg("pod", "pod")]),
  ]);
  assert.deepEqual(localVerbFor(kube, "context"), { verb: "switch_ctx", arg: "context", label: "Switch context" });
  assert.deepEqual(localVerbFor(kube, "namespace"), { verb: "set_namespace", arg: "namespace", label: "Set namespace" });
  assert.equal(localVerbFor(kube, "node"), null, "no verb for the kind");
  assert.equal(localVerbFor(kube, "section"), null);
  // A verb that writes or reads is never a way to make something the default, whatever its first argument is.
  const writer = entry("talos", [verb("zap", "Zap", "write", [arg("context", "context")]), verb("peek", "Peek", "read", [arg("context", "context")])]);
  assert.equal(localVerbFor(writer, "context"), null);
  // Only a required first argument counts, and only the first.
  const optional = entry("azure", [verb("use", "Use", "local", [arg("context", "context", false)]), verb("two", "Two", "local", [arg("a", "pod"), arg("context", "context")])]);
  assert.equal(localVerbFor(optional, "context"), null);
  assert.equal(localVerbFor(null, "context"), null);
});

test("an action that asks first ends in an ellipsis and says so", () => {
  assert.equal(actionLabel({ label: "Logs", effect: "read" }), "Logs");
  assert.equal(actionLabel({ label: "Exec", effect: "exec" }), "Exec…");
  assert.equal(actionLabel({ label: "Delete", effect: "write" }), "Delete…");
  assert.equal(actionLabel({ label: "Use context", effect: "local" }), "Use context…");
  assert.equal(actionLabel({ label: "Exec…", effect: "exec" }), "Exec…", "not twice");
  assert.deepEqual(["read", "local", "write", "exec"].map(asksFirst), [false, true, true, true]);
});

test("a held terminal says exited with its status", () => {
  assert.equal(heldText(1), "exited 1");
  assert.equal(heldText(0), "exited 0");
  assert.equal(heldText(137), "exited 137");
  assert.equal(heldText(null), "exited");
});
