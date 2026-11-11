// CodeMirror 6 editor bound to a collaborative Yjs text.

import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, indentWithTab } from "@codemirror/commands";
import {
  HighlightStyle,
  StreamLanguage,
  bracketMatching,
  foldGutter,
  foldKeymap,
  indentOnInput,
  syntaxHighlighting,
} from "@codemirror/language";
import { stex } from "@codemirror/legacy-modes/mode/stex";
import { type Diagnostic as CmDiagnostic, lintGutter, setDiagnostics } from "@codemirror/lint";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { Compartment, EditorSelection, EditorState, type Extension } from "@codemirror/state";
import { oneDarkHighlightStyle } from "@codemirror/theme-one-dark";
import {
  EditorView,
  drawSelection,
  dropCursor,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { yCollab, ySyncAnnotation, yUndoManagerKeymap } from "y-codemirror.next";
import * as Y from "yjs";
import type { Awareness } from "y-protocols/awareness";

import type { Diagnostic } from "./api";
import { type ProjectSymbols, latexCompletions } from "./completions";
import type { User } from "./user";

const themeCompartment = new Compartment();
const editableCompartment = new Compartment();

const lightHighlight = HighlightStyle.define([
  { tag: t.keyword, color: "#2f5da8", fontWeight: "600" },
  { tag: [t.comment, t.lineComment], color: "#6a7a90", fontStyle: "italic" },
  { tag: [t.string, t.special(t.string)], color: "#2b7a63" },
  { tag: [t.number, t.atom, t.bool], color: "#8a4fa3" },
  { tag: [t.tagName, t.typeName, t.className], color: "#a8741a" },
  { tag: [t.bracket, t.punctuation, t.operator], color: "#5b6a82" },
  { tag: [t.variableName, t.name], color: "#172033" },
  { tag: t.invalid, color: "#b3412f" },
]);

function themeFor(mode: "light" | "dark"): Extension {
  const dark = mode === "dark";
  const base = EditorView.theme(
    {
      "&": { height: "100%", backgroundColor: "var(--surface)", color: "var(--text)" },
      ".cm-scroller": { fontFamily: "var(--font-mono)", fontSize: "14px", lineHeight: "1.65" },
      ".cm-content": { padding: "12px 0", caretColor: "var(--accent)" },
      ".cm-gutters": {
        backgroundColor: "var(--surface)",
        color: "var(--muted)",
        borderRight: "1px solid var(--line)",
      },
      ".cm-activeLine": { backgroundColor: dark ? "#ffffff0a" : "#2f5da80d" },
      ".cm-activeLineGutter": { backgroundColor: dark ? "#ffffff0f" : "#2f5da814", color: "var(--text)" },
      "&.cm-focused": { outline: "none" },
      ".cm-cursor": { borderLeftColor: "var(--accent)" },
      "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": {
        backgroundColor: dark ? "#6c9be644" : "#2f5da833",
      },
      ".cm-tooltip": {
        backgroundColor: "var(--surface)",
        border: "1px solid var(--line)",
        borderRadius: "6px",
        boxShadow: "0 6px 20px #0003",
      },
      ".cm-tooltip-autocomplete ul li[aria-selected]": {
        backgroundColor: "var(--accent)",
        color: "var(--accent-ink)",
      },
      ".cm-ySelectionInfo": { fontFamily: "var(--font-ui)", fontSize: "11px", padding: "1px 5px", borderRadius: "3px 3px 3px 0" },
    },
    { dark },
  );
  return [base, syntaxHighlighting(dark ? oneDarkHighlightStyle : lightHighlight)];
}

const wrap = (before: string, after: string) => (view: EditorView) => {
  const { state } = view;
  view.dispatch(
    state.changeByRange((range) => {
      const text = state.sliceDoc(range.from, range.to);
      const start = range.from + before.length;
      return {
        changes: { from: range.from, to: range.to, insert: `${before}${text}${after}` },
        range: range.empty ? EditorSelection.cursor(start) : EditorSelection.range(start, start + text.length),
      };
    }),
  );
  return true;
};

export interface EditorHandle {
  view: EditorView;
  setTheme(mode: "light" | "dark"): void;
  /** Editing is disabled until the first sync with the server completes. */
  setEditable(editable: boolean): void;
  setDiagnostics(items: Diagnostic[], file: string): void;
  reveal(line: number): void;
  focus(): void;
  destroy(): void;
}

export function createEditor(opts: {
  parent: HTMLElement;
  ytext: Y.Text;
  awareness: Awareness;
  user: User;
  theme: "light" | "dark";
  symbols: ProjectSymbols;
  onLocalChange: () => void;
  onCompile: () => void;
}): EditorHandle {
  opts.awareness.setLocalStateField("user", opts.user);
  const undoManager = new Y.UndoManager(opts.ytext);

  const view = new EditorView({
    parent: opts.parent,
    state: EditorState.create({
      doc: opts.ytext.toString(),
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        foldGutter(),
        lintGutter(),
        drawSelection(),
        dropCursor(),
        EditorState.allowMultipleSelections.of(true),
        indentOnInput(),
        bracketMatching(),
        closeBrackets(),
        rectangularSelection(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        EditorView.lineWrapping,
        StreamLanguage.define(stex),
        autocompletion({ override: [latexCompletions(opts.symbols)], activateOnTyping: true }),
        yCollab(opts.ytext, opts.awareness, { undoManager }),
        keymap.of([
          { key: "Mod-Enter", run: () => (opts.onCompile(), true) },
          { key: "Mod-s", run: () => (opts.onCompile(), true) },
          { key: "Mod-b", run: wrap("\\textbf{", "}") },
          { key: "Mod-i", run: wrap("\\textit{", "}") },
          { key: "Mod-Shift-m", run: wrap("$", "$") },
          ...closeBracketsKeymap,
          ...completionKeymap,
          ...searchKeymap,
          ...foldKeymap,
          ...yUndoManagerKeymap,
          ...defaultKeymap,
          indentWithTab,
        ]),
        EditorView.updateListener.of((update) => {
          if (!update.docChanged) return;
          // Changes applied from the shared document carry y-codemirror's annotation;
          // only local typing should trigger auto-compile.
          const remote = update.transactions.every((tr) => tr.annotation(ySyncAnnotation) !== undefined);
          if (!remote) opts.onLocalChange();
        }),
        themeCompartment.of(themeFor(opts.theme)),
        editableCompartment.of(EditorView.editable.of(false)),
      ],
    }),
  });

  return {
    view,
    setTheme(mode) {
      view.dispatch({ effects: themeCompartment.reconfigure(themeFor(mode)) });
    },
    setEditable(editable) {
      view.dispatch({ effects: editableCompartment.reconfigure(EditorView.editable.of(editable)) });
    },
    setDiagnostics(items, file) {
      const doc = view.state.doc;
      const mapped: CmDiagnostic[] = [];
      for (const d of items) {
        if (d.line === null || d.file !== file || d.line < 1 || d.line > doc.lines) continue;
        const line = doc.line(d.line);
        mapped.push({
          from: line.from,
          to: Math.max(line.from, line.to),
          severity: d.severity === "error" ? "error" : "warning",
          message: d.message,
        });
      }
      view.dispatch(setDiagnostics(view.state, mapped));
    },
    reveal(lineNumber) {
      const line = view.state.doc.line(Math.min(Math.max(1, lineNumber), view.state.doc.lines));
      view.dispatch({
        selection: { anchor: line.from },
        effects: EditorView.scrollIntoView(line.from, { y: "center" }),
      });
      view.focus();
    },
    focus: () => view.focus(),
    destroy: () => {
      undoManager.destroy();
      view.destroy();
    },
  };
}
