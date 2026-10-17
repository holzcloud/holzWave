// The pure half of the on-device row (ondevice.ts): how the member `on_device` of the answer of `ai_profiles` is read, when
// the row joins the list of providers, how a pick of it is judged and what a blocked send says. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  DEVICE_ID, DEVICE_MODEL, DEVICE_NAME, DEVICE_OFF, deviceAvailable, deviceChoice, readDevice, withDevice,
} from "../../dist/ondevice.js";
import { chipView, pickProfile } from "../../dist/chatview.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/ai_device.json", import.meta.url), "utf8"));

/** The hostile word of these tests; it appears in test code only. */
const CANARY = "zq-device-state-5e1c";

const answer = (onDevice) => ({ profiles: [], notes: [], on_device: onDevice });
const good = { id: "apple", model: "system", state: "available", advice: "none" };
const entry = (id, extra = {}) => ({ id, name: `Name ${id}`, model: "llama3.2", host: "localhost", ...extra });

test("the device view is read member by member and only the exact word available is available", () => {
  const view = readDevice(answer(good));
  assert.deepEqual(view, { state: "available", advice: "none" });
  assert.deepEqual(Object.keys(view).sort(), ["advice", "state"], "exactly those two own members");
  assert.equal(Object.getPrototypeOf(view), Object.prototype);

  // Extra members keep nothing, and an own __proto__ member is never copied.
  const extra = readDevice(JSON.parse(`{"on_device": {"id": "apple", "model": "system", "state": "available", "advice": "none",
    "api_key": "k", "locale": "de", "__proto__": {"polluted": true}}}`));
  assert.deepEqual(extra, { state: "available", advice: "none" });
  assert.equal(Object.hasOwn(extra, "__proto__"), false);
  assert.equal(extra.polluted, undefined);
  assert.equal(Object.keys(extra).length, 2);

  // The wrong id or model, a missing, null, array, string or number member, and a raw value that is no object give null.
  for (const bad of [
    answer({ ...good, id: "evil" }),
    answer({ ...good, id: "Apple" }),
    answer({ ...good, model: "other" }),
    answer({ state: "available", advice: "none" }),
    answer(null),
    answer([good]),
    answer("available"),
    answer(7),
    { profiles: [], notes: [] },
    {},
    [],
    null,
    undefined,
    "on_device",
    7,
  ]) {
    assert.equal(readDevice(bad), null, JSON.stringify(bad));
  }
  // The member must be the answer's own, not an inherited one.
  assert.equal(readDevice(Object.create({ on_device: good })), null);

  // A state that is no short lower-case word reads as `other` with the advice `unknown`; markup never survives.
  const longWord = "a".repeat(49);
  for (const state of [7, null, undefined, "", longWord, "Available", "avail able", "avail4ble", `<img src=x ${CANARY}>`, "available\n", " available"]) {
    const read = readDevice(answer({ ...good, state }));
    assert.deepEqual(read, { state: "other", advice: "unknown" }, String(state));
    assert.ok(!JSON.stringify(read).includes(CANARY));
  }
  assert.deepEqual(readDevice(answer({ ...good, state: "a".repeat(48) })), { state: "a".repeat(48), advice: "none" });
  // An invalid advice reads as `unknown` and keeps a valid state.
  for (const advice of [7, null, undefined, "", "Enable", "x".repeat(49), `javascript:${CANARY}`, "none "]) {
    assert.deepEqual(readDevice(answer({ ...good, state: "model_not_ready", advice })), { state: "model_not_ready", advice: "unknown" });
  }

  // Only the state `available` is available.
  assert.equal(deviceAvailable({ state: "available", advice: "none" }), true);
  assert.equal(deviceAvailable(null), false);
  for (const { state, advice } of golden.states) assert.equal(deviceAvailable({ state, advice }), state === "available", state);
  for (const state of [...golden.unsent, "Available", "available ", "other", "unknown", ""]) {
    assert.equal(deviceAvailable({ state, advice: "none" }), false, state);
  }
});

test("the device row joins the list only when available and a pick of it needs a loaded list", () => {
  const a = entry("a");
  const b = entry("b");
  const available = { state: "available", advice: "none" };
  const row = { id: "apple", name: "Apple on-device", model: "system", host: "" };
  const list = [a, b];
  const joined = withDevice(list, available);
  assert.deepEqual(joined, [a, b, row]);
  assert.deepEqual(list, [a, b], "the argument is left as it was");
  assert.notEqual(joined, list);
  assert.deepEqual(deviceChoice(), row);
  for (const view of [null, { state: "model_not_ready", advice: "wait_for_model" }, { state: "Available", advice: "none" }]) {
    const copy = withDevice(list, view);
    assert.deepEqual(copy, [a, b]);
    assert.notEqual(copy, list, "a plain copy");
  }

  // The device row is picked like any row of a loaded list.
  const both = [a, b, row];
  assert.deepEqual(pickProfile(both, "apple", null), { id: "apple", via: "choice" });
  assert.deepEqual(pickProfile(both, null, "apple"), { id: "apple", via: "pin" });
  assert.deepEqual(pickProfile([row], null, null), { id: "apple", via: "only" });
  assert.deepEqual(pickProfile(both, null, null), { id: "", via: "ask" });

  // A pick of it needs a loaded list.
  assert.deepEqual(pickProfile(null, "apple", null), { id: "", via: "unknown" });
  assert.deepEqual(pickProfile(null, null, "apple"), { id: "", via: "unknown" });
  assert.deepEqual(pickProfile(null, "b", null), { id: "b", via: "choice" }, "another id behaves as before");
  assert.deepEqual(pickProfile(null, null, "b"), { id: "b", via: "pin" });

  // Over a loaded list without the entry the pin is refused by name and no id is put in its place.
  assert.deepEqual(pickProfile(list, null, "apple"), { id: "", via: "device_off" });
  assert.deepEqual(pickProfile([a], null, "apple"), { id: "", via: "device_off" });
  assert.deepEqual(pickProfile([], null, "apple"), { id: "", via: "device_off" });
  assert.deepEqual(pickProfile(list, "a", "apple"), { id: "a", via: "choice" }, "a listed choice still wins");
  assert.deepEqual(pickProfile(list, null, "gone"), { id: "gone", via: "pin" }, "another pin that is not listed is still a pin");
  // A choice of the device that is gone is never turned into another provider: the window remembers it, and a send says so.
  assert.deepEqual(pickProfile(list, "apple", null), { id: "", via: "device_off" });
  assert.deepEqual(pickProfile([a], "apple", null), { id: "", via: "device_off" });
  assert.deepEqual(pickProfile(list, "apple", "apple"), { id: "", via: "device_off" });
  assert.deepEqual(pickProfile(list, "apple", "b"), { id: "b", via: "pin" }, "a pin of another provider is the chat's own choice");
});

test("the chip names the device even when the list has no entry for it", () => {
  const nopin = { profile: null, model: null };
  const local = entry("local-1", { name: "Local" });
  const cloud = entry("other-1", { name: "Cloud", model: "big-model", host: "api.example.org" });
  const told = { profile: "apple", model: "system", host: null, locality: "on_device" };

  const route = chipView(told, [local], { profile: "apple", model: "system" }, null);
  assert.equal(route.name, DEVICE_NAME);
  assert.equal(route.model, "system");
  assert.equal(route.host, "");
  assert.equal(route.where, "on device");
  assert.equal(route.leaves, false);
  assert.ok(!route.text.includes("→"), "no arrow");
  assert.equal(route.text, "Apple on-device · system");

  // A route for the device when the list does hold the row says the same.
  const listed = chipView(told, [local, deviceChoice()], nopin, "apple");
  assert.deepEqual([listed.name, listed.model, listed.where, listed.text], ["Apple on-device", "system", "on device", "Apple on-device · system"]);
  assert.equal(listed.kind, "button");

  // A pin with no route: the name and the model, no place word. Also while the list is not read yet.
  const pinned = chipView(null, [local, cloud], { profile: "apple", model: "system" }, null);
  assert.deepEqual([pinned.name, pinned.model, pinned.where, pinned.host], ["Apple on-device", "system", "", ""]);
  const unread = chipView(null, null, { profile: "apple", model: "system" }, null);
  assert.deepEqual([unread.name, unread.model, unread.where, unread.kind], ["Apple on-device", "system", "", "text"]);
  // A pin that is gone shows its name as a button (there is a provider to choose) and says why in words.
  assert.equal(pinned.kind, "button");
  assert.ok(pinned.aria.includes("not available"));
  const lone = chipView(null, [local], { profile: "apple", model: "system" }, null);
  assert.equal(lone.kind, "button", "one other provider is still a choice to make");
  // With no saved provider the chip says so.
  assert.equal(chipView(null, [], { profile: "apple", model: "system" }, null).kind, "none");

  // Another id still falls back to its bare id, as before.
  const registered = chipView({ ...told, profile: "registered", model: "on-device" }, [local], { profile: "registered", model: "on-device" }, null);
  assert.equal(registered.text, "registered · on-device");
  assert.equal(chipView(null, [cloud], { profile: "local-1", model: "m" }, null).name, "local-1");
});

test("a blocked send says one fixed sentence", () => {
  assert.equal(DEVICE_OFF, "Apple on-device is not available. Choose another provider, or see the provider settings for the reason.");
  assert.ok(!DEVICE_OFF.includes("://"));
  assert.equal(DEVICE_ID, golden.id);
  assert.equal(DEVICE_MODEL, golden.model);
  assert.equal(DEVICE_NAME, "Apple on-device");
});
