// Shared, live view of the LaTeX compilers on this computer.

import { type CompilerId, type Engine, type ToolchainStatus, api } from "./api";
import { toast } from "./dom";

type Listener = (status: ToolchainStatus) => void;

const ENGINES: Engine[] = ["pdflatex", "xelatex", "lualatex", "tectonic"];

class ToolchainStore {
  status: ToolchainStatus | null = null;
  private listeners = new Set<Listener>();
  private timer: number | undefined;

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    if (this.status) fn(this.status);
    this.schedule();
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** True when at least one engine can run. Optimistic until the first answer. */
  hasEngine(): boolean {
    return this.status ? ENGINES.some((e) => this.supports(e)) : true;
  }

  supports(engine: Engine): boolean {
    return this.status ? this.status.active[engine] : true;
  }

  firstSupported(): Engine | null {
    return ENGINES.find((e) => this.supports(e)) ?? null;
  }

  installing(): boolean {
    const job = this.status?.job;
    return !!job && !["done", "failed", "cancelled"].includes(job.phase);
  }

  async refresh(): Promise<ToolchainStatus | null> {
    try {
      this.set(await api.toolchain());
    } catch {
      /* the server may be restarting; the next poll will retry */
    }
    return this.status;
  }

  async install(id: CompilerId) {
    try {
      await api.installCompiler(id);
    } catch (e) {
      toast((e as Error).message, "error");
    }
    await this.refresh();
  }

  async cancel() {
    try {
      await api.cancelInstall();
    } catch (e) {
      toast((e as Error).message, "error");
    }
    await this.refresh();
  }

  async remove(id: CompilerId): Promise<boolean> {
    try {
      await api.removeCompiler(id);
    } catch (e) {
      toast((e as Error).message, "error");
      await this.refresh();
      return false;
    }
    await this.refresh();
    return true;
  }

  private set(next: ToolchainStatus) {
    this.status = next;
    for (const fn of [...this.listeners]) fn(next);
    this.schedule();
  }

  /** Poll quickly while installing, and slowly while nothing can compile so
   *  that a compiler installed from another browser is noticed. */
  private schedule() {
    window.clearTimeout(this.timer);
    if (this.listeners.size === 0) return;
    const delay = this.installing() ? 600 : this.status && !this.hasEngine() ? 5000 : 0;
    if (delay) this.timer = window.setTimeout(() => void this.refresh(), delay);
  }
}

export const toolchain = new ToolchainStore();
