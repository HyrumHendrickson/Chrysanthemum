// PDF preview built on pdf.js. Pages are laid out as placeholders and drawn
// lazily; on reload the new document is rendered off-screen first so the
// preview never flashes blank, and the scroll position is kept.

import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentLoadingTask, PDFDocumentProxy, PDFPageProxy, RenderTask } from "pdfjs-dist";

pdfjs.GlobalWorkerOptions.workerSrc = "/assets/pdf.worker.mjs";

const PAGE_GAP = 14;
const PAD = 18;
const MIN_SCALE = 0.25;
const MAX_SCALE = 5;

interface PageEntry {
  num: number;
  page: PDFPageProxy;
  w: number; // size at scale 1, in CSS px
  h: number;
  el: HTMLDivElement;
  canvas: HTMLCanvasElement | null;
  task: RenderTask | null;
  renderedScale: number;
}

export interface PdfViewerEvents {
  onPage?: (current: number, total: number) => void;
  onZoom?: (label: string) => void;
}

export class PdfViewer {
  private doc: PDFDocumentProxy | null = null;
  private docTask: PDFDocumentLoadingTask | null = null;
  private pages: PageEntry[] = [];
  private pagesEl: HTMLDivElement;
  private observer: IntersectionObserver | null = null;
  private fit = true;
  private scale = 1;
  private loadToken = 0;
  private resizeObserver: ResizeObserver;
  private resizeTimer: number | undefined;
  private lastWidth = 0;

  constructor(
    private readonly scroller: HTMLElement,
    private readonly events: PdfViewerEvents = {},
  ) {
    this.pagesEl = document.createElement("div");
    this.pagesEl.className = "pdf-pages";
    scroller.append(this.pagesEl);
    scroller.addEventListener("scroll", this.reportPage, { passive: true });
    scroller.addEventListener("wheel", this.onWheel, { passive: false });
    this.lastWidth = scroller.clientWidth;
    this.resizeObserver = new ResizeObserver(() => {
      const width = this.scroller.clientWidth;
      if (width === this.lastWidth) return;
      this.lastWidth = width;
      window.clearTimeout(this.resizeTimer);
      this.resizeTimer = window.setTimeout(() => {
        if (this.fit) this.relayout();
      }, 120);
    });
    this.resizeObserver.observe(scroller);
  }

  get pageCount(): number {
    return this.pages.length;
  }

  /** Loads (or reloads) a PDF from `url`. Resolves false if there is nothing to show. */
  async load(url: string): Promise<boolean> {
    const token = ++this.loadToken;
    const res = await fetch(url, { cache: "no-store", credentials: "same-origin" });
    if (!res.ok) return false;
    const data = new Uint8Array(await res.arrayBuffer());
    if (token !== this.loadToken) return true;

    const task = pdfjs.getDocument({
      data,
      wasmUrl: "/pdfjs/wasm/",
      standardFontDataUrl: "/pdfjs/standard_fonts/",
    });
    const doc = await task.promise;
    if (token !== this.loadToken) {
      void task.destroy();
      return true;
    }

    const loaded = await Promise.all(
      Array.from({ length: doc.numPages }, async (_, i) => {
        const page = await doc.getPage(i + 1);
        const vp = page.getViewport({ scale: 1 });
        return { page, w: vp.width, h: vp.height };
      }),
    );
    if (token !== this.loadToken) {
      void task.destroy();
      return true;
    }

    const previousTop = this.scroller.scrollTop;
    const previousHeight = this.scroller.scrollHeight;
    const next = this.buildPages(loaded);
    const scale = this.computeScale(next);

    // Draw the pages around the current scroll position before swapping them in.
    const frag = document.createElement("div");
    frag.className = "pdf-pages";
    for (const p of next) {
      this.sizePage(p, scale);
      frag.append(p.el);
    }
    const top = previousHeight > 0 && this.scale === scale ? previousTop : this.ratioTop(next, scale, previousTop, previousHeight);
    const visible = this.visibleRange(next, scale, top);
    await Promise.all(visible.map((p) => this.draw(p, scale)));
    if (token !== this.loadToken) {
      void task.destroy();
      return true;
    }

    this.teardown();
    this.doc = doc;
    this.docTask = task;
    this.pages = next;
    this.scale = scale;
    this.pagesEl.replaceWith(frag);
    this.pagesEl = frag;
    this.scroller.scrollTop = top;
    this.observe();
    this.events.onZoom?.(this.zoomLabel());
    this.reportPage();
    return true;
  }

  clear() {
    this.loadToken++;
    this.teardown();
    this.pages = [];
    this.pagesEl.replaceChildren();
    this.events.onPage?.(0, 0);
  }

  zoomIn() {
    this.setScale(this.scale * 1.15);
  }

  zoomOut() {
    this.setScale(this.scale / 1.15);
  }

  fitWidth() {
    this.fit = true;
    this.relayout();
  }

  destroy() {
    this.loadToken++;
    this.resizeObserver.disconnect();
    window.clearTimeout(this.resizeTimer);
    this.scroller.removeEventListener("scroll", this.reportPage);
    this.scroller.removeEventListener("wheel", this.onWheel);
    this.teardown();
  }

  // ---- internals ----

  private buildPages(loaded: { page: PDFPageProxy; w: number; h: number }[]): PageEntry[] {
    return loaded.map(({ page, w, h }, i) => {
      const el = document.createElement("div");
      el.className = "pdf-page";
      el.dataset.page = String(i + 1);
      return { num: i + 1, page, w, h, el, canvas: null, task: null, renderedScale: 0 };
    });
  }

  private computeScale(pages: PageEntry[]): number {
    if (!this.fit) return this.scale;
    const widest = Math.max(1, ...pages.map((p) => p.w));
    const available = Math.max(200, this.scroller.clientWidth - PAD * 2);
    return clamp(available / widest, MIN_SCALE, MAX_SCALE);
  }

  private sizePage(p: PageEntry, scale: number) {
    p.el.style.width = `${Math.floor(p.w * scale)}px`;
    p.el.style.height = `${Math.floor(p.h * scale)}px`;
  }

  private ratioTop(pages: PageEntry[], scale: number, top: number, oldHeight: number): number {
    if (oldHeight <= 0) return 0;
    const total = pages.reduce((sum, p) => sum + p.h * scale + PAGE_GAP, PAD * 2);
    return (top / oldHeight) * total;
  }

  private visibleRange(pages: PageEntry[], scale: number, top: number): PageEntry[] {
    const bottom = top + this.scroller.clientHeight;
    let y = PAD;
    const out: PageEntry[] = [];
    for (const p of pages) {
      const h = p.h * scale;
      if (y + h >= top - 200 && y <= bottom + 200) out.push(p);
      y += h + PAGE_GAP;
    }
    return out.slice(0, 6);
  }

  private async draw(p: PageEntry, scale: number) {
    if (p.renderedScale === scale && p.canvas) return;
    p.task?.cancel();
    const dpr = window.devicePixelRatio || 1;
    const viewport = p.page.getViewport({ scale: scale * dpr });
    const canvas = document.createElement("canvas");
    canvas.width = Math.floor(viewport.width);
    canvas.height = Math.floor(viewport.height);
    canvas.style.width = `${Math.floor(p.w * scale)}px`;
    canvas.style.height = `${Math.floor(p.h * scale)}px`;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const task = p.page.render({ canvasContext: ctx, canvas, viewport });
    p.task = task;
    try {
      await task.promise;
    } catch {
      return; // cancelled by a newer render
    }
    if (p.task !== task) return;
    p.task = null;
    p.canvas?.remove();
    p.el.append(canvas);
    p.canvas = canvas;
    p.renderedScale = scale;
  }

  private observe() {
    this.observer?.disconnect();
    this.observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          const p = this.pages[Number((e.target as HTMLElement).dataset.page) - 1];
          if (!p) continue;
          if (e.isIntersecting) void this.draw(p, this.scale);
        }
      },
      { root: this.scroller, rootMargin: "600px 0px" },
    );
    for (const p of this.pages) this.observer.observe(p.el);
  }

  private teardown() {
    this.observer?.disconnect();
    this.observer = null;
    for (const p of this.pages) p.task?.cancel();
    if (this.docTask) void this.docTask.destroy();
    this.docTask = null;
    this.doc = null;
  }

  private setScale(scale: number) {
    this.fit = false;
    this.scale = clamp(scale, MIN_SCALE, MAX_SCALE);
    this.relayout();
  }

  private relayout() {
    if (this.pages.length === 0) return;
    const ratio = this.scroller.scrollHeight > 0 ? this.scroller.scrollTop / this.scroller.scrollHeight : 0;
    this.scale = this.computeScale(this.pages);
    for (const p of this.pages) {
      p.task?.cancel();
      p.task = null;
      p.canvas?.remove();
      p.canvas = null;
      p.renderedScale = 0;
      this.sizePage(p, this.scale);
    }
    this.scroller.scrollTop = ratio * this.scroller.scrollHeight;
    this.observe();
    this.events.onZoom?.(this.zoomLabel());
  }

  private zoomLabel(): string {
    return this.fit ? "Fit width" : `${Math.round(this.scale * 100)}%`;
  }

  private onWheel = (e: WheelEvent) => {
    if (!e.ctrlKey && !e.metaKey) return;
    e.preventDefault();
    if (e.deltaY < 0) this.zoomIn();
    else this.zoomOut();
  };

  private reportPage = () => {
    if (this.pages.length === 0) return;
    const probe = this.scroller.scrollTop + this.scroller.clientHeight * 0.3;
    let y = PAD;
    let current = 1;
    for (const p of this.pages) {
      const h = p.h * this.scale;
      if (probe >= y) current = p.num;
      y += h + PAGE_GAP;
    }
    this.events.onPage?.(current, this.pages.length);
  };
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}
