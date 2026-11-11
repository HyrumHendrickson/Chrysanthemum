// Tiny DOM helpers: element builder, toasts and accessible dialogs.

type Child = Node | string | null | undefined | false;
type Props = {
  class?: string;
  html?: string;
  [key: string]: unknown;
};

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Props = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") el.className = String(value);
    else if (key === "html") el.innerHTML = String(value);
    else if (key.startsWith("on") && typeof value === "function") {
      el.addEventListener(key.slice(2).toLowerCase(), value as EventListener);
    } else if (value === true) el.setAttribute(key, "");
    else el.setAttribute(key, String(value));
  }
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child);
  }
  return el;
}

export function clear(el: Element) {
  el.replaceChildren();
}

export function toast(message: string, kind: "info" | "error" = "info") {
  const host = document.getElementById("toasts");
  if (!host) return;
  const el = h("div", { class: `toast ${kind}` }, message);
  host.append(el);
  window.setTimeout(() => el.remove(), kind === "error" ? 7000 : 3500);
}

function dialog(build: (dlg: HTMLDialogElement, close: (v: unknown) => void) => void): Promise<unknown> {
  return new Promise((resolve) => {
    const dlg = h("dialog", { class: "dialog" });
    let result: unknown = null;
    const close = (value: unknown) => {
      result = value;
      dlg.close();
    };
    dlg.addEventListener("close", () => {
      dlg.remove();
      resolve(result);
    });
    dlg.addEventListener("click", (e) => {
      if (e.target === dlg) close(null);
    });
    build(dlg, close);
    document.body.append(dlg);
    dlg.showModal();
  });
}

export async function promptText(opts: {
  title: string;
  label: string;
  value?: string;
  confirm?: string;
  hint?: string;
}): Promise<string | null> {
  const result = await dialog((dlg, close) => {
    const input = h("input", {
      type: "text",
      value: opts.value ?? "",
      autocomplete: "off",
      spellcheck: "false",
      "aria-label": opts.label,
    });
    const form = h(
      "form",
      {
        method: "dialog",
        onsubmit: (e: Event) => {
          e.preventDefault();
          const v = input.value.trim();
          if (v) close(v);
        },
      },
      h("h2", {}, opts.title),
      h("label", {}, opts.label, input),
      opts.hint ? h("p", { class: "hint" }, opts.hint) : null,
      h(
        "div",
        { class: "dialog-actions" },
        h("button", { type: "button", class: "btn", onclick: () => close(null) }, "Cancel"),
        h("button", { type: "submit", class: "btn primary" }, opts.confirm ?? "OK"),
      ),
    );
    dlg.append(form);
    queueMicrotask(() => {
      input.focus();
      const dot = input.value.lastIndexOf(".");
      input.setSelectionRange(0, dot > 0 ? dot : input.value.length);
    });
  });
  return (result as string | null) ?? null;
}

export async function confirmAction(opts: {
  title: string;
  message: string;
  confirm: string;
  danger?: boolean;
}): Promise<boolean> {
  const result = await dialog((dlg, close) => {
    const ok = h("button", { type: "button", class: `btn ${opts.danger ? "danger" : "primary"}`, onclick: () => close(true) }, opts.confirm);
    dlg.append(
      h("h2", {}, opts.title),
      h("p", {}, opts.message),
      h("div", { class: "dialog-actions" }, h("button", { type: "button", class: "btn", onclick: () => close(false) }, "Cancel"), ok),
    );
    queueMicrotask(() => ok.focus());
  });
  return result === true;
}

export async function showDialog(build: (body: HTMLElement, close: () => void) => void): Promise<void> {
  await dialog((dlg, close) => {
    const body = h("div", { class: "dialog-body" });
    dlg.append(body);
    build(body, () => close(null));
  });
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const area = h("textarea", { style: "position:fixed;opacity:0" });
    area.value = text;
    document.body.append(area);
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    return ok;
  }
}

export function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  const min = Math.round(diff / 60000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const hours = Math.round(min / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days} d ago`;
  return new Date(ms).toLocaleDateString();
}

export function debounce<A extends unknown[]>(fn: (...args: A) => void, ms: number) {
  let t: number | undefined;
  const wrapped = (...args: A) => {
    window.clearTimeout(t);
    t = window.setTimeout(() => fn(...args), ms);
  };
  wrapped.cancel = () => window.clearTimeout(t);
  return wrapped;
}
