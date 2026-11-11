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
  { tag: t.keyword, color: "#0b6b3a", fontWeight: "600" },
  { tag: [t.comment, t.lineComment], color: "#6a7a90", fontStyle: "italic" },
  { tag: [t.string, t.special(t.string)], color: "#1f6f8b" },
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
      ".cm-activeLine": { backgroundColor: dark ? "#ffffff0a" : "#0988420d" },
      ".cm-activeLineGutter": { backgroundColor: dark ? "#ffffff0f" : "#09884214", color: "var(--text)" },
      "&.cm-focused": { outline: "none" },
      ".cm-cursor": { borderLeftColor: "var(--accent)" },
      "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": {
        backgroundColor: dark ? "#4fcf8644" : "#09884233",
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


export type EditorCommand =
  | "bold" | "italic" | "underline" | "math" | "displaymath" | "link" | "comment"
  | "itemize" | "enumerate"
  | "normal" | "section" | "subsection" | "subsubsection" | "paragraph";

const HEADING = /^(\s*)\\(?:section|subsection|subsubsection|paragraph|subparagraph)\*?\{(.*)\}\s*$/;

function setHeading(view: EditorView, command: string | null) {
  const { state } = view;
  const line = state.doc.lineAt(state.selection.main.head);
  const m = HEADING.exec(line.text);
  const indent = m ? (m[1] ?? "") : (/^\s*/.exec(line.text)?.[0] ?? "");
  const inner = m ? (m[2] ?? "") : line.text.trim();
  const text = command ? `${indent}\\${command}{${inner}}` : `${indent}${inner}`;
  view.dispatch({
    changes: { from: line.from, to: line.to, insert: text },
    selection: EditorSelection.cursor(line.from + text.length),
  });
}

function makeList(view: EditorView, env: "itemize" | "enumerate") {
  const { state } = view;
  const range = state.selection.main;
  const first = state.doc.lineAt(range.from);
  const last = state.doc.lineAt(range.to);
  const items: string[] = [];
  for (let n = first.number; n <= last.number; n++) {
    const text = state.doc.line(n).text.trim();
    if (text) items.push(`  \\item ${text}`);
  }
  const body = items.length ? items.join("\n") : "  \\item ";
  const insert = `\\begin{${env}}\n${body}\n\\end{${env}}`;
  view.dispatch({
    changes: { from: first.from, to: last.to, insert },
    selection: EditorSelection.cursor(first.from + insert.length - `\n\\end{${env}}`.length),
  });
}

function toggleComment(view: EditorView) {
  const { state } = view;
  const range = state.selection.main;
  const first = state.doc.lineAt(range.from).number;
  const last = state.doc.lineAt(range.to).number;
  const lines = [];
  for (let n = first; n <= last; n++) lines.push(state.doc.line(n));
  const filled = lines.filter((l) => l.text.trim() !== "");
  const allCommented = filled.length > 0 && filled.every((l) => l.text.trimStart().startsWith("%"));
  const changes = filled.map((l) => {
    const indent = l.text.length - l.text.trimStart().length;
    if (allCommented) {
      const after = l.text.slice(indent + 1).startsWith(" ") ? 2 : 1;
      return { from: l.from + indent, to: l.from + indent + after, insert: "" };
    }
    return { from: l.from + indent, insert: "% " };
  });
  view.dispatch({ changes });
}

function insertLink(view: EditorView) {
  const { state } = view;
  const range = state.selection.main;
  const text = state.sliceDoc(range.from, range.to) || "text";
  const insert = `\\href{https://}{${text}}`;
  const urlStart = range.from + "\\href{".length;
  view.dispatch({
    changes: { from: range.from, to: range.to, insert },
    selection: EditorSelection.range(urlStart, urlStart + "https://".length),
  });
}

function runCommand(view: EditorView, command: EditorCommand) {
  switch (command) {
    case "bold": wrap("\\textbf{", "}")(view); break;
    case "italic": wrap("\\textit{", "}")(view); break;
    case "underline": wrap("\\underline{", "}")(view); break;
    case "math": wrap("$", "$")(view); break;
    case "displaymath": wrap("\\[ ", " \\]")(view); break;
    case "link": insertLink(view); break;
    case "comment": toggleComment(view); break;
    case "itemize": makeList(view, "itemize"); break;
    case "enumerate": makeList(view, "enumerate"); break;
    case "normal": setHeading(view, null); break;
    default: setHeading(view, command);
  }
  view.focus();
}

export interface EditorHandle {
  exec(command: EditorCommand): void;
  undo(): void;
  redo(): void;
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
          { key: "Mod-u", run: wrap("\\underline{", "}") },
          { key: "Mod-/", run: (v) => (toggleComment(v), true) },
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
    exec: (command) => runCommand(view, command),
    undo: () => {
      undoManager.undo();
      view.focus();
    },
    redo: () => {
      undoManager.redo();
      view.focus();
    },
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
