// The palette actions of the chat (docs/design.md section 6.19). They prepare a message and a card and hand them to the chat
// module, which applies them to the chat that came up: no command is called here and nothing is sent, shared or read. The
// person still reads the item, shares the pane and presses Send.
import { registerAction } from "./actions.js";
import { paneBlocks } from "./blockstate.js";
import { actingPane, openWithIntent } from "./chat.js";
import { EXPLAIN_ACTION, EXPLAIN_TEXT, EXPLAIN_TITLE, WORDS, explainPlan } from "./inlinestate.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";

/**
 * `Explain error`: open the chat where the person is, write the fixed message into its composer and open the context card for
 * the pane the chat acts for with one item chosen. Answers null when done, else one sentence. The pane is read before anything
 * opens, because opening a chat moves the focus.
 */
export async function explainError(): Promise<string | null> {
  const pane = actingPane();
  // A pane with a block is a preview, a file browser or a monitor: only a terminal pane has an error to explain.
  if (pane === null || paneBlocks.has(pane)) return WORDS.notTerminal;
  const plan = explainPlan(state.frames.get(pane));
  return openWithIntent({ text: EXPLAIN_TEXT, kinds: plan.kinds, first: plan.first, ready: WORDS.explainReady });
}

/** Run an action that answers a sentence and show the sentence as the notice of the status line. */
function say(run: () => Promise<string | null>): () => void {
  return () => {
    void run().then((sentence) => {
      if (sentence !== null) showNotice(sentence);
    });
  };
}

/** Register the palette actions. Call once at start-up, after `initChat`. */
export async function initInline(): Promise<void> {
  registerAction({ id: EXPLAIN_ACTION, title: EXPLAIN_TITLE, run: say(explainError) });
}
