// The reply action of the chat (docs/design.md section 6.19). A button under a one-line code block of a finished reply puts the
// line on the prompt of the pane the chat acts for through the terminal's paste path and never runs it. Every text goes in as a
// text node, the command text is never put into an attribute, and the verdict on the line is taken again at the click.
import { call } from "./bridge.js";
import { paneBlocks } from "./blockstate.js";
import { actingPane } from "./chat.js";
import type { ReplyHandle } from "./chat.js";
import { el } from "./dom.js";
import { MAX_BUTTONS, WORDS, commandVerdict, insertRefusal, insertedWords } from "./inlinestate.js";
import type { InsertTarget } from "./inlinestate.js";
import { focusInput } from "./keyboard.js";
import { applyView } from "./scene.js";
import { state } from "./state.js";
import type { View } from "./types.js";

/** Whether a node is one of the bars or notes this module puts after a code block. */
const isMark = (node: Element | null): boolean => node !== null && (node.classList.contains("chat-codebar") || node.classList.contains("chat-codenote"));

/**
 * Put the reply action under the code blocks of `node`, the rendered copy of a finished reply: a button under a block that is one
 * clean line, a warn line under a one-line block that holds characters that are not shown, nothing under any other. At most
 * `MAX_BUTTONS` bars and notes in all. A block that already has its bar or note gets none, so a second call adds nothing.
 */
export function decorateReply(node: HTMLElement, reply: ReplyHandle): void {
  let used = node.querySelectorAll(".chat-codebar, .chat-codenote").length;
  for (const pre of Array.from(node.querySelectorAll("pre"))) {
    if (used >= MAX_BUTTONS) return;
    if (isMark(pre.nextElementSibling)) continue;
    const verdict = commandVerdict(pre.textContent ?? "");
    if (verdict.kind === "none") continue;
    used += 1;
    if (verdict.kind === "hidden") {
      const note = el("p", "chat-codenote");
      note.textContent = WORDS.notInsertable;
      pre.after(note);
      continue;
    }
    const bar = el("div", "chat-codebar");
    const button = el("button", "chat-insert", bar);
    button.type = "button";
    button.dataset.field = "";
    button.textContent = WORDS.insertLabel;
    const text = verdict.text;
    // An event that a script made is not a press: only the person's own click or key reaches the terminal.
    button.addEventListener("click", (e) => {
      if (e.isTrusted) void insertCommand(text, reply);
    });
    pre.after(bar);
  }
}

/**
 * Put `command` at the prompt of the pane the chat acts for, through the paste command with the unsafe flag off, once, with no
 * line break and no key press. The line is checked again here; the pane is taken now and must be a terminal pane of the shown tab
 * that is not showing a full-screen program. Only the answer `written` counts: anything else is said in words and never repeated.
 */
export async function insertCommand(command: string, reply: ReplyHandle): Promise<void> {
  const verdict = commandVerdict(command);
  if (verdict.kind !== "command" || verdict.text !== command) {
    reply.say(WORDS.refused, "error");
    return;
  }
  const pane = actingPane();
  const target: InsertTarget = {
    pane,
    terminal: pane !== null && !paneBlocks.has(pane),
    inView: pane !== null && (state.view?.panes.some(([id]) => id === pane) ?? false),
    fullScreen: pane !== null && state.frames.get(pane)?.modes.alt === true,
  };
  const refusal = insertRefusal(target);
  if (refusal !== null || pane === null) {
    reply.say(refusal ?? WORDS.noTerminal, "error");
    return;
  }
  let outcome: unknown = null;
  try {
    outcome = await call<unknown>("paste", { pane, text: verdict.text, allow_unsafe: false });
  } catch {
    // a refused or failed paste is told below, in words
  }
  if (outcome !== "written") {
    reply.say(WORDS.refused, "error");
    return;
  }
  // The terminal pane gets the keyboard; on a narrow screen this also shows the pane that holds the line.
  try {
    applyView(await call<View>("focus_pane", { pane }));
  } catch {
    // the line is at the prompt; only the focus did not move
  }
  focusInput();
  reply.say(insertedWords(pane), "note");
}
