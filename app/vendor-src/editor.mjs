// The code editor of a preview pane: CodeMirror 6, bundled once by scripts/vendor-editor.mjs into dist/vendor/editor and
// loaded only when a text file is opened. The page has no bundler of its own; this is the one place one is used, for a
// library that is written as many small packages. The look comes from the page's tokens (`var(--hw-*)`), so a theme
// moves the editor with the rest of the chrome. Nothing here touches the network or the file system: the page gives it
// text and takes text back.
//
// CodeMirror puts its styles in a constructed style sheet (`adoptedStyleSheets`), which the page's policy
// (`style-src 'self'`) allows, and sets sizes through the CSSOM, never through a style attribute. The editor lives in a shadow root (see createEditor).
import { Compartment, EditorState } from "@codemirror/state";
import {
  EditorView, drawSelection, dropCursor, highlightActiveLine, highlightActiveLineGutter, highlightSpecialChars, keymap,
  lineNumbers, rectangularSelection,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import {
  HighlightStyle, StreamLanguage, bracketMatching, foldGutter, foldKeymap, indentOnInput, syntaxHighlighting,
} from "@codemirror/language";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { tags as t } from "@lezer/highlight";

/** The languages, by the names the core gives (`core/src/preview.rs`). Each is loaded when a file of it is opened. */
const LANGUAGES = {
  rust: () => import("@codemirror/lang-rust").then((m) => m.rust()),
  typescript: () => import("@codemirror/lang-javascript").then((m) => m.javascript({ typescript: true })),
  javascript: () => import("@codemirror/lang-javascript").then((m) => m.javascript()),
  json: () => import("@codemirror/lang-json").then((m) => m.json()),
  python: () => import("@codemirror/lang-python").then((m) => m.python()),
  markdown: () => import("@codemirror/lang-markdown").then((m) => m.markdown()),
  html: () => import("@codemirror/lang-html").then((m) => m.html()),
  css: () => import("@codemirror/lang-css").then((m) => m.css()),
  yaml: () => import("@codemirror/lang-yaml").then((m) => m.yaml()),
  go: () => import("@codemirror/lang-go").then((m) => m.go()),
  java: () => import("@codemirror/lang-java").then((m) => m.java()),
  c: () => import("@codemirror/lang-cpp").then((m) => m.cpp()),
  cpp: () => import("@codemirror/lang-cpp").then((m) => m.cpp()),
  sql: () => import("@codemirror/lang-sql").then((m) => m.sql()),
  xml: () => import("@codemirror/lang-xml").then((m) => m.xml()),
  shell: () => import("@codemirror/legacy-modes/mode/shell").then((m) => StreamLanguage.define(m.shell)),
  toml: () => import("@codemirror/legacy-modes/mode/toml").then((m) => StreamLanguage.define(m.toml)),
  swift: () => import("@codemirror/legacy-modes/mode/swift").then((m) => StreamLanguage.define(m.swift)),
  ruby: () => import("@codemirror/legacy-modes/mode/ruby").then((m) => StreamLanguage.define(m.ruby)),
  kotlin: () => import("@codemirror/legacy-modes/mode/clike").then((m) => StreamLanguage.define(m.kotlin)),
  zig: () => import("@codemirror/legacy-modes/mode/clike").then((m) => StreamLanguage.define(m.c)),
};

/** The colours of the syntax: the terminal palette's own tokens, so a code block reads like the terminal beside it. */
const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.controlKeyword, t.operatorKeyword, t.modifier], color: "var(--hw-rose)" },
  { tag: [t.string, t.special(t.string), t.regexp], color: "var(--hw-ok)" },
  { tag: [t.number, t.bool, t.null, t.atom], color: "var(--hw-warn)" },
  { tag: [t.comment, t.lineComment, t.blockComment], color: "var(--hw-ink-3)", fontStyle: "italic" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.labelName], color: "var(--hw-info)" },
  { tag: [t.typeName, t.className, t.namespace], color: "var(--hw-info)" },
  { tag: [t.propertyName, t.attributeName], color: "var(--hw-ink)" },
  { tag: [t.heading], color: "var(--hw-rose)", fontWeight: "700" },
  { tag: [t.link, t.url], color: "var(--hw-info)", textDecoration: "underline" },
  { tag: [t.emphasis], fontStyle: "italic" },
  { tag: [t.strong], fontWeight: "700" },
  { tag: [t.invalid], color: "var(--hw-danger)" },
]);

const theme = EditorView.theme(
  {
    "&": { height: "100%", color: "var(--hw-ink)", backgroundColor: "var(--hw-pane)", fontSize: "1rem" },
    ".cm-scroller": { fontFamily: "inherit", lineHeight: "1.5", overflow: "auto" },
    ".cm-content": { caretColor: "var(--hw-rose)", padding: ".5rem 0" },
    "&.cm-focused": { outline: "none" },
    ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--hw-rose)", borderLeftWidth: "2px" },
    "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection": {
      backgroundColor: "var(--hw-selection)",
    },
    ".cm-activeLine": { backgroundColor: "var(--hw-rose-wash)" },
    ".cm-gutters": { backgroundColor: "var(--hw-pane)", color: "var(--hw-ink-3)", border: "none", borderRight: "1px solid var(--hw-edge)" },
    ".cm-activeLineGutter": { backgroundColor: "var(--hw-rose-wash)", color: "var(--hw-ink)" },
    ".cm-foldPlaceholder": { backgroundColor: "var(--hw-ground-2)", border: "1px solid var(--hw-edge)", color: "var(--hw-ink-2)" },
    ".cm-matchingBracket": { outline: "1px solid var(--hw-rose)", backgroundColor: "transparent" },
    ".cm-searchMatch": { backgroundColor: "var(--hw-selection)", outline: "1px solid var(--hw-warn)" },
    ".cm-panels": { backgroundColor: "var(--hw-ground-2)", color: "var(--hw-ink)", borderTop: "1px solid var(--hw-edge)" },
    ".cm-panel input, .cm-panel button, .cm-panel label": { font: "inherit", color: "var(--hw-ink)" },
    ".cm-panel input": { backgroundColor: "var(--hw-ground)", border: "1px solid var(--hw-edge)", borderRadius: ".333rem" },
    ".cm-panel button": { backgroundColor: "transparent", border: "1px solid var(--hw-edge)", borderRadius: ".333rem" },
  },
  { dark: true },
);

/**
 * Make an editor in `parent`.
 *
 * `text` is the starting content and `language` a name from the core (or none). `onChange(text)` is called after every
 * edit that changes the content; `onSave()` when the person presses the save chord (Cmd+S, Ctrl+S); `onLeave()` on
 * Escape, which gives the keyboard back to the terminal. A read-only editor shows the text and edits nothing.
 */
export async function createEditor({ parent, text, language, readOnly = false, onChange, onSave, onLeave }) {
  const languageSlot = new Compartment();
  const state = EditorState.create({
    doc: text,
    extensions: [
      lineNumbers(),
      highlightActiveLineGutter(),
      highlightSpecialChars(),
      history(),
      foldGutter(),
      drawSelection(),
      dropCursor(),
      EditorState.allowMultipleSelections.of(true),
      indentOnInput(),
      syntaxHighlighting(highlight),
      bracketMatching(),
      rectangularSelection(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      EditorState.tabSize.of(4),
      EditorView.contentAttributes.of({ "aria-label": "Editor", spellcheck: "false", autocorrect: "off", autocapitalize: "off" }),
      EditorState.readOnly.of(readOnly),
      EditorView.editable.of(!readOnly),
      keymap.of([
        {
          key: "Mod-s",
          run: () => {
            onSave?.();
            return true;
          },
        },
        {
          key: "Escape",
          run: () => {
            onLeave?.();
            return true;
          },
        },
        indentWithTab,
        ...searchKeymap,
        ...foldKeymap,
        ...historyKeymap,
        ...defaultKeymap,
      ]),
      theme,
      languageSlot.of([]),
      EditorView.updateListener.of((update) => {
        if (update.docChanged) onChange?.(update.state.doc.toString());
      }),
    ],
  });
  // CodeMirror writes its style sheet into a <style> element of the document, which the page's policy
  // (`style-src 'self'`) blocks. In a shadow root it adopts a constructed style sheet instead, which the policy allows.
  // The host is marked as a text field of the page (`data-field`), so the key handling leaves its keys alone: events that
  // cross the shadow boundary have the host as their target.
  parent.dataset.field = "editor";
  const root = parent.shadowRoot ?? parent.attachShadow({ mode: "open" });
  const view = new EditorView({ state, parent: root, root });

  const load = LANGUAGES[language];
  if (load) {
    // The language arrives a moment later; the text is usable before it does.
    void load()
      .then((extension) => view.dispatch({ effects: languageSlot.reconfigure(extension) }))
      .catch(() => {});
  }

  return {
    /** The whole text. */
    getText: () => view.state.doc.toString(),
    /** Replace the whole text without calling `onChange` (a file that was read again). */
    setText(next) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: next }, annotations: [] });
    },
    focus: () => view.focus(),
    hasFocus: () => view.hasFocus,
    destroy: () => view.destroy(),
  };
}
