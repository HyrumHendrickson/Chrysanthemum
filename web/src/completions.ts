// LaTeX autocompletion: common commands, environments, and labels/citation keys.

import { type Completion, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";

const COMMANDS = [
  "documentclass", "usepackage", "begin", "end", "title", "author", "date", "maketitle", "tableofcontents",
  "section", "subsection", "subsubsection", "paragraph", "chapter", "part", "appendix",
  "label", "ref", "eqref", "autoref", "cref", "pageref", "cite", "citep", "citet", "footnote",
  "textbf", "textit", "texttt", "textsc", "textrm", "textsf", "emph", "underline", "mathbf", "mathit", "mathrm", "mathcal", "mathbb", "mathfrak",
  "frac", "sqrt", "sum", "prod", "int", "oint", "lim", "infty", "partial", "nabla", "cdot", "cdots", "ldots", "times", "pm", "leq", "geq", "neq", "approx", "equiv", "subset", "subseteq", "in", "notin", "cup", "cap", "forall", "exists", "to", "mapsto", "implies", "iff",
  "alpha", "beta", "gamma", "delta", "epsilon", "varepsilon", "zeta", "eta", "theta", "lambda", "mu", "nu", "xi", "pi", "rho", "sigma", "tau", "phi", "chi", "psi", "omega", "Gamma", "Delta", "Theta", "Lambda", "Sigma", "Phi", "Psi", "Omega",
  "left", "right", "big", "Big", "bigg", "overline", "hat", "tilde", "vec", "bar", "dot", "ddot",
  "includegraphics", "caption", "centering", "item", "hline", "multicolumn", "input", "include", "bibliography", "bibliographystyle", "printbibliography", "addbibresource",
  "newcommand", "renewcommand", "newenvironment", "newtheorem", "setlength", "vspace", "hspace", "noindent", "newpage", "clearpage", "pagebreak", "linebreak",
  "tiny", "scriptsize", "footnotesize", "small", "normalsize", "large", "Large", "LARGE", "huge", "Huge",
  "url", "href", "hyperref", "texorpdfstring", "today", "LaTeX", "TeX",
].map((name): Completion => ({ label: `\\${name}`, type: "keyword", apply: `\\${name}` }));

const ENVIRONMENTS = [
  "document", "abstract", "itemize", "enumerate", "description", "equation", "equation*", "align", "align*", "gather", "gather*", "multline", "split",
  "figure", "table", "tabular", "center", "flushleft", "flushright", "quote", "quotation", "verbatim", "minipage",
  "theorem", "lemma", "proposition", "corollary", "proof", "definition", "example", "remark",
  "matrix", "pmatrix", "bmatrix", "vmatrix", "cases", "tikzpicture", "lstlisting", "thebibliography", "frame",
];

function environmentCompletions(): Completion[] {
  return ENVIRONMENTS.map((name) => ({
    label: name,
    type: "class",
    apply: (view, _completion, from, to) => {
      // Complete the name and close the environment, leaving the cursor inside.
      const after = view.state.doc.sliceString(to, to + 1) === "}" ? 1 : 0;
      const line = view.state.doc.lineAt(from);
      const indent = /^\s*/.exec(line.text)?.[0] ?? "";
      const insert = `${name}}\n${indent}  \n${indent}\\end{${name}}`;
      view.dispatch({
        changes: { from, to: to + after, insert },
        selection: { anchor: from + name.length + 2 + indent.length + 2 },
        userEvent: "input.complete",
      });
    },
  }));
}

export interface ProjectSymbols {
  bibKeys: () => string[];
}

function scan(doc: string, pattern: RegExp): string[] {
  const found = new Set<string>();
  for (const m of doc.matchAll(pattern)) if (m[1]) found.add(m[1]);
  return [...found];
}

export function latexCompletions(symbols: ProjectSymbols) {
  const envs = environmentCompletions();
  return (context: CompletionContext): CompletionResult | null => {
    const line = context.state.doc.lineAt(context.pos);
    const before = line.text.slice(0, context.pos - line.from);

    const arg = /\\(begin|end|ref|eqref|autoref|cref|pageref|cite[a-z]*)\{([^}]*)$/.exec(before);
    if (arg) {
      const [, cmd = "", typed = ""] = arg;
      const from = context.pos - typed.length;
      if (cmd === "begin") return { from, options: envs, validFor: /^[\w*]*$/ };
      if (cmd === "end") {
        const open = scan(context.state.doc.sliceString(0, context.pos), /\\begin\{([^}]+)\}/g);
        return { from, options: open.reverse().map((label) => ({ label, type: "class" })), validFor: /^[\w*]*$/ };
      }
      if (cmd.startsWith("cite")) {
        const keys = [
          ...symbols.bibKeys(),
          ...scan(context.state.doc.toString(), /\\bibitem(?:\[[^\]]*\])?\{([^}]+)\}/g),
        ];
        return { from, options: [...new Set(keys)].map((label) => ({ label, type: "text" })), validFor: /^[^},]*$/ };
      }
      const labels = scan(context.state.doc.toString(), /\\label\{([^}]+)\}/g);
      return { from, options: labels.map((label) => ({ label, type: "variable" })), validFor: /^[^}]*$/ };
    }

    const word = context.matchBefore(/\\[a-zA-Z]*/);
    if (!word || (word.from === word.to && !context.explicit)) return null;
    return { from: word.from, options: COMMANDS, validFor: /^\\[a-zA-Z]*$/ };
  };
}
