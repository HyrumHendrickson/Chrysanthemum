import { type Health, api } from "./api";
import { Home } from "./home";
import { Workspace } from "./workspace";

const app = document.getElementById("app") as HTMLElement;
let view: { destroy?: () => void } | null = null;
let health: Health | null = null;

function route() {
  view?.destroy?.();
  view = null;
  const match = /^#\/p\/([^/]+)$/.exec(location.hash);
  if (match?.[1]) {
    document.title = `${decodeURIComponent(match[1])} · Scriptorium`;
    view = new Workspace(app, decodeURIComponent(match[1]), health);
  } else {
    document.title = "Scriptorium";
    view = new Home(app, health);
  }
}

async function boot() {
  try {
    health = await api.health();
  } catch {
    health = null;
  }
  window.addEventListener("hashchange", route);
  route();
}

void boot();
