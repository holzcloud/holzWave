// The pure half of the own window frame (docs/design.md section 6.33): how the facts of the shell are read, the table of the
// window buttons with their words, and how the answers of the window are read. No DOM, no `window`, no call into the shell:
// the reader of the window state gets the command function as an argument. The words are written here once; the unit test is
// the one place that compares them with the design record, the page never reads the record.

/** A button word of the shell, the contract between the shell and the page. */
export type ButtonWord = "minimize" | "maximize" | "close";

/** The drawing of a glyph: the maximise button shows `restore` while the window is maximised. */
export type GlyphWord = "minimize" | "maximize" | "restore" | "close";

/** What the shell wrote: the platform, the style, the side and the buttons in the order they are drawn. */
export interface FrameFacts {
  readonly os: string;
  readonly style: string;
  readonly side: string;
  readonly buttons: readonly ButtonWord[];
}

/** One window button: the shell's word, the name that is also the title, the glyph drawn and the window command a press sends. */
export interface ButtonRow {
  readonly word: ButtonWord;
  readonly name: string;
  readonly glyph: GlyphWord;
  readonly press: "minimize" | "toggle_maximize" | "close";
}

/** The three buttons in the order Windows draws them (design 6.33, window buttons). */
export const BUTTONS: readonly ButtonRow[] = [
  { word: "minimize", name: "Minimize", glyph: "minimize", press: "minimize" },
  { word: "maximize", name: "Maximize", glyph: "maximize", press: "toggle_maximize" },
  { word: "close", name: "Close", glyph: "close", press: "close" },
];

/** The row of the restore state: the maximise button of a maximised window (design 6.33, window buttons and states). */
const RESTORE: ButtonRow = { word: "maximize", name: "Restore", glyph: "restore", press: "toggle_maximize" };

/** The maximise row for a window that is maximised or not: the other two rows never change with the state. */
export function maximizeRow(maximized: boolean): ButtonRow {
  return maximized ? RESTORE : (BUTTONS.find((row) => row.word === "maximize") as ButtonRow);
}

/** The words of the table, for the reader. */
const WORDS: readonly string[] = BUTTONS.map((row) => row.word);

/** One accepted contract of the shell: the platform, the styles and sides it may name and the lists of buttons it may send. */
interface Contract {
  readonly os: string;
  readonly styles: readonly string[];
  readonly sides: readonly string[];
  readonly lists: readonly (readonly string[])[];
}

/** Every non-empty list of distinct words of the table, in every order: 3 + 6 + 6 = 15 lists. */
function orderedSubsets(words: readonly string[]): readonly (readonly string[])[] {
  const out: string[][] = [];
  const walk = (taken: string[]): void => {
    if (taken.length > 0) out.push(taken);
    for (const word of words) if (!taken.includes(word)) walk([...taken, word]);
  };
  walk([]);
  return out;
}

/**
 * The contracts the page draws, as data (design 6.33, side and set). Windows: flat, on the right, the three buttons in this one
 * order. Linux: flat or round, left or right, and any non-empty list of distinct buttons in any order (the shell reads the
 * layout of the desktop and never sends an empty list: a window that cannot be closed by pointer is not drawn by guesswork).
 */
const CONTRACTS: readonly Contract[] = [
  { os: "windows", styles: ["flat"], sides: ["right"], lists: [["minimize", "maximize", "close"]] },
  { os: "linux", styles: ["flat", "round"], sides: ["left", "right"], lists: orderedSubsets(WORDS) },
];

/** The keys of the facts, in the order the shell writes them. */
const KEYS: readonly string[] = ["os", "style", "side", "buttons"];

/** The value of an own data property, or `undefined` for a getter, an inherited member or a missing key (a getter is never run). */
function data(object: object, key: string): unknown {
  const descriptor = Object.getOwnPropertyDescriptor(object, key);
  return descriptor !== undefined && "value" in descriptor ? descriptor.value : undefined;
}

/** A plain object or array of exactly these own keys and no others (an own `__proto__` counts as a key). */
function exactKeys(object: object, keys: readonly string[]): boolean {
  const own = Object.keys(object);
  return own.length === keys.length && keys.every((key) => own.includes(key)) && Object.getOwnPropertySymbols(object).length === 0;
}

/** Whether `list` is a plain array of exactly the words `words`, in that order. */
function sameWords(list: unknown, words: readonly string[]): list is string[] {
  if (!Array.isArray(list) || Object.getPrototypeOf(list) !== Array.prototype || list.length !== words.length) return false;
  if (Object.keys(list).length !== words.length) return false;
  return words.every((word, i) => data(list, String(i)) === word);
}

/**
 * The facts of the shell, or `null` for anything else. The value must be a plain object (the prototype `Object.prototype`) with
 * exactly the four own data members, and they must be one row of the table of contracts word for word: the platform, a style
 * and a side the row names, and a list of at most three buttons that is one of the row's lists (the length is looked at before
 * an element is). Each member is read once. The result is a new object made of the table's own words; nothing of the input is
 * kept, so a hostile value can neither reach the page nor be changed afterwards.
 */
export function readFrame(raw: unknown): FrameFacts | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  if (Object.getPrototypeOf(raw) !== Object.prototype || !exactKeys(raw, KEYS)) return null;
  const os = data(raw, "os");
  const style = data(raw, "style");
  const side = data(raw, "side");
  const buttons = data(raw, "buttons");
  if (!Array.isArray(buttons) || buttons.length < 1 || buttons.length > WORDS.length) return null;
  const contract = CONTRACTS.find((row) => row.os === os);
  if (contract === undefined) return null;
  const chosenStyle = contract.styles.find((word) => word === style);
  const chosenSide = contract.sides.find((word) => word === side);
  const list = contract.lists.find((words) => sameWords(buttons, words));
  if (chosenStyle === undefined || chosenSide === undefined || list === undefined) return null;
  return {
    os: contract.os,
    style: chosenStyle,
    side: chosenSide,
    buttons: list.filter((word): word is ButtonWord => WORDS.includes(word)),
  };
}

/** What the page knows of the window: whether it is maximised and whether it is full screen. */
export interface WindowState {
  readonly maximized: boolean;
  readonly fullscreen: boolean;
}

/** The commands of the window plugin that are asked, one per member. */
const ASKED = { maximized: "plugin:window|is_maximized", fullscreen: "plugin:window|is_fullscreen" } as const;

/**
 * One member of the state from one answer: a boolean is taken as it is, anything else (a string, a number, `null`, a missing
 * answer) and a rejection keep the previous value, so a window that answers oddly never flips the drawing. Only the boolean
 * `true` makes a member true.
 */
async function member(read: (command: string) => Promise<unknown>, command: string, previous: boolean): Promise<boolean> {
  try {
    const answer = await read(command);
    return typeof answer === "boolean" ? answer : previous;
  } catch {
    return previous;
  }
}

/** Ask the window for both members. `previous` is what the page showed so far; the reader is injected (the page passes `call`). */
export async function windowState(read: (command: string) => Promise<unknown>, previous: WindowState): Promise<WindowState> {
  const [maximized, fullscreen] = await Promise.all([
    member(read, ASKED.maximized, previous.maximized),
    member(read, ASKED.fullscreen, previous.fullscreen),
  ]);
  return { maximized, fullscreen };
}
