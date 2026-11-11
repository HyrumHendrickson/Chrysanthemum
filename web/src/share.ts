import { api } from "./api";
import { copyText, h, showDialog, toast } from "./dom";
import { icons } from "./icons";

/** Dialog explaining how others on the network can join. */
export async function openShareDialog(projectId?: string) {
  let info;
  try {
    info = await api.share();
  } catch (e) {
    toast((e as Error).message, "error");
    return;
  }
  await showDialog((body, close) => {
    body.append(h("h2", {}, "Work together"));
    if (!info.lan) {
      body.append(
        h("p", {}, "Scriptorium is currently reachable only from this computer. To let others on your network edit with you, stop it and start it again with:"),
        h("pre", { class: "codeblock" }, "scriptorium --lan"),
        h("p", { class: "hint" }, "A one-time access token is generated for you and printed in the terminal together with the join links."),
      );
    } else {
      body.append(
        h("p", {}, "Send one of these links to anyone on the same network. Everyone edits the same live document, and changes are saved automatically."),
      );
      if (info.urls.length === 0) {
        body.append(h("p", { class: "hint" }, "No network address was detected. Use this computer's IP address with the port shown in the terminal."));
      }
      for (const base of info.urls) {
        const query = info.token ? `?token=${info.token}` : "";
        const link = `${base}${query}${projectId ? `#/p/${encodeURIComponent(projectId)}` : ""}`;
        const copy = h("button", { class: "icon-btn", type: "button", "aria-label": "Copy link", title: "Copy link", html: icons.copy });
        copy.addEventListener("click", async () => {
          toast((await copyText(link)) ? "Link copied" : "Could not copy the link", "info");
        });
        body.append(h("div", { class: "linkrow" }, h("code", {}, link), copy));
      }
      body.append(
        info.token
          ? h("p", { class: "hint" }, "The link contains the access token. Share it only with people you trust: they can edit files and run LaTeX on this machine.")
          : h("p", { class: "hint warn" }, "Authentication is turned off. Anyone who can reach this computer on the network can edit and compile."),
      );
    }
    body.append(h("div", { class: "dialog-actions" }, h("button", { class: "btn primary", type: "button", onclick: close }, "Done")));
  });
}
