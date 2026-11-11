import { type ProjectInfo, type ToolchainStatus, api } from "./api";
import { openCompilersDialog } from "./compilers";
import { clear, confirmAction, h, promptText, relativeTime, toast } from "./dom";
import { icons } from "./icons";
import { openShareDialog } from "./share";
import { theme } from "./theme";
import { toolchain } from "./toolchain";

export class Home {
  private rows = h("ul", { class: "project-rows" });
  private compilerBox = h("div", { class: "side-compilers" });
  private banner = h("div");
  private projects: ProjectInfo[] = [];
  private query = "";
  private unsubscribe: () => void;

  constructor(private readonly root: HTMLElement) {
    this.render();
    this.unsubscribe = toolchain.subscribe((s) => this.paintCompilers(s));
    void toolchain.refresh();
    void this.refresh();
  }

  destroy() {
    this.unsubscribe();
  }

  private async createProject() {
    const name = await promptText({
      title: "New project",
      label: "Project name",
      value: "",
      confirm: "Create",
      hint: "Letters, digits, '-' and '_' only. Each project is a folder on this computer.",
    });
    if (!name) return;
    try {
      const info = await api.createProject(name);
      location.hash = `#/p/${encodeURIComponent(info.id)}`;
    } catch (err) {
      toast((err as Error).message, "error");
    }
  }

  private render() {
    const themeBtn = h("button", { class: "icon-btn", type: "button", "aria-label": "Toggle colour theme", title: "Toggle colour theme" });
    const paintTheme = () => (themeBtn.innerHTML = theme.get() === "dark" ? icons.sun : icons.moon);
    paintTheme();
    themeBtn.addEventListener("click", () => {
      theme.toggle();
      paintTheme();
    });

    const search = h("input", { type: "search", class: "search", placeholder: "Search projects", "aria-label": "Search projects", autocomplete: "off" });
    search.addEventListener("input", () => {
      this.query = search.value.trim().toLowerCase();
      this.paintRows();
    });

    clear(this.root);
    this.root.append(
      h(
        "div",
        { class: "home" },
        h(
          "header",
          { class: "topbar" },
          h("a", { class: "wordmark", href: "#/" }, h("span", { class: "brand-mark", html: icons.logo }), "Chrysanthemum"),
          h("div", { class: "spacer" }),
          h("button", { class: "btn", type: "button", onclick: () => void openCompilersDialog() }, h("span", { html: icons.chip }), "Compilers"),
          h("button", { class: "btn", type: "button", onclick: () => void openShareDialog() }, h("span", { html: icons.share }), "Work together"),
          themeBtn,
        ),
        h(
          "div",
          { class: "home-body" },
          h(
            "aside",
            { class: "home-side" },
            h("button", { class: "btn primary block", type: "button", onclick: () => void this.createProject() }, h("span", { html: icons.plus }), "New Project"),
            h("nav", { class: "side-nav", "aria-label": "Projects" }, h("a", { class: "side-link active", href: "#/", "aria-current": "page" }, "All projects")),
            h("div", { class: "spacer" }),
            this.compilerBox,
          ),
          h(
            "main",
            { class: "home-main" },
            h("div", { class: "home-title" }, h("h1", {}, "All Projects"), search),
            this.banner,
            h(
              "div",
              { class: "project-table" },
              h("div", { class: "project-head" }, h("span", {}, "Title"), h("span", {}, "Last modified"), h("span", { class: "actions-col" }, "Actions")),
              this.rows,
            ),
          ),
        ),
      ),
    );
  }

  private paintCompilers(s: ToolchainStatus) {
    const names = s.compilers.filter((c) => c.installed).map((c) => c.name);
    const engines = Object.entries(s.active)
      .filter(([k, v]) => v && k !== "latexmk")
      .map(([k]) => k);
    clear(this.compilerBox);
    this.compilerBox.append(
      h("div", { class: "side-label" }, "Compiler"),
      engines.length
        ? h("div", { class: "side-status ok" }, h("span", { html: icons.check }), names.length ? names.join(", ") : "Found on this computer")
        : h("div", { class: "side-status none" }, "None installed"),
      h("button", { class: "btn small block", type: "button", onclick: () => void openCompilersDialog() }, engines.length ? "Manage compilers" : "Install a compiler"),
    );
    clear(this.banner);
    if (engines.length === 0) {
      this.banner.append(
        h(
          "div",
          { class: "banner warn", role: "alert" },
          h("div", {}, h("strong", {}, "No LaTeX compiler yet. "), "You can write and collaborate, but turning documents into PDFs needs one. Chrysanthemum can install it for you."),
          h("button", { class: "btn primary", type: "button", onclick: () => void openCompilersDialog() }, "Install a compiler"),
        ),
      );
    }
  }

  private paintRows() {
    clear(this.rows);
    const shown = this.projects.filter((p) => p.id.toLowerCase().includes(this.query));
    if (this.projects.length === 0) {
      this.rows.append(
        h(
          "li",
          { class: "empty-row" },
          h("span", { class: "empty-logo", html: icons.logo }),
          h("p", {}, "No projects yet."),
          h("button", { class: "btn primary", type: "button", onclick: () => void this.createProject() }, "Create your first project"),
        ),
      );
      return;
    }
    if (shown.length === 0) {
      this.rows.append(h("li", { class: "empty-row" }, h("p", {}, "No projects match your search.")));
      return;
    }
    for (const p of shown) {
      const del = h("button", { class: "icon-btn danger", type: "button", "aria-label": `Delete ${p.id}`, title: "Delete project", html: icons.trash });
      del.addEventListener("click", async () => {
        const ok = await confirmAction({
          title: `Delete “${p.id}”?`,
          message: "All files in this project will be permanently removed from this computer.",
          confirm: "Delete project",
          danger: true,
        });
        if (!ok) return;
        try {
          await api.deleteProject(p.id);
          await this.refresh();
        } catch (e) {
          toast((e as Error).message, "error");
        }
      });
      this.rows.append(
        h(
          "li",
          { class: "project-row" },
          h("a", { class: "project-title", href: `#/p/${encodeURIComponent(p.id)}` }, p.id),
          h("span", { class: "project-meta" }, relativeTime(p.modified)),
          h(
            "span",
            { class: "row-tools" },
            h("a", { class: "icon-btn", href: api.zipUrl(p.id), download: "", title: "Download source (.zip)", "aria-label": `Download ${p.id}`, html: icons.download }),
            del,
          ),
        ),
      );
    }
  }

  private async refresh() {
    try {
      this.projects = await api.projects();
    } catch (e) {
      toast((e as Error).message, "error");
      return;
    }
    this.paintRows();
  }
}
