// The dialog of a keyboard-interactive login (docs/design.md 6.22): the server of a saved connection asks a question
// (a one-time code, a password prompt of PAM) and the person answers it here.
//
// What the server wrote is untrusted. It is set with `textContent` only, under the fixed sentence that says where it
// comes from; the title and the host line are the person's own saved connection (the core looks them up by id). An
// answer is read from its field once, sent, and the fields are emptied and removed: nothing keeps it, and no field is
// ever filled in. While it is open the dialog consumes the keys (the key handler stack). A question that arrives while one
// is open waits for its turn.
import { call, listen } from "./bridge.js";
import { el, loadStyles } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import { PromptQueue, readEnd, readPrompt, serverTextLine } from "./loginpromptstate.js";
import type { LoginPrompt } from "./loginpromptstate.js";

const queue = new PromptQueue();
/** Takes the dialog that is on screen away, without a call. */
let closeShown: (() => void) | null = null;

/** Register the two listeners. Call once at start-up. */
export function initLoginPrompt(): void {
  void listen<unknown>("login-prompt", (payload) => {
    const prompt = readPrompt(payload);
    if (prompt === null) return;
    const now = queue.push(prompt);
    if (now !== null) void show(now);
  }).catch(() => {});
  void listen<unknown>("login-prompt-end", (payload) => {
    const id = readEnd(payload);
    if (id === null) return;
    // Answered in another window, or cancelled or timed out in the core: the dialog goes without a call.
    const ended = queue.end(id);
    if (ended.closed) closeShown?.();
    if (ended.next !== null) void show(ended.next);
  }).catch(() => {});
}

async function show(prompt: LoginPrompt): Promise<void> {
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./settings.css");
    await loadStyles("./loginprompt.css");
  } catch {
    // the dialog still works without its style sheets
  }
  const { focusInput } = await import("./keyboard.js");
  // A question that ended while the sheets loaded is not shown.
  if (queue.current?.id !== prompt.id) return;

  const backdrop = el("div", "dlg-backdrop", document.body);
  const box = el("div", "dlg loginprompt", backdrop);
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-labelledby", "lp-title");

  // The person's own connection: the title and the host line.
  const title = el("h2", "set-title", box);
  title.id = "lp-title";
  title.textContent = `Login prompt from ${prompt.connection}`;
  el("p", "lp-host", box).textContent = `Host: ${prompt.host}`;
  el("p", "lp-source", box).textContent = serverTextLine;

  // What the server wrote, as text, in a block of its own.
  const block = el("fieldset", "lp-block", box);
  el("legend", "lp-legend", block).textContent = "The server says";
  if (prompt.name !== "") el("p", "lp-name", block).textContent = prompt.name;
  if (prompt.instructions !== "") el("p", "lp-instructions", block).textContent = prompt.instructions;
  const inputs: HTMLInputElement[] = [];
  prompt.prompts.forEach((line, i) => {
    const row = el("div", "lp-row", block);
    const label = el("label", "lp-label", row);
    label.htmlFor = `lp-field-${i}`;
    label.textContent = line.text;
    const input = el("input", "", row);
    input.id = `lp-field-${i}`;
    // A masked field for what the server says is not echoed, a plain one for what it says is. Never remembered.
    input.type = line.echo ? "text" : "password";
    input.autocomplete = "off";
    input.spellcheck = false;
    input.setAttribute("autocapitalize", "off");
    input.setAttribute("autocorrect", "off");
    el("span", "lp-tag", row).textContent = line.echo ? "shown" : "hidden";
    inputs.push(input);
  });
  el("p", "lp-note", box).textContent = "Your answer goes to this login only. Nothing is stored.";

  const actions = el("div", "dlg-actions", box);
  const cancel = el("button", "", actions);
  cancel.type = "button";
  cancel.textContent = "Cancel";
  const send = el("button", "dlg-ok", actions);
  send.type = "button";
  send.textContent = "Send";

  let pop: () => void = () => {};
  /** Take the dialog away: empty and remove the fields first, so no answer stays in the page. */
  const close = (): void => {
    for (const input of inputs) input.value = "";
    inputs.length = 0;
    pop();
    backdrop.remove();
    closeShown = null;
    focusInput();
  };
  closeShown = close;

  /** This question is over here; the one that waited, if any, is shown. */
  const leave = (): void => {
    const follow = queue.end(prompt.id).next;
    if (follow !== null) void show(follow);
  };
  const doSend = (): void => {
    // Read once, now; the fields are emptied before the call goes out.
    const answers = inputs.map((input) => input.value);
    close();
    // An answer the core refuses (the question is over) needs nothing more: the dialog is gone either way.
    void call<boolean>("login_prompt_answer", { id: prompt.id, answers }).catch(() => false);
    leave();
  };
  const doCancel = (): void => {
    close();
    void call<boolean>("login_prompt_cancel", { id: prompt.id }).catch(() => false);
    leave();
  };

  send.addEventListener("click", doSend);
  cancel.addEventListener("click", doCancel);
  // A press on the backdrop does nothing: a stray click must not throw an answer away.

  const stops = (): HTMLElement[] => Array.from(box.querySelectorAll<HTMLElement>("input, button"));
  pop = pushKeyHandler((e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      doCancel();
    } else if (e.key === "Tab") {
      e.preventDefault();
      const items = stops();
      const at = items.indexOf(document.activeElement as HTMLElement);
      const to = e.shiftKey ? (at <= 0 ? items.length - 1 : at - 1) : at < 0 || at === items.length - 1 ? 0 : at + 1;
      items[to]?.focus();
    } else if (e.key === "Enter" && e.target instanceof HTMLInputElement) {
      e.preventDefault();
      const at = inputs.indexOf(e.target);
      if (at >= 0 && at < inputs.length - 1) inputs[at + 1]!.focus();
      else doSend();
    }
    return true;
  });
  inputs[0]?.focus();
}
