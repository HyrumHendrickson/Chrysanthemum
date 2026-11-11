import { type Health, type ProjectInfo, api } from "./api";
import { clear, confirmAction, h, relativeTime, toast } from "./dom";
import { icons } from "./icons";
import { openShareDialog } from "./share";
import { theme } from "./theme";

export class Home {
  private list = h("ul", { class: "project-list" });
  private projects: ProjectInfo[] = [];

  constructor(
    private readonly root: HTMLElement,
    private readonly health: Health | null,
  ) {
    this.render();
    void this.refresh();
  }

  destroy() {
    /* nothing to release */
  }

  private render() {
    const input = h("input", {
      type: "text",
      name: "name",
      placeholder: "thesis-2026",
      autocomplete: "off",
      spellcheck: "false",
      maxlength: "64",
      pattern: "[A-Za-z0-9_][A-Za-z0-9_\\-]*",
      title: "Letters, digits, '-' and '_'",
      "aria-label": "New project name",
      required: true,
    });
    const form = h(
      "form",
      {
        class: "new-project",
        onsubmit: async (e: Event) => {
          e.preventDefault();
          try {
            const info = await api.createProject(input.value.trim());
            location.hash = `#/p/${encodeURIComponent(info.id)}`;
          } catch (err) {
            toast((err as Error).message, "error");
          }
        },
      },
      input,
      h("button", { class: "btn primary", type: "submit" }, "Create project"),
    );

    const themeBtn = h("button", { class: "icon-btn", type: "button", "aria-label": "Toggle colour theme" });
    const paintTheme = () => (themeBtn.innerHTML = theme.get() === "dark" ? icons.sun : icons.moon);
    paintTheme();
    themeBtn.addEventListener("click", () => {
      theme.toggle();
      paintTheme();
    });

    const warning =
      this.health && !this.health.toolchain_ok
        ? h(
            "div",
            { class: "banner warn", role: "alert" },
            "No TeX installation was found on this computer. You can write and collaborate, but compiling needs TeX Live, MiKTeX or Tectonic. Install one, then restart Scriptorium.",
          )
        : null;

    clear(this.root);
    this.root.append(
      h(
        "div",
        { class: "home" },
        h(
          "header",
          { class: "home-bar" },
          h("a", { class: "wordmark", href: "#/" }, h("span", { html: icons.logo }), "Scriptorium"),
          h(
            "div",
            { class: "bar-actions" },
            h("button", { class: "btn", type: "button", onclick: () => void openShareDialog() }, h("span", { html: icons.share }), "Work together"),
            themeBtn,
          ),
        ),
        h(
          "main",
          { class: "home-main" },
          h("h1", {}, "Projects"),
          h("p", { class: "lede" }, "Documents live in plain folders on this computer. Compiling and collaboration both run locally."),
          warning,
          form,
          this.list,
        ),
      ),
    );
  }

  private async refresh() {
    try {
      this.projects = await api.projects();
    } catch (e) {
      toast((e as Error).message, "error");
      return;
    }
    clear(this.list);
    if (this.projects.length === 0) {
      this.list.append(h("li", { class: "empty-row" }, "No projects yet. Name one above to start writing."));
      return;
    }
    for (const p of this.projects) {
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
      this.list.append(
        h(
          "li",
          { class: "project-row" },
          h("a", { class: "project-link", href: `#/p/${encodeURIComponent(p.id)}` }, h("span", { class: "project-title" }, p.id), h("span", { class: "project-meta" }, `Edited ${relativeTime(p.modified)}`)),
          del,
        ),
      );
    }
  }
}
