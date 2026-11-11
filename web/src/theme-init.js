// Applies the stored colour theme before first paint to avoid a flash.
// Kept as an external file so the page can run under a strict CSP.
try {
  var t = localStorage.getItem("chrysanthemum.theme");
  if (!t) t = matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  document.documentElement.dataset.theme = t;
} catch (e) {}
