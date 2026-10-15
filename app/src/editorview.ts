// The code editor of a preview pane. CodeMirror 6 is bundled once into vendor/editor (scripts/vendor-editor.mjs) and
// loaded here only when a text file is opened, the same way PDF.js is loaded for a PDF.

export interface EditorOptions {
  parent: HTMLElement;
  text: string;
  language: string | null;
  readOnly?: boolean;
  onChange?: (text: string) => void;
  /** Cmd+S or Ctrl+S. */
  onSave?: () => void;
  /** Escape: the keyboard goes back to the terminal. */
  onLeave?: () => void;
}

export interface EditorHandle {
  getText(): string;
  setText(text: string): void;
  focus(): void;
  hasFocus(): boolean;
  destroy(): void;
}

interface EditorModule {
  createEditor(o: EditorOptions): Promise<EditorHandle>;
}

let module: Promise<EditorModule> | null = null;

/** Load the editor once. The path is a string at run time on purpose: the module is a file of the bundle. */
export function loadEditor(): Promise<EditorModule> {
  module ??= import(new URL("./vendor/editor/editor.js", location.href).href) as Promise<EditorModule>;
  module.catch(() => {
    module = null; // a failed load is tried again on the next file
  });
  return module;
}

/** Make an editor in `o.parent`. */
export async function showEditor(o: EditorOptions): Promise<EditorHandle> {
  return (await loadEditor()).createEditor(o);
}
