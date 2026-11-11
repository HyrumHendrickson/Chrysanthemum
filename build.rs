//! Builds the web interface (web/dist) so it can be embedded in the binary.
//!
//! Requires Node.js (22.13 or newer) and npm when the interface has to be
//! built. Set CHRYSANTHEMUM_SKIP_WEB_BUILD=1 to skip building, for example when
//! web/dist was produced by another step.

use std::{fs, path::Path, process::Command, time::SystemTime};

const FALLBACK_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Chrysanthemum</title>\
<body style=\"font:16px system-ui;max-width:40rem;margin:4rem auto;padding:0 1rem\">\
<h1>Web interface not built</h1>\
<p>This binary was compiled without the web interface. Install Node.js 22.13 or newer, \
then run <code>npm ci &amp;&amp; npm run build</code> in <code>web/</code> and rebuild with \
<code>cargo build --release</code>.</p></body>";

fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let meta = fs::metadata(path).ok()?;
    let mut newest = meta.modified().ok();
    if meta.is_dir() {
        for entry in fs::read_dir(path).ok()?.flatten() {
            if let Some(t) = newest_mtime(&entry.path())
                && newest.is_none_or(|n| t > n)
            {
                newest = Some(t);
            }
        }
    }
    newest
}

fn run(npm: &str, args: &[&str]) -> bool {
    Command::new(npm)
        .args(args)
        .current_dir("web")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn main() {
    for p in [
        "web/src",
        "web/package.json",
        "web/package-lock.json",
        "web/build.mjs",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }
    println!("cargo:rerun-if-env-changed=CHRYSANTHEMUM_SKIP_WEB_BUILD");

    let dist_index = Path::new("web/dist/index.html");
    let skip = std::env::var_os("CHRYSANTHEMUM_SKIP_WEB_BUILD").is_some_and(|v| v != "0");

    if !skip {
        let built = newest_mtime(Path::new("web/dist/assets/app.js"));
        let sources = [
            "web/src",
            "web/package.json",
            "web/package-lock.json",
            "web/build.mjs",
        ]
        .iter()
        .filter_map(|p| newest_mtime(Path::new(p)))
        .max();
        let stale = match (built, sources) {
            (None, _) => true,
            (Some(b), Some(s)) => s > b,
            (Some(_), None) => false,
        };
        if stale {
            let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
            let installed = Path::new("web/node_modules").is_dir()
                || run(npm, &["ci", "--no-audit", "--no-fund"]);
            if !(installed && run(npm, &["run", "build"])) {
                println!(
                    "cargo:warning=could not build the web interface (is Node.js 22.13+ installed?); \
                     the binary will serve a placeholder page"
                );
            }
        }
    }

    if !dist_index.exists() {
        fs::create_dir_all("web/dist").expect("create web/dist");
        fs::write(dist_index, FALLBACK_PAGE).expect("write fallback page");
    }
}
