// End-to-end check of the browser-side collaboration provider against a running
// Scriptorium server. Usage: SCRIPTORIUM_URL=http://127.0.0.1:3000 node test/e2e.mjs
//
// It drives the real DocProvider (the same code the UI uses) with two clients
// and verifies sync, presence, persistence and clean reconnection.

import { build } from "esbuild";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import assert from "node:assert/strict";

const base = process.env.SCRIPTORIUM_URL ?? "http://127.0.0.1:3000";
const token = process.env.SCRIPTORIUM_TOKEN;
const wsBase = base.replace(/^http/, "ws");
const headers = token ? { Authorization: `Bearer ${token}` } : {};

// Bundle the provider for Node, with the few browser globals it touches.
const dir = mkdtempSync(join(tmpdir(), "scriptorium-e2e-"));
const outfile = join(dir, "provider.mjs");
await build({
  entryPoints: ["src/provider.ts"],
  bundle: true,
  format: "esm",
  platform: "node",
  outfile,
  banner: {
    js: "import { createRequire } from 'node:module'; const require = createRequire(import.meta.url);\nglobalThis.window ??= { addEventListener() {}, removeEventListener() {}, setTimeout, clearTimeout };",
  },
  logLevel: "error",
});
const { DocProvider } = await import(pathToFileURL(outfile).href);

const api = async (method, path, body) => {
  const res = await fetch(base + path, {
    method,
    headers: { ...headers, ...(body && typeof body === "object" ? { "Content-Type": "application/json" } : {}) },
    body: body === undefined ? undefined : typeof body === "string" ? body : JSON.stringify(body),
  });
  return res;
};

const until = async (what, fn, ms = 6000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await fn()) return;
    await new Promise((r) => setTimeout(r, 40));
  }
  throw new Error(`timed out waiting for: ${what}`);
};

const project = `e2e-${Date.now().toString(36)}`;
const providers = [];
const open = (path) => {
  const p = new DocProvider(`${wsBase}/ws/doc/${project}`, { path }, {});
  providers.push(p);
  return p;
};
const text = (p) => p.doc.getText("content").toString();
const disk = async (path) => (await api("GET", `/api/projects/${project}/raw?path=${encodeURIComponent(path)}`)).text();
let failed = false;
const step = async (name, fn) => {
  try {
    await fn();
    console.log(`ok   ${name}`);
  } catch (e) {
    failed = true;
    console.log(`FAIL ${name}\n     ${e.stack ?? e}`);
  }
};

try {
  assert.equal((await api("POST", "/api/projects", { name: project })).status, 201);
  assert.equal((await api("PUT", `/api/projects/${project}/upload?path=notes.tex`, "hello")).status, 201);

  let a, b;
  await step("two clients load the same file", async () => {
    a = open("notes.tex");
    b = open("notes.tex");
    await until("both synced", () => a.synced && b.synced);
    assert.equal(text(a), "hello");
    assert.equal(text(b), "hello");
  });

  await step("edits propagate between clients", async () => {
    a.doc.getText("content").insert(5, " world");
    await until("B sees A's edit", () => text(b) === "hello world");
    b.doc.getText("content").insert(0, ">> ");
    await until("A sees B's edit", () => text(a) === ">> hello world");
  });

  await step("concurrent edits converge", async () => {
    a.doc.getText("content").insert(0, "A");
    b.doc.getText("content").insert(text(b).length, "B");
    await until("converged", () => text(a) === text(b) && text(a).startsWith("A>> ") && text(a).endsWith("B"));
  });

  await step("presence is shared", async () => {
    a.awareness.setLocalStateField("user", { name: "Ada", color: "#2f5da8" });
    await until("B sees Ada", () => [...b.awareness.getStates().values()].some((s) => s.user?.name === "Ada"));
    const late = open("notes.tex");
    await until("late joiner sees Ada", () => [...late.awareness.getStates().values()].some((s) => s.user?.name === "Ada"));
    late.destroy();
  });

  await step("server persists the merged text", async () => {
    const expected = text(a);
    await until("disk matches", async () => (await disk("notes.tex")) === expected);
  });

  await step("a lone client reconnects without duplicating text", async () => {
    b.destroy();
    await until("only A remains", () => a.awareness.getStates().size === 1);
    const before = text(a);
    // Drop A's socket: the server closes the room, then A reconnects to a rebuilt one.
    a.ws.close();
    await until("A disconnected", () => a.status !== "connected");
    await until("A reconnected and synced", () => a.status === "connected" && a.synced, 10000);
    await new Promise((r) => setTimeout(r, 400));
    assert.equal(text(a), before);
    assert.equal(await disk("notes.tex"), before);
  });

  await step("changes made on disk are picked up by new sessions", async () => {
    a.destroy();
    await new Promise((r) => setTimeout(r, 300));
    assert.equal((await api("PUT", `/api/projects/${project}/upload?path=notes.tex`, "rewritten")).status, 201);
    const c = open("notes.tex");
    await until("C synced", () => c.synced);
    assert.equal(text(c), "rewritten");
    c.destroy();
  });

  await step("unknown files are refused", async () => {
    const ws = new WebSocket(`${wsBase}/ws/doc/${project}?path=missing.tex`);
    const outcome = await new Promise((resolve) => {
      ws.onopen = () => resolve("open");
      ws.onerror = () => resolve("refused");
      ws.onclose = () => resolve("refused");
    });
    assert.equal(outcome, "refused");
  });
} finally {
  for (const p of providers) p.destroy();
  await api("DELETE", `/api/projects/${project}`);
}

if (failed) process.exit(1);
console.log("all end-to-end checks passed");
