// Placeholder for the RED commit of plan 02-18 task 3: the real types and signatures, no behaviour yet.

/** What the core answered to a paste. */
export type PasteOutcome = "written" | "empty" | "rejected";

/** What the page does next: nothing, ask the user, or send the same text again without asking. */
export type PasteStep = "done" | "confirm" | "resend";

/** The part of a pasted text the confirmation shows. */
export interface Preview {
  lines: number;
  shown: string[];
  hidden: number;
  control: boolean;
}

export const PREVIEW_LINES = 5;
export const PREVIEW_COLUMNS = 80;

export function pasteDecision(_outcome: unknown, _text: string, _confirmSetting: boolean): PasteStep {
  return "done";
}

export function previewOf(_text: string): Preview {
  return { lines: 0, shown: [], hidden: 0, control: false };
}

export function isConfirmingPaste(): boolean {
  return false;
}

export function confirmPaste(_text: string): Promise<boolean> {
  return Promise.resolve(false);
}
