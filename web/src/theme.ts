export type Mode = "light" | "dark";

const listeners = new Set<(mode: Mode) => void>();

export const theme = {
  get(): Mode {
    return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
  },
  set(mode: Mode) {
    document.documentElement.dataset.theme = mode;
    try {
      localStorage.setItem("scriptorium.theme", mode);
    } catch {
      /* preference lasts for this session only */
    }
    for (const fn of listeners) fn(mode);
  },
  toggle() {
    this.set(this.get() === "dark" ? "light" : "dark");
  },
  subscribe(fn: (mode: Mode) => void): () => void {
    listeners.add(fn);
    return () => listeners.delete(fn);
  },
};
