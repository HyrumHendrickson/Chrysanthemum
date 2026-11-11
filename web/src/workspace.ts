// The editing workspace: file tree, collaborative editor, PDF preview and logs.

import {
  type CompileResult,
  type Diagnostic,
  type Engine,
  type Entry,
  type Health,
  type Settings,
  api,
} from "./api";
import { DocProvider, type ConnectionStatus } from "./provider";
import { clear, confirmAction, debounce, h, promptText, toast } from "./dom";
import { type EditorHandle, createEditor } from "./editor";
import { icons } from "./icons";
import { PdfViewer } from "./pdf";
import { openShareDialog } from "./share";
import { theme } from "./theme";
import { type User, initials, loadUser, saveUser } from "./user";

const TEXT_EXT = new Set([
  "tex", "bib", "sty", "cls", "bst", "txt", "md", "cfg", "def", "ltx", "tikz", "csv", "json", "yaml", "yml",
  "bbx", "cbx", "lbx", "dtx", "ins", "rst", "toml", "ist", "latexmkrc", "gitignore",
]);
const IMAGE_EXT = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg"]);

const extOf = (path: string) => (path.includes(".") ? (path.split(".").pop() ?? "").toLowerCase() : "");
const baseName = (path: string) => path.split("/").pop() ?? path;
const dirOf = (path: string) => (path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "");

interface TreeNode {
  name: string;
  path: string;
  kind: "file" | "dir";
  children: TreeNode[];
}

function buildTree(entries: Entry[]): TreeNode[] {
  const root: TreeNode = { name: "", path: "", kind: "dir", children: [] };
  const dirs = new Map<string, TreeNode>([["", root]]);
  for (const e of entries) {
    const parent = dirs.get(dirOf(e.path)) ?? root;
    const node: TreeNode = { name: baseName(e.path), path: e.path, kind: e.kind, children: [] };
    parent.children.push(node);
    if (e.kind === "dir") dirs.set(e.path, node);
  }
  return root.children;
}

type LogTab = "problems" | "log";

export class Workspace {
  private files: Entry[] = [];
  private settings: Settings = { main: "main.tex", engine: "pdflatex" };
  private current: string | null = null;
  private provider: DocProvider | null = null;
  private editor: EditorHandle | null = null;
  private pdf: PdfViewer;
  private events: WebSocket | null = null;
  private eventsTimer: number | undefined;
  private eventsAttempts = 0;
  private destroyed = false;

  private user: User = loadUser();
  private collapsed = new Set<string>();
  private selectedDir = "";
  private bibKeys: string[] = [];

  private autoCompile = localStorage.getItem("scriptorium.auto") !== "off";
  private compiling = false;
  private pending = false;
  private lastApplied = 0;
  private result: CompileResult | null = null;
  private logTab: LogTab = "problems";
  private logOpen = false;
  private pendingLine: number | null = null;

  // elements
  private el!: Record<string, HTMLElement>;
  private engineSelect!: HTMLSelectElement;
  private autoToggle!: HTMLInputElement;
  private unsubscribeTheme: () => void;
  private readonly scheduleAuto = debounce(() => {
    if (this.autoCompile) void this.compile();
  }, 1800);

  constructor(
    private readonly root: HTMLElement,
    private readonly id: string,
    private readonly health: Health | null,
  ) {
    this.build();
    this.pdf = new PdfViewer(this.el.pdfScroll as HTMLElement, {
      onPage: (cur, total) => {
        this.el.pageInfo!.textContent = total ? `Page ${cur} of ${total}` : "";
      },
      onZoom: (label) => {
        this.el.zoomLabel!.textContent = label;
      },
    });
    this.unsubscribeTheme = theme.subscribe((m) => this.editor?.setTheme(m));
    document.addEventListener("keydown", this.onKeydown);
    void this.init();
  }

  destroy() {
    this.destroyed = true;
    this.scheduleAuto.cancel();
    document.removeEventListener("keydown", this.onKeydown);
    this.unsubscribeTheme();
    window.clearTimeout(this.eventsTimer);
    this.events?.close();
    this.closeEditor();
    this.pdf.destroy();
  }

  // ------------------------------------------------------------------ setup

  private async init() {
    try {
      const [files, settings] = await Promise.all([api.files(this.id), api.settings(this.id)]);
      this.files = files;
      this.settings = settings;
    } catch (e) {
      toast((e as Error).message, "error");
      location.hash = "#/";
      return;
    }
    this.renderTree();
    this.syncSettingsUi();
    this.connectEvents();
    void this.refreshBibKeys();
    await this.openFile(this.settings.main);

    try {
      const last = await api.lastCompile(this.id);
      if (last) await this.applyResult(last);
      else void this.compile();
    } catch {
      /* the first compile will surface any problem */
    }
  }

  private build() {
    const e: Record<string, HTMLElement> = {};
    const mk = <K extends keyof HTMLElementTagNameMap>(name: string, tag: K, props: Record<string, unknown> = {}, ...kids: (Node | string | null)[]) => {
      const node = h(tag, props, ...kids);
      e[name] = node;
      return node;
    };

    this.engineSelect = h("select", { class: "select", "aria-label": "TeX engine" });
    const engines: [Engine, string][] = [
      ["pdflatex", "pdfLaTeX"],
      ["xelatex", "XeLaTeX"],
      ["lualatex", "LuaLaTeX"],
      ["tectonic", "Tectonic"],
    ];
    for (const [value, label] of engines) {
      const installed = this.health?.toolchain[value] ?? true;
      this.engineSelect.append(h("option", { value, disabled: !installed }, installed ? label : `${label} (not installed)`));
    }
    this.engineSelect.addEventListener("change", () => void this.saveSettings({ ...this.settings, engine: this.engineSelect.value as Engine }));

    this.autoToggle = h("input", { type: "checkbox", role: "switch" });
    this.autoToggle.checked = this.autoCompile;
    this.autoToggle.addEventListener("change", () => {
      this.autoCompile = this.autoToggle.checked;
      localStorage.setItem("scriptorium.auto", this.autoCompile ? "on" : "off");
    });

    const themeBtn = h("button", { class: "icon-btn", type: "button", "aria-label": "Toggle colour theme", title: "Toggle colour theme" });
    const paint = () => (themeBtn.innerHTML = theme.get() === "dark" ? icons.sun : icons.moon);
    paint();
    themeBtn.addEventListener("click", () => {
      theme.toggle();
      paint();
    });

    const uploadInput = h("input", { type: "file", multiple: true, hidden: true });
    uploadInput.addEventListener("change", () => {
      if (uploadInput.files) void this.uploadFiles([...uploadInput.files]);
      uploadInput.value = "";
    });

    const downloadMenu = h(
      "details",
      { class: "menu" },
      h("summary", { class: "btn", "aria-label": "Download" }, h("span", { html: icons.download }), "Download"),
      h(
        "div",
        { class: "menu-pop" },
        h("a", { href: api.pdfDownloadUrl(this.id), download: "" }, "PDF"),
        h("a", { href: api.zipUrl(this.id), download: "" }, "Source (.zip)"),
      ),
    );
    document.addEventListener("click", (ev) => {
      if (!downloadMenu.contains(ev.target as Node)) downloadMenu.removeAttribute("open");
    });

    const tree = mk("tree", "div", { class: "tree", role: "tree", "aria-label": "Project files" });
    const filesPanel = mk(
      "files",
      "aside",
      { class: "files" },
      h(
        "div",
        { class: "pane-bar" },
        h("span", { class: "pane-title" }, "Files"),
        h("div", { class: "pane-actions" },
          h("button", { class: "icon-btn", type: "button", title: "New file", "aria-label": "New file", html: icons.plus, onclick: () => void this.newEntry("file") }),
          h("button", { class: "icon-btn", type: "button", title: "New folder", "aria-label": "New folder", html: icons.newFolder, onclick: () => void this.newEntry("dir") }),
          h("button", { class: "icon-btn", type: "button", title: "Upload files", "aria-label": "Upload files", html: icons.upload, onclick: () => uploadInput.click() }),
        ),
      ),
      tree,
      uploadInput,
    );
    this.wireDrop(filesPanel);

    const presence = mk("presence", "div", { class: "presence", "aria-label": "People in this file" });
    const conn = mk("conn", "span", { class: "conn", role: "status" });

    const compileBtn = mk("compileBtn", "button", { class: "btn primary", type: "button", title: "Compile (Ctrl+Enter)", onclick: () => void this.compile() });
    compileBtn.innerHTML = `${icons.play}<span class="label">Recompile</span>`;

    const top = h(
      "header",
      { class: "topbar" },
      h("a", { class: "icon-btn", href: "#/", "aria-label": "All projects", title: "All projects", html: icons.back }),
      h("span", { class: "brand-mark", html: icons.logo }),
      h("h1", { class: "project-name", title: this.id }, this.id),
      this.engineSelect,
      h("div", { class: "spacer" }),
      presence,
      conn,
      h("label", { class: "switch", title: "Compile automatically after you stop typing" }, this.autoToggle, h("span", {}, "Auto-compile")),
      compileBtn,
      downloadMenu,
      h("button", { class: "btn", type: "button", onclick: () => void openShareDialog(this.id) }, h("span", { html: icons.share }), h("span", { class: "label" }, "Share")),
      themeBtn,
    );

    const editorHost = mk("editorHost", "div", { class: "editor-host" });
    const editorPane = mk(
      "editorPane",
      "section",
      { class: "editor-pane", "aria-label": "Editor" },
      h("div", { class: "pane-bar" }, mk("fileLabel", "span", { class: "pane-title" }), mk("notice", "span", { class: "notice", hidden: true })),
      editorHost,
    );

    const pdfScroll = mk("pdfScroll", "div", { class: "pdf-scroll", tabindex: "0", "aria-label": "PDF preview" });
    const empty = mk("pdfEmpty", "div", { class: "pdf-empty" }, "The preview appears here after the first successful compile.");
    const previewPane = mk(
      "previewPane",
      "section",
      { class: "preview-pane", "aria-label": "Preview" },
      h(
        "div",
        { class: "pane-bar" },
        mk("status", "span", { class: "status", role: "status" }),
        h("div", { class: "spacer" }),
        mk("pageInfo", "span", { class: "page-info" }),
        h("div", { class: "pane-actions" },
          h("button", { class: "icon-btn", type: "button", title: "Zoom out", "aria-label": "Zoom out", html: icons.zoomOut, onclick: () => this.pdf.zoomOut() }),
          mk("zoomLabel", "span", { class: "zoom-label" }, "Fit width"),
          h("button", { class: "icon-btn", type: "button", title: "Zoom in", "aria-label": "Zoom in", html: icons.zoomIn, onclick: () => this.pdf.zoomIn() }),
          h("button", { class: "icon-btn", type: "button", title: "Fit to width", "aria-label": "Fit to width", html: icons.fit, onclick: () => this.pdf.fitWidth() }),
        ),
      ),
      h("div", { class: "pdf-wrap" }, pdfScroll, empty),
    );

    const split = (name: string) => h("div", { class: "splitter", role: "separator", "aria-orientation": "vertical", "data-split": name, tabindex: "0" });
    const panes = mk("panes", "div", { class: "panes", "data-view": "editor" }, filesPanel, split("files"), editorPane, split("preview"), previewPane);

    const narrow = h(
      "div",
      { class: "view-toggle", role: "tablist" },
      h("button", { type: "button", role: "tab", class: "btn", onclick: () => (panes.dataset.view = "files") }, "Files"),
      h("button", { type: "button", role: "tab", class: "btn", onclick: () => (panes.dataset.view = "editor") }, "Editor"),
      h("button", { type: "button", role: "tab", class: "btn", onclick: () => (panes.dataset.view = "preview") }, "PDF"),
    );

    const logBody = mk("logBody", "div", { class: "log-body" });
    const tabProblems = mk("tabProblems", "button", { class: "tab", type: "button", role: "tab", onclick: () => this.setLogTab("problems") });
    const tabLog = mk("tabLog", "button", { class: "tab", type: "button", role: "tab", onclick: () => this.setLogTab("log") }, "Raw log");
    const logPanel = mk(
      "logPanel",
      "section",
      { class: "logpanel", "aria-label": "Compilation output" },
      h(
        "div",
        { class: "pane-bar log-bar" },
        h("div", { class: "tabs", role: "tablist" }, tabProblems, tabLog),
        h("div", { class: "spacer" }),
        h("button", { class: "btn small", type: "button", title: "Delete generated files", onclick: () => void this.clean() }, h("span", { html: icons.broom }), "Clean build"),
        h("button", { class: "icon-btn", type: "button", "aria-label": "Show or hide output", title: "Show or hide output", html: icons.panel, onclick: () => this.toggleLog() }),
      ),
      logBody,
    );

    this.root.replaceChildren(h("div", { class: "ws" }, top, narrow, panes, logPanel));
    this.el = e;
    this.applyLayout();
    this.initSplitters();
    this.renderStatus(null);
    this.renderLog();
    this.updateLogVisibility();
    this.renderPresence();
    this.renderConn("connecting");
  }

  // ----------------------------------------------------------------- layout

  private layout = (() => {
    try {
      const saved = JSON.parse(localStorage.getItem("scriptorium.layout") ?? "{}") as { files?: number; split?: number };
      return { files: saved.files ?? 232, split: saved.split ?? 0.5 };
    } catch {
      return { files: 232, split: 0.5 };
    }
  })();

  private applyLayout() {
    const panes = this.el.panes!;
    panes.style.setProperty("--files-w", `${this.layout.files}px`);
    panes.style.setProperty("--split", String(this.layout.split));
  }

  private initSplitters() {
    const panes = this.el.panes!;
    for (const handle of panes.querySelectorAll<HTMLElement>(".splitter")) {
      const kind = handle.dataset.split;
      const move = (clientX: number) => {
        const rect = panes.getBoundingClientRect();
        if (kind === "files") {
          this.layout.files = Math.min(520, Math.max(140, clientX - rect.left));
        } else {
          const left = rect.left + this.layout.files + 6;
          const width = rect.width - this.layout.files - 12;
          this.layout.split = Math.min(0.85, Math.max(0.15, (clientX - left) / width));
        }
        this.applyLayout();
      };
      const save = () => localStorage.setItem("scriptorium.layout", JSON.stringify(this.layout));
      handle.addEventListener("pointerdown", (ev) => {
        ev.preventDefault();
        handle.setPointerCapture(ev.pointerId);
        panes.classList.add("dragging");
        const onMove = (m: PointerEvent) => move(m.clientX);
        const onUp = () => {
          panes.classList.remove("dragging");
          handle.removeEventListener("pointermove", onMove);
          handle.removeEventListener("pointerup", onUp);
          save();
        };
        handle.addEventListener("pointermove", onMove);
        handle.addEventListener("pointerup", onUp);
      });
      handle.addEventListener("keydown", (ev) => {
        const step = ev.key === "ArrowLeft" ? -16 : ev.key === "ArrowRight" ? 16 : 0;
        if (!step) return;
        ev.preventDefault();
        if (kind === "files") this.layout.files = Math.min(520, Math.max(140, this.layout.files + step));
        else this.layout.split = Math.min(0.85, Math.max(0.15, this.layout.split + step / 1000));
        this.applyLayout();
        save();
      });
    }
  }

  private onKeydown = (ev: KeyboardEvent) => {
    if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter") {
      ev.preventDefault();
      void this.compile();
    }
  };

  // ------------------------------------------------------------- file tree

  private renderTree() {
    const tree = this.el.tree!;
    clear(tree);
    const nodes = buildTree(this.files);
    if (nodes.length === 0) {
      tree.append(h("p", { class: "tree-empty" }, "No files yet. Add one with the buttons above."));
      return;
    }
    const walk = (list: TreeNode[], depth: number) => {
      for (const node of list) {
        const isDir = node.kind === "dir";
        const open = !this.collapsed.has(node.path);
        const isMain = !isDir && node.path === this.settings.main;
        const row = h(
          "div",
          {
            class: `row${node.path === this.current ? " active" : ""}${isDir && node.path === this.selectedDir ? " selected" : ""}`,
            role: "treeitem",
            "aria-level": String(depth + 1),
            "aria-expanded": isDir ? String(open) : null,
            tabindex: "0",
            style: `--depth:${depth}`,
          },
          h("span", { class: `twisty${isDir ? "" : " none"}${isDir && open ? " open" : ""}`, html: isDir ? icons.chevron : "" }),
          h("span", { class: "kind", html: isDir ? icons.folder : icons.file }),
          h("span", { class: "name", title: node.path }, node.name),
          isMain ? h("span", { class: "badge", title: "Main document", html: icons.main }) : null,
          h(
            "span",
            { class: "row-actions" },
            !isDir && node.name.endsWith(".tex") && !isMain
              ? h("button", { class: "icon-btn", type: "button", title: "Set as main document", "aria-label": "Set as main document", html: icons.main, onclick: (ev: Event) => { ev.stopPropagation(); void this.saveSettings({ ...this.settings, main: node.path }); } })
              : null,
            h("button", { class: "icon-btn", type: "button", title: "Rename", "aria-label": `Rename ${node.name}`, html: icons.pencil, onclick: (ev: Event) => { ev.stopPropagation(); void this.renameEntry(node); } }),
            h("button", { class: "icon-btn danger", type: "button", title: "Delete", "aria-label": `Delete ${node.name}`, html: icons.trash, onclick: (ev: Event) => { ev.stopPropagation(); void this.deleteEntry(node); } }),
          ),
        );
        const activate = () => {
          if (isDir) {
            this.selectedDir = node.path;
            if (this.collapsed.has(node.path)) this.collapsed.delete(node.path);
            else this.collapsed.add(node.path);
            this.renderTree();
          } else {
            this.selectedDir = dirOf(node.path);
            void this.openFile(node.path);
            this.el.panes!.dataset.view = "editor";
          }
        };
        row.addEventListener("click", activate);
        row.addEventListener("keydown", (ev) => {
          if (ev.key === "Enter" || ev.key === " ") {
            ev.preventDefault();
            activate();
          }
        });
        tree.append(row);
        if (isDir && open) walk(node.children, depth + 1);
      }
    };
    walk(nodes, 0);
  }

  private async refreshFiles() {
    try {
      this.files = await api.files(this.id);
    } catch (e) {
      toast((e as Error).message, "error");
      return;
    }
    this.renderTree();
    void this.refreshBibKeys();
    if (this.current && !this.files.some((f) => f.path === this.current)) {
      this.closeEditor();
      this.current = null;
      this.showNotice("The file you were editing was removed.");
      const fallback = this.files.some((f) => f.path === this.settings.main) ? this.settings.main : this.files.find((f) => f.kind === "file")?.path;
      if (fallback) await this.openFile(fallback);
      else this.showPlaceholder("Select or create a file to start editing.");
    }
  }

  private async newEntry(kind: "file" | "dir") {
    const prefix = this.selectedDir ? `${this.selectedDir}/` : "";
    const path = await promptText({
      title: kind === "file" ? "New file" : "New folder",
      label: "Path",
      value: kind === "file" ? `${prefix}chapter.tex` : prefix,
      confirm: "Create",
      hint: "Use slashes to place it inside folders, for example sections/intro.tex.",
    });
    if (!path) return;
    try {
      await api.createEntry(this.id, path.replace(/^\/+/, ""), kind);
      await this.refreshFiles();
      if (kind === "file") await this.openFile(path.replace(/^\/+/, ""));
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async renameEntry(node: TreeNode) {
    const to = await promptText({ title: `Rename ${node.name}`, label: "New path", value: node.path, confirm: "Rename" });
    if (!to || to === node.path) return;
    try {
      await api.rename(this.id, node.path, to.replace(/^\/+/, ""));
      if (this.current === node.path || this.current?.startsWith(`${node.path}/`)) {
        const renamed = to.replace(/^\/+/, "") + (this.current === node.path ? "" : this.current!.slice(node.path.length));
        this.closeEditor();
        this.current = null;
        await this.refreshFiles();
        await this.openFile(renamed);
      } else {
        await this.refreshFiles();
      }
      this.settings = await api.settings(this.id);
      this.syncSettingsUi();
      this.renderTree();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async deleteEntry(node: TreeNode) {
    const ok = await confirmAction({
      title: `Delete ${node.name}?`,
      message: node.kind === "dir" ? "The folder and everything in it will be permanently removed." : "This file will be permanently removed.",
      confirm: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.deleteEntry(this.id, node.path);
      await this.refreshFiles();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async uploadFiles(files: File[]) {
    const prefix = this.selectedDir ? `${this.selectedDir}/` : "";
    let done = 0;
    for (const file of files) {
      try {
        await api.upload(this.id, `${prefix}${file.name}`, file);
        done += 1;
      } catch (e) {
        toast(`${file.name}: ${(e as Error).message}`, "error");
      }
    }
    if (done > 0) {
      toast(done === 1 ? "1 file uploaded" : `${done} files uploaded`);
      await this.refreshFiles();
    }
  }

  private wireDrop(target: HTMLElement) {
    target.addEventListener("dragover", (ev) => {
      if (ev.dataTransfer?.types.includes("Files")) {
        ev.preventDefault();
        target.classList.add("drop");
      }
    });
    target.addEventListener("dragleave", () => target.classList.remove("drop"));
    target.addEventListener("drop", (ev) => {
      ev.preventDefault();
      target.classList.remove("drop");
      if (ev.dataTransfer?.files.length) void this.uploadFiles([...ev.dataTransfer.files]);
    });
  }

  // --------------------------------------------------------------- settings

  private syncSettingsUi() {
    this.engineSelect.value = this.settings.engine;
  }

  private async saveSettings(next: Settings) {
    try {
      this.settings = await api.saveSettings(this.id, next);
      this.syncSettingsUi();
      this.renderTree();
      if (this.autoCompile) void this.compile();
    } catch (e) {
      toast((e as Error).message, "error");
      this.syncSettingsUi();
    }
  }

  // ----------------------------------------------------------------- editor

  private closeEditor() {
    this.editor?.destroy();
    this.editor = null;
    this.provider?.destroy();
    this.provider = null;
    clear(this.el.editorHost!);
    this.renderPresence();
  }

  private showPlaceholder(message: string, extra?: Node) {
    clear(this.el.editorHost!);
    this.el.editorHost!.append(h("div", { class: "placeholder" }, h("p", {}, message), extra ?? null));
  }

  private showNotice(message: string) {
    const n = this.el.notice!;
    n.textContent = message;
    n.hidden = false;
    window.setTimeout(() => {
      if (n.textContent === message) n.hidden = true;
    }, 8000);
  }

  private async openFile(path: string, line?: number) {
    if (line) this.pendingLine = line;
    if (this.current === path && (this.editor || !TEXT_EXT.has(extOf(path)))) {
      if (line && this.editor) this.editor.reveal(line);
      return;
    }
    this.closeEditor();
    this.current = path;
    this.el.fileLabel!.textContent = path;
    this.renderTree();
    const ext = extOf(path);

    if (IMAGE_EXT.has(ext)) {
      this.showPlaceholder("", h("img", { class: "image-preview", src: api.rawUrl(this.id, path), alt: path }));
      return;
    }
    if (!TEXT_EXT.has(ext) && ext !== "") {
      this.showPlaceholder("This file type cannot be edited here.", h("a", { class: "btn", href: api.rawUrl(this.id, path), download: baseName(path) }, "Download file"));
      return;
    }

    const provider = new DocProvider(
      api.wsDocUrl(this.id),
      { path },
      {
        onStatus: (s) => this.renderConn(s),
        onSynced: () => {
          if (this.provider !== provider) return;
          this.editor?.setEditable(true);
          this.el.editorHost!.classList.remove("loading");
          if (this.result) this.editor?.setDiagnostics(this.result.diagnostics, path);
          if (this.pendingLine) {
            this.editor?.reveal(this.pendingLine);
            this.pendingLine = null;
          }
        },
        onReset: () => {
          this.showNotice("This document changed on the server, so it was reloaded.");
          const again = this.current;
          this.closeEditor();
          this.current = null;
          if (again) void this.openFile(again);
        },
      },
    );
    this.provider = provider;
    this.el.editorHost!.classList.add("loading");
    this.editor = createEditor({
      parent: this.el.editorHost!,
      ytext: provider.doc.getText("content"),
      awareness: provider.awareness,
      user: this.user,
      theme: theme.get(),
      symbols: { bibKeys: () => this.bibKeys },
      onLocalChange: () => this.scheduleAuto(),
      onCompile: () => void this.compile(),
    });
    provider.awareness.on("change", () => this.renderPresence());
    this.renderPresence();
  }

  private renderConn(status: ConnectionStatus) {
    const c = this.el.conn!;
    const label = status === "connected" ? "Live" : status === "connecting" ? "Connecting" : "Reconnecting";
    c.className = `conn ${status}`;
    c.replaceChildren(h("span", { class: "dot" }), h("span", { class: "label" }, label));
    c.title =
      status === "connected"
        ? "Connected. Edits sync instantly and save automatically."
        : "Connection to the server was lost. Your edits will sync when it returns.";
  }

  private renderPresence() {
    const box = this.el.presence!;
    clear(box);
    const mine = h(
      "button",
      { class: "avatar me", type: "button", style: `--c:${this.user.color}`, title: `${this.user.name} (you). Click to change your name.`, "aria-label": "Change your display name" },
      initials(this.user.name),
    );
    mine.addEventListener("click", () => void this.renameSelf());
    const others: HTMLElement[] = [];
    if (this.provider) {
      for (const [clientId, state] of this.provider.awareness.getStates()) {
        if (clientId === this.provider.doc.clientID) continue;
        const u = (state as { user?: User }).user;
        if (!u) continue;
        others.push(h("span", { class: "avatar", style: `--c:${u.color}`, title: u.name }, initials(u.name)));
      }
    }
    box.append(...others.slice(0, 5));
    if (others.length > 5) box.append(h("span", { class: "avatar more" }, `+${others.length - 5}`));
    box.append(mine);
  }

  private async renameSelf() {
    const name = await promptText({ title: "Your display name", label: "Name", value: this.user.name, confirm: "Save", hint: "Collaborators see this next to your cursor." });
    if (!name) return;
    this.user = { ...this.user, name: name.slice(0, 40) };
    saveUser(this.user);
    this.provider?.awareness.setLocalStateField("user", this.user);
    this.renderPresence();
  }

  private async refreshBibKeys() {
    const bibs = this.files.filter((f) => f.kind === "file" && f.path.endsWith(".bib") && f.size < 4_000_000);
    const keys = new Set<string>();
    await Promise.all(
      bibs.map(async (b) => {
        try {
          const text = await api.rawText(this.id, b.path);
          for (const m of text.matchAll(/@\w+\s*\{\s*([^,\s]+)\s*,/g)) if (m[1]) keys.add(m[1]);
        } catch {
          /* unreadable bibliography: skip */
        }
      }),
    );
    this.bibKeys = [...keys].sort();
  }

  // ------------------------------------------------------------ compilation

  private async compile() {
    if (this.compiling) {
      this.pending = true;
      return;
    }
    this.compiling = true;
    this.renderStatus("compiling");
    this.el.compileBtn!.setAttribute("aria-busy", "true");
    this.el.compileBtn!.classList.add("busy");
    try {
      const result = await api.compile(this.id);
      await this.applyResult(result);
    } catch (e) {
      this.renderStatus(null, (e as Error).message);
      toast((e as Error).message, "error");
    } finally {
      this.compiling = false;
      this.el.compileBtn!.removeAttribute("aria-busy");
      this.el.compileBtn!.classList.remove("busy");
      if (this.pending) {
        this.pending = false;
        void this.compile();
      }
    }
  }

  private async applyResult(result: CompileResult) {
    if (this.destroyed || result.id < this.lastApplied) return;
    this.lastApplied = result.id;
    this.result = result;
    this.renderStatus(result);
    this.renderLog();
    if (this.current) this.editor?.setDiagnostics(result.diagnostics, this.current);
    if (result.diagnostics.some((d) => d.severity === "error") && !this.logOpen) {
      this.logOpen = true;
      this.updateLogVisibility();
    }
    if (result.pdf) {
      try {
        const shown = await this.pdf.load(api.pdfUrl(this.id, result.id));
        this.el.pdfEmpty!.hidden = shown;
      } catch (e) {
        toast(`Could not display the PDF: ${(e as Error).message}`, "error");
      }
    } else {
      this.el.pdfEmpty!.hidden = this.pdf.pageCount > 0;
    }
  }

  private async clean() {
    try {
      await api.clean(this.id);
      this.pdf.clear();
      this.el.pdfEmpty!.hidden = false;
      this.result = null;
      this.renderStatus(null);
      this.renderLog();
      toast("Generated files removed");
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private renderStatus(result: CompileResult | "compiling" | null, error?: string) {
    const s = this.el.status!;
    s.className = "status";
    if (result === "compiling") {
      s.classList.add("busy");
      s.textContent = "Compiling…";
      return;
    }
    if (error) {
      s.classList.add("bad");
      s.textContent = error;
      return;
    }
    if (!result) {
      s.textContent = "Not compiled yet";
      return;
    }
    const errors = result.diagnostics.filter((d) => d.severity === "error").length;
    const warnings = result.diagnostics.length - errors;
    const secs = (result.duration_ms / 1000).toFixed(1);
    if (result.timed_out) {
      s.classList.add("bad");
      s.textContent = "Timed out";
    } else if (errors > 0 || !result.ok) {
      s.classList.add("bad");
      s.textContent = errors > 0 ? `${errors} error${errors === 1 ? "" : "s"}` : "Compile failed";
    } else {
      s.classList.add("good");
      s.textContent = warnings > 0 ? `Compiled in ${secs} s, ${warnings} warning${warnings === 1 ? "" : "s"}` : `Compiled in ${secs} s`;
    }
  }

  // ------------------------------------------------------------------- logs

  private setLogTab(tab: LogTab) {
    this.logTab = tab;
    if (!this.logOpen) {
      this.logOpen = true;
      this.updateLogVisibility();
    }
    this.renderLog();
  }

  private toggleLog() {
    this.logOpen = !this.logOpen;
    this.updateLogVisibility();
  }

  private updateLogVisibility() {
    this.el.logPanel!.classList.toggle("open", this.logOpen);
  }

  private renderLog() {
    const diags = this.result?.diagnostics ?? [];
    const errors = diags.filter((d) => d.severity === "error").length;
    const warnings = diags.length - errors;
    const problemsLabel: (string | Node)[] = ["Problems"];
    if (errors) problemsLabel.push(h("span", { class: "count bad" }, String(errors)));
    if (warnings) problemsLabel.push(h("span", { class: "count warn" }, String(warnings)));
    this.el.tabProblems!.replaceChildren(...problemsLabel);
    this.el.tabProblems!.setAttribute("aria-selected", String(this.logTab === "problems"));
    this.el.tabLog!.setAttribute("aria-selected", String(this.logTab === "log"));
    this.el.tabProblems!.classList.toggle("active", this.logTab === "problems");
    this.el.tabLog!.classList.toggle("active", this.logTab === "log");

    const body = this.el.logBody!;
    clear(body);
    if (!this.result) {
      body.append(h("p", { class: "log-empty" }, "Compile the project to see errors and warnings here."));
      return;
    }
    if (this.logTab === "log") {
      body.append(h("pre", { class: "raw-log" }, this.result.log || "(no output)"));
      return;
    }
    if (diags.length === 0) {
      body.append(h("p", { class: "log-empty" }, this.result.ok ? "No problems found." : "The compiler reported a failure without a specific message. Open the raw log for details."));
      return;
    }
    const list = h("ul", { class: "problems" });
    const sorted = [...diags].sort((a, b) => Number(b.severity === "error") - Number(a.severity === "error"));
    for (const d of sorted) list.append(this.problemRow(d));
    body.append(list);
  }

  private problemRow(d: Diagnostic): HTMLElement {
    const where = d.file ? `${d.file}${d.line ? `:${d.line}` : ""}` : "";
    const row = h(
      "li",
      { class: `problem ${d.severity}` },
      h("span", { class: "sev", html: d.severity === "error" ? icons.error : icons.warn }),
      h("span", { class: "msg" }, d.message),
      where ? h("span", { class: "where" }, where) : null,
    );
    if (d.file) {
      row.tabIndex = 0;
      row.setAttribute("role", "button");
      const go = () => {
        this.el.panes!.dataset.view = "editor";
        void this.openFile(d.file as string, d.line ?? undefined);
      };
      row.addEventListener("click", go);
      row.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter") go();
      });
    }
    return row;
  }

  // ----------------------------------------------------------------- events

  private connectEvents() {
    if (this.destroyed) return;
    const ws = new WebSocket(api.wsEventsUrl(this.id));
    this.events = ws;
    ws.onopen = () => {
      this.eventsAttempts = 0;
    };
    ws.onmessage = (msg) => {
      try {
        const data = JSON.parse(String(msg.data)) as { type: string; result?: CompileResult };
        void this.onEvent(data);
      } catch {
        /* ignore malformed events */
      }
    };
    ws.onclose = () => {
      if (this.destroyed) return;
      this.eventsAttempts += 1;
      this.eventsTimer = window.setTimeout(() => this.connectEvents(), Math.min(5000, 400 * 2 ** Math.min(this.eventsAttempts, 4)));
    };
    ws.onerror = () => ws.close();
  }

  private async onEvent(ev: { type: string; result?: CompileResult }) {
    switch (ev.type) {
      case "compiled":
        if (!this.compiling && ev.result && ev.result.id > this.lastApplied) {
          const full = await api.lastCompile(this.id).catch(() => null);
          if (full) await this.applyResult(full);
        }
        break;
      case "files":
        await this.refreshFiles();
        break;
      case "settings":
        this.settings = await api.settings(this.id).catch(() => this.settings);
        this.syncSettingsUi();
        this.renderTree();
        break;
      case "cleaned":
        this.pdf.clear();
        this.el.pdfEmpty!.hidden = false;
        break;
      case "resync":
        await this.refreshFiles();
        break;
      default:
        break;
    }
  }
}
