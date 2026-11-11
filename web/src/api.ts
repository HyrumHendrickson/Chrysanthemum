export type Engine = "pdflatex" | "xelatex" | "lualatex" | "tectonic";

export interface ProjectInfo {
  id: string;
  modified: number;
}

export interface Entry {
  path: string;
  kind: "file" | "dir";
  size: number;
}

export interface Settings {
  main: string;
  engine: Engine;
}

export interface Diagnostic {
  severity: "error" | "warning";
  file: string | null;
  line: number | null;
  message: string;
}

export interface CompileResult {
  id: number;
  ok: boolean;
  pdf: boolean;
  timed_out: boolean;
  duration_ms: number;
  engine: string;
  diagnostics: Diagnostic[];
  log: string;
}

export interface Toolchain {
  latexmk: boolean;
  pdflatex: boolean;
  xelatex: boolean;
  lualatex: boolean;
  tectonic: boolean;
}

export interface Health {
  version: string;
  toolchain: Toolchain;
  toolchain_ok: boolean;
}

export interface ShareInfo {
  lan: boolean;
  urls: string[];
  token: string | null;
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

async function request<T>(method: string, url: string, body?: unknown, raw?: BodyInit): Promise<T> {
  const init: RequestInit = { method, credentials: "same-origin" };
  if (raw !== undefined) {
    init.body = raw;
  } else if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  let res: Response;
  try {
    res = await fetch(url, init);
  } catch {
    throw new ApiError("Cannot reach the Scriptorium server. Is it still running?", 0);
  }
  if (!res.ok) {
    let message = `${res.status} ${res.statusText}`;
    try {
      const data = (await res.json()) as { error?: string };
      if (data.error) message = data.error;
    } catch {
      /* body was not JSON */
    }
    throw new ApiError(message, res.status);
  }
  if (res.status === 204) return undefined as T;
  const type = res.headers.get("content-type") ?? "";
  return (type.includes("json") ? res.json() : undefined) as Promise<T>;
}

const p = (id: string) => `/api/projects/${encodeURIComponent(id)}`;
const q = (path: string) => `path=${encodeURIComponent(path)}`;

export const api = {
  health: () => request<Health>("GET", "/api/health"),
  share: () => request<ShareInfo>("GET", "/api/share"),
  projects: () => request<ProjectInfo[]>("GET", "/api/projects"),
  createProject: (name: string) => request<ProjectInfo>("POST", "/api/projects", { name }),
  deleteProject: (id: string) => request<void>("DELETE", p(id)),
  files: (id: string) => request<Entry[]>("GET", `${p(id)}/files`),
  createEntry: (id: string, path: string, kind: "file" | "dir") =>
    request<void>("POST", `${p(id)}/files`, { path, kind }),
  deleteEntry: (id: string, path: string) => request<void>("DELETE", `${p(id)}/files?${q(path)}`),
  rename: (id: string, from: string, to: string) =>
    request<void>("POST", `${p(id)}/files/rename`, { from, to }),
  upload: (id: string, path: string, data: Blob) =>
    request<void>("PUT", `${p(id)}/upload?${q(path)}`, undefined, data),
  settings: (id: string) => request<Settings>("GET", `${p(id)}/settings`),
  saveSettings: (id: string, s: Settings) => request<Settings>("PUT", `${p(id)}/settings`, s),
  compile: (id: string) => request<CompileResult>("POST", `${p(id)}/compile`),
  lastCompile: (id: string) => request<CompileResult | null>("GET", `${p(id)}/compile`),
  clean: (id: string) => request<void>("POST", `${p(id)}/clean`),
  pdfUrl: (id: string, v?: number) => `${p(id)}/pdf${v ? `?v=${v}` : ""}`,
  pdfDownloadUrl: (id: string) => `${p(id)}/pdf?download=true`,
  zipUrl: (id: string) => `${p(id)}/download.zip`,
  rawUrl: (id: string, path: string) => `${p(id)}/raw?${q(path)}`,
  rawText: async (id: string, path: string) => {
    const res = await fetch(`${p(id)}/raw?${q(path)}`, { credentials: "same-origin" });
    if (!res.ok) throw new ApiError(res.statusText, res.status);
    return res.text();
  },
  wsDocUrl: (id: string) => `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws/doc/${encodeURIComponent(id)}`,
  wsEventsUrl: (id: string) => `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws/events/${encodeURIComponent(id)}`,
};
