import { Home } from "./home";
import { toolchain } from "./toolchain";
import { Workspace } from "./workspace";

const app = document.getElementById("app") as HTMLElement;
let view: { destroy?: () => void } | null = null;

function route() {
  view?.destroy?.();
  view = null;
  const match = /^#\/p\/([^/]+)$/.exec(location.hash);
  if (match?.[1]) {
    document.title = `${decodeURIComponent(match[1])} · Chrysanthemum`;
    view = new Workspace(app, decodeURIComponent(match[1]));
  } else {
    document.title = "Chrysanthemum";
    view = new Home(app);
  }
}

async function boot() {
  await toolchain.refresh();
  window.addEventListener("hashchange", route);
  route();
}

void boot();
