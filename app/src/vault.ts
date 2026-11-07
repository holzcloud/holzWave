// The vault on the page (docs/design.md section 6.6, rules Lock chip to Vault lost): on Android the secrets sit in a vault that
// the system unlocks with the screen lock, and this module shows its state and lets the person act on it. Everything here exists
// on Android only: `initVault` does nothing on any other platform, so a desktop and a browser get no element, no request and no
// stylesheet, and the module is not even fetched (phone.ts loads it on demand).
//
// The page never asks for a secret. The unlock sheet only starts the system's own prompt through `vault_unlock`; it has no input
// element, so nothing here can be taken for the prompt. Every text is a constant of vaultstate.ts and goes in with `textContent`;
// no text of the host or of a name reaches the document from here.
//
// What this module draws: the chip (phone: the place of the top strip, tablet: a segment of the status line), the unlock sheet, the
// quiet line of a connection that could not start, the notice for a device with no screen lock, the dialog of a lost vault, the
// group `Security` of the Settings dialog and the dialog that turns protection off. The five-minute rule itself runs in the host;
// the page only tells it when the window is hidden and shown (`vault_hidden`).
import { call, setLockedHook } from "./bridge.js";
import type { LockedCall } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { state } from "./state.js";
import { chipSlot } from "./status.js";
import {
  LOCKED_EVENT,
  STRINGS,
  actionOf,
  chipModel,
  createUnlockQueue,
  failureWord,
  freshNotice,
  isLockedFailure,
  lockedAnswer,
  noticeStep,
  parseStatus,
} from "./vaultstate.js";
import type { Chip, Glyph, VaultStatus } from "./vaultstate.js";
import { openLayer } from "./weblayer.js";
import type { Layer } from "./weblayer.js";

/** The last status the host answered; null until the first answer and when the answer was not a status. */
let status: VaultStatus | null = null;
let started = false;
let memory = freshNotice();
/** The person tapped the `Not protected` chip: the notice shows again. */
let asked = false;
const watchers = new Set<(status: VaultStatus | null) => void>();

/** The actions that wait for an unlock. The sheet opens for the first one. */
const queue = createUnlockQueue(() => void openSheet());

// ---- the status -------------------------------------------------------------------------------------

/** Take a `vault_status` answer: remember it and draw everything that shows it. An answer that is no status is no vault. */
function accept(raw: unknown): VaultStatus | null {
  status = parseStatus(raw);
  drawChip();
  drawNotice();
  for (const watcher of watchers) watcher(status);
  return status;
}

/** Ask the host for the status (it never prompts) and draw it. */
async function refresh(): Promise<VaultStatus | null> {
  try {
    return accept(await call<unknown>("vault_status"));
  } catch {
    return accept(null);
  }
}

// ---- the chip ---------------------------------------------------------------------------------------

/**
 * The SVG namespace is an XML name that `createElementNS` needs; nothing ever fetches it. It is written in parts so the scan for
 * network addresses in first-party sources, which cannot tell a name from an address, has nothing to flag.
 */
const SVG = ["http:", "", "www.w3.org", "2000", "svg"].join("/");

/** The padlock of a state, 12 by 14 units, drawn with the text colour: the closed one, the open one, the one with a bar. */
function padlock(glyph: Glyph): SVGSVGElement {
  const svg = document.createElementNS(SVG, "svg") as SVGSVGElement;
  svg.setAttribute("class", "vault-glyph");
  svg.setAttribute("data-glyph", glyph);
  svg.setAttribute("viewBox", "0 0 12 14");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.4");
  const body = document.createElementNS(SVG, "rect");
  for (const [name, value] of [["x", "1"], ["y", "6"], ["width", "10"], ["height", "7"], ["rx", "1.2"]] as const) body.setAttribute(name, value);
  const shackle = document.createElementNS(SVG, "path");
  shackle.setAttribute("d", glyph === "lock-open" ? "M3 6V4a3 3 0 0 1 6 0" : "M3 6V4a3 3 0 0 1 6 0v2");
  svg.append(body, shackle);
  if (glyph === "lock-bar") {
    const bar = document.createElementNS(SVG, "line");
    for (const [name, value] of [["x1", "0.5"], ["y1", "13.5"], ["x2", "11.5"], ["y2", "0.5"]] as const) bar.setAttribute(name, value);
    svg.append(bar);
  }
  return svg;
}

let chip: HTMLButtonElement | null = null;
let chipSvg: SVGSVGElement | null = null;
let chipWord: HTMLElement | null = null;

function makeChip(): HTMLButtonElement {
  const node = el("button", "vault-chip label");
  node.type = "button";
  node.id = "vault-chip";
  chipWord = el("span", "vault-word", node);
  node.addEventListener("click", () => void onChip());
  return node;
}

/** The place of the chip for the layout class the page has now: the strip of the phone, the status line of the tablet, none otherwise. */
function slot(): HTMLElement | null {
  const root = document.documentElement;
  if (root.classList.contains("layout-phone")) return document.getElementById("phone-chip");
  if (root.classList.contains("layout-tablet")) return chipSlot();
  return null;
}

function setChip(node: HTMLButtonElement, model: Chip): void {
  node.dataset.state = model.kind;
  node.dataset.action = model.action;
  node.setAttribute("aria-label", model.name);
  if (chipWord) setText(chipWord, model.label);
  if (chipSvg?.getAttribute("data-glyph") !== model.glyph) {
    const next = padlock(model.glyph);
    if (chipSvg) chipSvg.replaceWith(next);
    else node.prepend(next);
    chipSvg = next;
  }
}

/** Show the chip for the status, or take it away when there is nothing to say. The empty place stays, so nothing moves. */
function drawChip(): void {
  const model = chipModel(status);
  const place = slot();
  if (model === null || place === null) {
    chip?.remove();
    return;
  }
  chip ??= makeChip();
  setChip(chip, model);
  if (chip.parentElement !== place) place.append(chip);
}

/** A tap on the chip: `Locked` opens the unlock sheet, `Unlocked` locks at once, `Not protected` shows the notice. */
async function onChip(): Promise<void> {
  const action = chipModel(status)?.action;
  if (action === "unlock") {
    void openSheet();
  } else if (action === "lock") {
    try {
      accept(await call<unknown>("vault_lock"));
    } catch {
      await refresh();
    }
  } else if (action === "notice") {
    asked = true;
    drawNotice();
  }
}

// ---- the unlock sheet --------------------------------------------------------------------------------

let sheet: Layer | null = null;
let opening = false;

/** Take the sheet away and say nothing to the queue: the caller decides whether the actions run or end. */
function closeSheet(): void {
  sheet?.close();
  sheet = null;
}

/** Leave the sheet without an unlock: the actions that waited end with their own failure. */
function leaveSheet(): void {
  closeSheet();
  queue.cancelled();
}

/** The unlock sheet: one button that starts the system's prompt, one that leaves. It has no input element. */
async function openSheet(): Promise<void> {
  if (sheet !== null || opening) return;
  opening = true;
  const layer = await openLayer({
    cls: "vault-box vault-sheet",
    labelledBy: "vault-sheet-title",
    sheets: ["./vault.css"],
    onEscape: leaveSheet,
  });
  opening = false;
  layer.backdrop.classList.add("vault-backdrop");
  sheet = layer;
  const box = layer.box;
  const title = el("h2", "dlg-title", box);
  title.id = "vault-sheet-title";
  title.textContent = STRINGS.sheetTitle;
  el("p", "vault-line", box).textContent = STRINGS.sheetLine;
  const actions = el("div", "dlg-actions vault-actions", box);
  const unlock = el("button", "vault-solid", actions);
  unlock.type = "button";
  unlock.textContent = STRINGS.unlock;
  const notNow = el("button", "vault-text", actions);
  notNow.type = "button";
  notNow.textContent = STRINGS.notNow;
  const quiet = el("p", "vault-quiet", box);
  quiet.setAttribute("role", "status");

  let busy = false;
  unlock.addEventListener("click", () => {
    if (busy) return;
    busy = true;
    unlock.disabled = true;
    setText(quiet, "");
    void (async () => {
      try {
        accept(await call<unknown>("vault_unlock"));
      } catch (failure) {
        busy = false;
        if (sheet !== layer) return;
        if (failureWord(failure) === "lost") {
          leaveSheet();
          await refresh();
          void openLost();
          return;
        }
        // Cancelled, or any other failure: the sheet stays and says so quietly. The host's text is never shown.
        unlock.disabled = false;
        setText(quiet, STRINGS.notUnlocked);
        unlock.focus();
        return;
      }
      busy = false;
      if (sheet === layer) closeSheet();
      clearAnswer();
      queue.unlocked();
    })();
  });
  notNow.addEventListener("click", leaveSheet);
  layer.backdrop.addEventListener("pointerdown", (e) => {
    if (e.target === layer.backdrop) leaveSheet();
  });
  unlock.focus();
}

// ---- the locked answer of a connection --------------------------------------------------------------

/** How long the quiet line stays when nobody dismisses it. */
const ANSWER_MS = 10_000;
let answer: HTMLElement | null = null;
let answerTimer: ReturnType<typeof setTimeout> | undefined;

/** The place of what the page shows over the stage: the answer line and the notice. */
function top(): HTMLElement | null {
  const stage = document.getElementById("stage");
  if (!stage) return null;
  let node = document.getElementById("vault-top");
  if (!node) {
    node = el("div", "", stage);
    node.id = "vault-top";
  }
  return node;
}

/** Take the quiet line away. */
function clearAnswer(): void {
  clearTimeout(answerTimer);
  answer?.remove();
  answer = null;
}

/** Say `text` as the first line of the stage, where the pane that was to open would start: dim, quiet, no error colour. */
function showAnswer(text: string): void {
  const place = top();
  if (!place) return;
  if (!answer) {
    answer = el("p", "vault-answer", place);
    answer.id = "vault-answer";
    answer.setAttribute("role", "status");
  }
  setText(answer, text);
  place.prepend(answer);
  clearTimeout(answerTimer);
  answerTimer = setTimeout(clearAnswer, ANSWER_MS);
}

/** A command failed because the secrets are locked: say so where it was to run, open the sheet and repeat the command after an unlock. */
function onLocked(locked: LockedCall): Promise<unknown> | null {
  if (!isLockedFailure(locked.error)) return null;
  const action = actionOf(locked.cmd);
  if (action === "connect") showAnswer(lockedAnswer("connect"));
  else if (action === "ai") document.dispatchEvent(new CustomEvent(LOCKED_EVENT, { detail: { action, args: locked.args } }));
  let key = locked.cmd;
  try {
    key = `${locked.cmd}:${JSON.stringify(locked.args ?? null)}`;
  } catch {
    key = `${locked.cmd}:${Math.random()}`;
  }
  return queue.request(key, locked.retry, locked.error);
}

// ---- the notice for a device with no screen lock ------------------------------------------------------

let notice: HTMLElement | null = null;
let noticeActions: HTMLElement | null = null;
let offer: HTMLButtonElement | null = null;

/** Show, keep or take away the notice by the rule of `noticeStep`. */
function drawNotice(): void {
  const step = noticeStep(memory, status, asked);
  memory = step.memory;
  if (!step.visible) {
    notice?.remove();
    notice = null;
    noticeActions = null;
    offer = null;
    return;
  }
  const place = top();
  if (!place) return;
  if (!notice) {
    notice = el("section", "vault-notice", place);
    notice.id = "vault-notice";
    notice.setAttribute("role", "note");
    el("p", "vault-line", notice).textContent = STRINGS.notice;
    noticeActions = el("div", "vault-actions", notice);
    const dismiss = el("button", "vault-text", noticeActions);
    dismiss.type = "button";
    dismiss.textContent = STRINGS.dismiss;
    dismiss.addEventListener("click", () => {
      memory = { ...memory, dismissed: true };
      asked = false;
      drawNotice();
      focusInput();
    });
  }
  // The offer appears when the system can switch now: a screen lock was set while the notice was up.
  if (step.offer && !offer && noticeActions) {
    offer = el("button", "vault-solid");
    offer.type = "button";
    offer.textContent = STRINGS.useProtected;
    offer.addEventListener("click", () => void protect(true));
    noticeActions.prepend(offer);
  } else if (!step.offer && offer) {
    offer.remove();
    offer = null;
  }
}

/** Switch the protection and take the status the host answers. A refusal leaves everything as it was. */
async function protect(on: boolean): Promise<boolean> {
  try {
    accept(await call<unknown>("vault_protect", { on }));
    return true;
  } catch {
    await refresh();
    return false;
  }
}

// ---- the dialogs --------------------------------------------------------------------------------------

let lostOpen = false;

/** The vault whose key is gone: `Start fresh` throws the old one away, `Not now` leaves it. Layouts, history and settings are not touched. */
async function openLost(): Promise<void> {
  if (lostOpen) return;
  lostOpen = true;
  let layer: Layer | null = null;
  const leave = (): void => {
    layer?.close();
    lostOpen = false;
  };
  layer = await openLayer({ cls: "vault-box vault-lost", labelledBy: "vault-lost-title", sheets: ["./vault.css"], onEscape: leave });
  const box = layer.box;
  const title = el("h2", "dlg-title", box);
  title.id = "vault-lost-title";
  title.textContent = STRINGS.lostTitle;
  el("p", "vault-line", box).textContent = STRINGS.lostLine;
  el("p", "vault-quiet", box).textContent = STRINGS.lostNote;
  const actions = el("div", "dlg-actions vault-actions", box);
  const fresh = el("button", "vault-solid", actions);
  fresh.type = "button";
  fresh.textContent = STRINGS.startFresh;
  const notNow = el("button", "vault-text", actions);
  notNow.type = "button";
  notNow.textContent = STRINGS.notNow;
  fresh.addEventListener("click", () => {
    fresh.disabled = true;
    void (async () => {
      try {
        accept(await call<unknown>("vault_reset"));
        leave();
      } catch {
        fresh.disabled = false;
        fresh.focus();
      }
    })();
  });
  notNow.addEventListener("click", leave);
  fresh.focus();
}

/** Ask whether to turn protection off. True only for `Turn off`; `Keep on`, Escape and a tap outside are false. */
async function confirmOff(): Promise<boolean> {
  let layer: Layer | null = null;
  return new Promise<boolean>((resolve) => {
    const finish = (yes: boolean): void => {
      layer?.close();
      resolve(yes);
    };
    void openLayer({ cls: "vault-box vault-off", labelledBy: "vault-off-line", sheets: ["./vault.css"], onEscape: () => finish(false) }).then((made) => {
      layer = made;
      const box = made.box;
      const line = el("p", "vault-line", box);
      line.id = "vault-off-line";
      line.textContent = STRINGS.offLine;
      el("p", "vault-quiet", box).textContent = STRINGS.offScope;
      const actions = el("div", "dlg-actions vault-actions", box);
      const off = el("button", "vault-solid", actions);
      off.type = "button";
      off.textContent = STRINGS.turnOff;
      const keep = el("button", "vault-text", actions);
      keep.type = "button";
      keep.textContent = STRINGS.keepOn;
      off.addEventListener("click", () => finish(true));
      keep.addEventListener("click", () => finish(false));
      made.backdrop.addEventListener("pointerdown", (e) => {
        if (e.target === made.backdrop) finish(false);
      });
      // The safe answer has the keyboard first: a held Enter must not turn protection off.
      keep.focus();
    });
  });
}

// ---- the Security group of the Settings dialog ---------------------------------------------------------

/**
 * The group `Security`, built into the Settings dialog before `before` when the host has a vault: one row, a switch that shows
 * whether the vault asks for the screen lock. The value is a fact of this device that the host keeps; it is never a setting of the
 * settings file and is never synced. Returns the function that removes the group (and stops it from ever appearing).
 */
export function mountSecurityGroup(parent: Element, before: Element | null): () => void {
  let gone = false;
  let remove = (): void => {};
  void (async () => {
    try {
      await loadStyles("./vault.css");
    } catch {
      // the group still works without its style sheet
    }
    const now = await refresh();
    if (gone || !parent.isConnected || now === null || now.state === "none") return;
    remove = buildGroup(parent, before);
  })();
  return () => {
    gone = true;
    remove();
  };
}

function buildGroup(parent: Element, before: Element | null): () => void {
  const set = el("fieldset", "set-group vault-group");
  parent.insertBefore(set, before !== null && before.parentNode === parent ? before : null);
  el("legend", "", set).textContent = STRINGS.group;
  const row = el("div", "vault-row", set);
  const label = el("span", "vault-label", row);
  label.id = "vault-row-label";
  label.textContent = STRINGS.rowLabel;
  const toggle = el("button", "vault-switch", row);
  toggle.type = "button";
  toggle.id = "vault-switch";
  toggle.setAttribute("role", "switch");
  toggle.setAttribute("aria-labelledby", "vault-row-label");
  toggle.setAttribute("aria-describedby", "vault-row-help");
  const word = el("span", "vault-switch-word", toggle);
  el("i", "", toggle);
  const help = el("p", "vault-help", set);
  help.id = "vault-row-help";
  help.textContent = STRINGS.rowHelp;

  const draw = (): void => {
    const on = status?.protection === "auth";
    toggle.setAttribute("aria-checked", String(on));
    toggle.dataset.on = String(on);
    setText(word, on ? STRINGS.switchOn : STRINGS.switchOff);
  };
  let busy = false;
  toggle.addEventListener("click", () => {
    if (busy) return;
    busy = true;
    void (async () => {
      try {
        if (status?.protection === "auth") {
          if (await confirmOff()) await protect(false);
        } else {
          await protect(true);
        }
      } finally {
        busy = false;
        draw();
        toggle.focus();
      }
    })();
  });
  watchers.add(draw);
  draw();
  return () => {
    watchers.delete(draw);
    set.remove();
  };
}

// ---- start --------------------------------------------------------------------------------------------

/** Tell the host that the window is hidden or shown, which runs the five-minute rule of the vault; a shown window reads the status again. */
function onVisibility(): void {
  const hidden = document.hidden;
  call("vault_hidden", { hidden }).catch(() => {});
  if (!hidden) void refresh();
}

/**
 * Start the vault's page. Only on Android: anywhere else this does nothing and loads nothing. It loads the stylesheet, hooks the
 * bridge so that a command that fails locked opens the unlock sheet and runs once more after an unlock, reports the window's
 * visibility to the host, and reads the status once. It opens nothing by itself.
 */
export async function initVault(): Promise<void> {
  if (state.os !== "android" || started) return;
  started = true;
  try {
    await loadStyles("./vault.css");
  } catch {
    // the page works without the look: the chip is still a button with a word
  }
  setLockedHook(onLocked);
  document.addEventListener("hw:layout", drawChip);
  document.addEventListener("visibilitychange", onVisibility);
  call("vault_hidden", { hidden: document.hidden }).catch(() => {});
  await refresh();
}
