// The Compilers dialog and the "install a compiler" card.

import type { CompilerInfo, JobStatus, ToolchainStatus } from "./api";
import { confirmAction, h, showDialog } from "./dom";
import { icons } from "./icons";
import { toolchain } from "./toolchain";

export function formatBytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${Math.round(n / 1e6)} MB`;
  return `${Math.max(1, Math.round(n / 1e3))} KB`;
}

const finished = (job: JobStatus | null): boolean => !job || ["done", "failed", "cancelled"].includes(job.phase);

function phaseLabel(job: JobStatus): string {
  switch (job.phase) {
    case "downloading":
      return job.total > 0 ? `Downloading ${formatBytes(job.downloaded)} of ${formatBytes(job.total)}` : "Downloading";
    case "verifying":
      return "Verifying the download";
    case "unpacking":
      return "Unpacking";
    case "testing":
      return "Testing the installation";
    default:
      return "";
  }
}

function progress(job: JobStatus): HTMLElement {
  const known = job.phase === "downloading" && job.total > 0;
  const bar = h("div", { class: `bar${known ? "" : " indeterminate"}` });
  if (known) bar.style.width = `${Math.min(100, (job.downloaded / job.total) * 100).toFixed(1)}%`;
  return h(
    "div",
    { class: "install-progress", role: "status" },
    h("div", { class: "progress" }, bar),
    h(
      "div",
      { class: "progress-row" },
      h("span", {}, phaseLabel(job)),
      h("button", { class: "btn small", type: "button", onclick: () => void toolchain.cancel() }, "Cancel"),
    ),
  );
}

function failure(status: ToolchainStatus, id: string): HTMLElement | null {
  const job = status.job;
  return job && job.id === id && job.phase === "failed" && job.error
    ? h("p", { class: "install-error", role: "alert" }, `Installation failed: ${job.error}`)
    : null;
}

function sizeLine(c: CompilerInfo): string {
  if (!c.download_bytes || !c.installed_bytes) return "";
  return `Download ${formatBytes(c.download_bytes)}, ${formatBytes(c.installed_bytes)} on disk`;
}

async function confirmRemove(c: CompilerInfo) {
  const ok = await confirmAction({
    title: `Remove ${c.name}?`,
    message: "Its files are deleted from this computer. Your projects are not affected, and you can install it again at any time.",
    confirm: "Remove",
    danger: true,
  });
  if (ok) await toolchain.remove(c.id);
}

function card(c: CompilerInfo, status: ToolchainStatus): HTMLElement {
  const busy = !finished(status.job);
  const mine = busy && status.job?.id === c.id;
  let action: HTMLElement | null;
  if (mine && status.job) {
    action = progress(status.job);
  } else if (c.installed) {
    action = h(
      "div",
      { class: "card-action" },
      h("span", { class: "tag ok" }, h("span", { html: icons.check }), "Installed"),
      status.can_manage
        ? h("button", { class: "btn danger-ghost", type: "button", onclick: () => void confirmRemove(c) }, h("span", { html: icons.trash }), "Remove")
        : null,
    );
  } else if (!c.available) {
    action = h("p", { class: "hint" }, "No download is available for this computer.");
  } else if (status.can_manage) {
    action = h(
      "div",
      { class: "card-action" },
      h(
        "button",
        { class: `btn ${c.recommended ? "primary" : ""}`, type: "button", disabled: busy, onclick: () => void toolchain.install(c.id) },
        h("span", { html: icons.download }),
        `Install ${c.name}`,
      ),
    );
  } else {
    action = null;
  }
  return h(
    "section",
    { class: `compiler-card${c.installed ? " installed" : ""}` },
    h(
      "div",
      { class: "card-head" },
      h("h3", {}, c.name),
      h("span", { class: "version" }, c.version),
      c.recommended && !c.installed ? h("span", { class: "tag" }, "Recommended") : null,
    ),
    h("p", {}, c.summary),
    h("div", { class: "chips" }, ...c.engines.map((e) => h("span", { class: "chip" }, e))),
    sizeLine(c) ? h("p", { class: "hint" }, sizeLine(c)) : null,
    h("p", { class: "hint" }, c.notes),
    action,
    failure(status, c.id),
  );
}

function render(body: HTMLElement, close: () => void, status: ToolchainStatus) {
  const sys = Object.entries(status.system)
    .filter(([, v]) => v)
    .map(([k]) => k);
  const parts: (Node | null)[] = [
    h("h2", {}, "LaTeX compilers"),
    h(
      "p",
      { class: "lede" },
      "Chrysanthemum can install a compiler for you, inside its own folder. Nothing is installed system-wide, and removing a compiler deletes everything it added.",
    ),
    status.can_manage ? null : h("p", { class: "hint warn" }, "Only the computer running Chrysanthemum can install or remove compilers."),
    ...status.compilers.map((c) => card(c, status)),
    sys.length
      ? h(
          "p",
          { class: "hint system" },
          h("span", { html: icons.chip }),
          `Already on this computer: ${sys.join(", ")}. These belong to your system, so Chrysanthemum uses them but never removes them.`,
        )
      : null,
    status.platform ? h("p", { class: "hint" }, `Detected: ${status.platform}`) : null,
    h("div", { class: "dialog-actions" }, h("button", { class: "btn primary", type: "button", onclick: close }, "Done")),
  ];
  body.replaceChildren(...parts.filter((p): p is Node => p !== null));
}

export async function openCompilersDialog() {
  await toolchain.refresh();
  let unsubscribe = () => {};
  try {
    await showDialog((body, close) => {
      body.classList.add("compilers");
      unsubscribe = toolchain.subscribe((s) => render(body, close, s));
    });
  } finally {
    unsubscribe();
  }
}

/** Shown where the PDF would be when nothing can compile yet. */
export function installCard(status: ToolchainStatus): HTMLElement {
  const pick = status.compilers.find((c) => c.recommended && c.available) ?? status.compilers.find((c) => c.available);
  const running = !finished(status.job);
  let action: HTMLElement | null = null;
  if (!status.can_manage) {
    action = h("p", { class: "hint" }, "Ask whoever runs Chrysanthemum to install a compiler from the Compilers menu.");
  } else if (running && status.job) {
    action = progress(status.job);
  } else if (pick) {
    action = h(
      "div",
      { class: "card-action center" },
      h("button", { class: "btn primary", type: "button", onclick: () => void toolchain.install(pick.id) }, h("span", { html: icons.download }), `Install ${pick.name}`, pick.download_bytes ? h("small", {}, ` ${formatBytes(pick.download_bytes)}`) : null),
      h("button", { class: "btn", type: "button", onclick: () => void openCompilersDialog() }, "Choose a compiler"),
    );
  } else {
    action = h("p", { class: "hint" }, "No compiler download is available for this computer. Install TeX Live or Tectonic yourself, then restart Chrysanthemum.");
  }
  return h(
    "div",
    { class: "install-card" },
    h("span", { class: "empty-logo", html: icons.logo }),
    h("h3", {}, "Install a LaTeX compiler"),
    h("p", {}, "A compiler turns your LaTeX into the PDF shown here. Chrysanthemum can install one in about a minute, in its own folder."),
    action,
    pick ? failure(status, pick.id) : null,
  );
}
