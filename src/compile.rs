//! LaTeX compilation: engine discovery, process supervision and log parsing.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use regex::Regex;
use serde::Serialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::Mutex,
};

use crate::{
    error::{AppError, AppResult},
    store::{Engine, Store},
    toolchain::{Resolved, ToolchainManager},
};

const MAX_CAPTURE: usize = 4 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 300;
/// Time allowed for the first Tectonic run, which downloads support files.
const FIRST_RUN_TIMEOUT: Duration = Duration::from_secs(300);
/// Most missing packages fetched for a single compilation.
const MAX_AUTO_PACKAGES: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompileResult {
    /// Monotonic per-project compile counter; also used to bust PDF caches.
    pub id: u64,
    pub ok: bool,
    pub pdf: bool,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub engine: String,
    pub diagnostics: Vec<Diagnostic>,
    pub log: String,
}

struct ProjectSlot {
    lock: Mutex<()>,
    counter: AtomicU64,
}

/// Serialises compilations per project and remembers the latest result.
pub struct CompileManager {
    store: Store,
    toolchains: Arc<ToolchainManager>,
    timeout: Duration,
    slots: std::sync::Mutex<HashMap<String, Arc<ProjectSlot>>>,
    last: std::sync::Mutex<HashMap<String, Arc<CompileResult>>>,
}

impl CompileManager {
    pub fn new(store: Store, toolchains: Arc<ToolchainManager>, timeout: Duration) -> Self {
        Self {
            store,
            toolchains,
            timeout,
            slots: Default::default(),
            last: Default::default(),
        }
    }

    pub fn toolchains(&self) -> &Arc<ToolchainManager> {
        &self.toolchains
    }

    pub fn last_result(&self, project: &str) -> Option<Arc<CompileResult>> {
        self.last.lock().unwrap().get(project).cloned()
    }

    pub fn forget(&self, project: &str) {
        self.last.lock().unwrap().remove(project);
        self.slots.lock().unwrap().remove(project);
    }

    pub fn pdf_path(&self, project: &str) -> AppResult<PathBuf> {
        let settings = self.store.load_settings(project)?;
        let stem = Path::new(&settings.main)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("main")
            .to_string();
        Ok(self.store.build_dir(project)?.join(format!("{stem}.pdf")))
    }

    fn slot(&self, project: &str) -> Arc<ProjectSlot> {
        self.slots
            .lock()
            .unwrap()
            .entry(project.to_string())
            .or_insert_with(|| {
                Arc::new(ProjectSlot {
                    lock: Mutex::new(()),
                    counter: AtomicU64::new(0),
                })
            })
            .clone()
    }

    /// Compiles the project's main file. Concurrent calls for one project run
    /// one after another.
    pub async fn compile(&self, project: &str) -> AppResult<Arc<CompileResult>> {
        let settings = self.store.load_settings(project)?;
        let dir = self.store.project_dir(project)?;
        let main = self.store.resolve(project, &settings.main)?;
        if !main.is_file() {
            return Err(AppError::bad_request(format!(
                "main file '{}' does not exist",
                settings.main
            )));
        }
        let build = self.store.build_dir(project)?;
        tokio::fs::create_dir_all(&build)
            .await
            .map_err(AppError::from)?;

        let slot = self.slot(project);
        let _guard = slot.lock.lock().await;
        // Keeps the compiler from being removed while it is running.
        let _in_use = self.toolchains.use_guard().await;
        let resolved = self.toolchains.current();
        let id = slot.counter.fetch_add(1, Ordering::SeqCst) + 1;

        let plan = self.plan(&resolved, settings.engine, &build, &settings.main)?;
        let started = Instant::now();
        // The very first Tectonic run downloads its support files.
        let first_tectonic = matches!(settings.engine, Engine::Tectonic)
            && resolved.tectonic_cache().is_some_and(|c| !c.exists());
        let budget = if first_tectonic {
            self.timeout.max(FIRST_RUN_TIMEOUT)
        } else {
            self.timeout
        };
        let deadline = started + budget;
        let mut run = run_plan(&plan, &dir, budget, &resolved).await;

        // A missing LaTeX package is the most common reason a fresh TeX Live
        // fails. Fetch what is missing and try once more.
        let stem_for_log = Path::new(&settings.main)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("main")
            .to_string();
        if !run.success && !run.timed_out && resolved.tlmgr.is_some() {
            let current_log = tokio::fs::read(build.join(format!("{stem_for_log}.log")))
                .await
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let combined = format!("{}\n{current_log}", run.output);
            let wanted = missing_files(&combined);
            let fonts = missing_font_packages(&combined);
            if !wanted.is_empty() || !fonts.is_empty() {
                let mut notes = String::new();
                let fetched = self
                    .fetch_missing_packages(&resolved, &dir, &wanted, &fonts, deadline, &mut notes)
                    .await;
                if fetched {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let again = run_plan(&plan, &dir, remaining, &resolved).await;
                    run = Run {
                        output: format!("{notes}\n{}", again.output),
                        ..again
                    };
                } else if !notes.is_empty() {
                    run.output = format!("{notes}\n{}", run.output);
                }
            }
        }
        let duration_ms = started.elapsed().as_millis() as u64;

        let stem = Path::new(&settings.main)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("main");
        let log_file = tokio::fs::read(build.join(format!("{stem}.log")))
            .await
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let log = match (&run.output, log_file) {
            (out, Some(file)) if !out.trim().is_empty() => {
                format!("{out}\n\n----- {stem}.log -----\n{file}")
            }
            (_, Some(file)) => file,
            (out, None) => out.clone(),
        };
        let diagnostics = parse_log(&log, &settings.main);
        let pdf = tokio::fs::metadata(build.join(format!("{stem}.pdf")))
            .await
            .map(|m| m.len() > 0)
            .unwrap_or(false);
        let result = Arc::new(CompileResult {
            id,
            ok: run.success && !run.timed_out,
            pdf,
            timed_out: run.timed_out,
            duration_ms,
            engine: plan.label.clone(),
            diagnostics,
            log,
        });
        self.last
            .lock()
            .unwrap()
            .insert(project.to_string(), result.clone());
        Ok(result)
    }

    pub async fn clean(&self, project: &str) -> AppResult<()> {
        let slot = self.slot(project);
        let _guard = slot.lock.lock().await;
        let build = self.store.build_dir(project)?;
        match tokio::fs::remove_dir_all(&build).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.last.lock().unwrap().remove(project);
        Ok(())
    }

    fn plan(
        &self,
        resolved: &Resolved,
        engine: Engine,
        build: &Path,
        main: &str,
    ) -> AppResult<Plan> {
        let tc = &resolved.toolchain;
        if !tc.supports(engine) {
            return Err(AppError::bad_request(format!(
                "the '{}' engine is not installed; install a compiler from the Compilers menu",
                engine_name(engine)
            )));
        }
        let steps = match engine {
            Engine::Tectonic => vec![vec![
                "tectonic".to_string(),
                "-X".into(),
                "compile".into(),
                "--outdir".into(),
                build.display().to_string(),
                "--keep-logs".into(),
                "--synctex".into(),
                main.into(),
            ]],
            _ if tc.latexmk => {
                let name = engine_name(engine);
                let flag = match engine {
                    Engine::Pdflatex => "-pdf",
                    Engine::Xelatex => "-xelatex",
                    _ => "-lualatex",
                };
                let tex_cmd = format!(
                    "{name} -no-shell-escape -interaction=nonstopmode -file-line-error -synctex=1 %O %S"
                );
                vec![vec![
                    "latexmk".to_string(),
                    flag.into(),
                    format!("-{name}={tex_cmd}"),
                    "-f".into(),
                    format!("-outdir={}", build.display()),
                    main.into(),
                ]]
            }
            _ => {
                let name = engine_name(engine).to_string();
                let pass = vec![
                    name,
                    "-no-shell-escape".into(),
                    "-interaction=nonstopmode".into(),
                    "-file-line-error".into(),
                    "-synctex=1".into(),
                    format!("-output-directory={}", build.display()),
                    main.into(),
                ];
                vec![pass.clone(), pass]
            }
        };
        let label = if matches!(engine, Engine::Tectonic) {
            "tectonic".to_string()
        } else if tc.latexmk {
            format!("latexmk ({})", engine_name(engine))
        } else {
            engine_name(engine).to_string()
        };
        Ok(Plan { steps, label })
    }
}

impl CompileManager {
    /// Installs the TeX Live packages that provide `files`, using the managed
    /// `tlmgr`. Returns true if at least one package was installed. Failures
    /// are reported in `notes` and never abort the compilation.
    async fn fetch_missing_packages(
        &self,
        resolved: &Resolved,
        cwd: &Path,
        files: &[String],
        packages: &[String],
        deadline: Instant,
        notes: &mut String,
    ) -> bool {
        let Some(tlmgr) = resolved.tlmgr.as_ref().map(|p| p.display().to_string()) else {
            return false;
        };
        let _lock = self.toolchains.tlmgr_lock().lock().await;
        let mut installed_any = false;
        let mut done: Vec<String> = Vec::new();
        for package in packages {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let install = vec![tlmgr.clone(), "install".into(), package.clone()];
            match run_command(&install, cwd, left, resolved).await {
                Ok((true, _, _)) => {
                    notes.push_str(&format!("Installed the missing TeX package '{package}'.\n"));
                    installed_any = true;
                    done.push(package.clone());
                }
                _ => notes.push_str(&format!(
                    "Could not install the TeX package '{package}'; check your internet connection.\n"
                )),
            }
        }
        for file in files.iter().take(MAX_AUTO_PACKAGES) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let search = vec![
                tlmgr.clone(),
                "search".into(),
                "--file".into(),
                "--global".into(),
                format!("/{file}"),
            ];
            let Ok((_, _, out)) =
                run_command(&search, cwd, left.min(Duration::from_secs(90)), resolved).await
            else {
                continue;
            };
            let Some(package) = parse_tlmgr_search(&out, file) else {
                notes.push_str(&format!("No TeX Live package provides '{file}'.\n"));
                continue;
            };
            if done.contains(&package) {
                continue;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let install = vec![tlmgr.clone(), "install".into(), package.clone()];
            match run_command(&install, cwd, left, resolved).await {
                Ok((true, _, _)) => {
                    notes.push_str(&format!(
                        "Installed the missing TeX package '{package}' (needed for {file}).\n"
                    ));
                    installed_any = true;
                    done.push(package);
                }
                _ => notes.push_str(&format!(
                    "Could not install the TeX package '{package}' (needed for {file}); check your internet connection.\n"
                )),
            }
        }
        installed_any
    }
}

/// Packages that provide fonts TeX reported as missing.
///
/// A document using the T1 encoding without a font package falls back to the
/// EC bitmap fonts, which a minimal TeX Live cannot generate; `cm-super`
/// supplies scalable versions.
pub fn missing_font_packages(log: &str) -> Vec<String> {
    use std::sync::OnceLock;
    static EC_FONT: OnceLock<Regex> = OnceLock::new();
    let re = EC_FONT.get_or_init(|| {
        Regex::new(r"Font\s+(?:\S+=)?ec[a-z]+[0-9]+\s+at\s+\S+\s+not found").unwrap()
    });
    if re.is_match(log) {
        vec!["cm-super".to_string()]
    } else {
        Vec::new()
    }
}

/// Names of files that TeX reported as missing, such as `tikz.sty`.
pub fn missing_files(log: &str) -> Vec<String> {
    use std::sync::OnceLock;
    static MISSING: OnceLock<Regex> = OnceLock::new();
    let re = MISSING.get_or_init(|| {
        Regex::new(r"File [`'\u{2018}]([^'\u{2019}\s/\\]+\.[A-Za-z0-9]{1,6})['\u{2019}] not found")
            .unwrap()
    });
    let mut seen = std::collections::HashSet::new();
    re.captures_iter(log)
        .map(|c| c[1].to_string())
        .filter(|f| seen.insert(f.clone()))
        .collect()
}

/// Picks the package that provides `file` from `tlmgr search --file` output.
///
/// The output lists each matching package on a line ending in a colon, followed
/// by indented paths of the files it contains.
pub fn parse_tlmgr_search(output: &str, file: &str) -> Option<String> {
    let suffix = format!("/{file}");
    let mut package: Option<&str> = None;
    for line in output.lines() {
        if line.starts_with(char::is_whitespace) {
            let path = line.trim();
            if package.is_some() && (path.ends_with(&suffix) || path == file) {
                return package.map(str::to_string);
            }
        } else if let Some(name) = line.trim_end().strip_suffix(':')
            && !name.is_empty()
            && !name.contains(char::is_whitespace)
            && !name.starts_with("tlmgr")
        {
            package = Some(name);
        }
    }
    None
}

pub fn engine_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Pdflatex => "pdflatex",
        Engine::Xelatex => "xelatex",
        Engine::Lualatex => "lualatex",
        Engine::Tectonic => "tectonic",
    }
}

struct Plan {
    steps: Vec<Vec<String>>,
    label: String,
}

struct Run {
    success: bool,
    timed_out: bool,
    output: String,
}

async fn run_plan(plan: &Plan, cwd: &Path, timeout: Duration, resolved: &Resolved) -> Run {
    let deadline = Instant::now() + timeout;
    let mut output = String::new();
    let mut success = false;
    for (i, argv) in plan.steps.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Run {
                success: false,
                timed_out: true,
                output,
            };
        }
        let step = run_command(argv, cwd, remaining, resolved).await;
        match step {
            Ok((ok, timed_out, out)) => {
                output.push_str(&out);
                success = ok;
                if timed_out {
                    return Run {
                        success: false,
                        timed_out: true,
                        output,
                    };
                }
                // A failed first pass makes a second pass pointless.
                if !ok && i == 0 {
                    break;
                }
            }
            Err(e) => {
                output.push_str(&format!("failed to start '{}': {e}\n", argv[0]));
                return Run {
                    success: false,
                    timed_out: false,
                    output,
                };
            }
        }
    }
    Run {
        success,
        timed_out: false,
        output,
    }
}

async fn run_command(
    argv: &[String],
    cwd: &Path,
    timeout: Duration,
    resolved: &Resolved,
) -> std::io::Result<(bool, bool, String)> {
    let mut cmd = Command::new(resolved.program(&argv[0]));
    // Tools such as latexmk start other tools by name, so they must see the
    // same search path, with Chrysanthemum's own compilers first.
    cmd.env("PATH", resolved.path_env());
    if let Some(cache) = resolved.tectonic_cache() {
        cmd.env("TECTONIC_CACHE_DIR", cache);
    }
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("max_print_line", "10000")
        .env("error_line", "254")
        .env("half_error_line", "238")
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn()?;
    let pid = child.id();
    let out_task = tokio::spawn(read_capped(child.stdout.take()));
    let err_task = tokio::spawn(read_capped(child.stderr.take()));

    let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => (Some(status?), false),
        Err(_) => {
            kill_tree(pid, &mut child).await;
            (None, true)
        }
    };
    let mut output = out_task.await.unwrap_or_default();
    output.push_str(&err_task.await.unwrap_or_default());
    if timed_out {
        output.push_str(&format!(
            "\nCompilation stopped after {} seconds (timeout).\n",
            timeout.as_secs()
        ));
    }
    Ok((status.is_some_and(|s| s.success()), timed_out, output))
}

async fn kill_tree(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // The child leads its own process group; signal the whole group so
        // that engines spawned by latexmk are terminated too.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.kill().await;
}

async fn read_capped<R: AsyncRead + Unpin>(reader: Option<R>) -> String {
    let Some(mut reader) = reader else {
        return String::new();
    };
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buf.len() < MAX_CAPTURE {
                    let take = n.min(MAX_CAPTURE - buf.len());
                    buf.extend_from_slice(&chunk[..take]);
                }
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Extracts errors and warnings from TeX output.
pub fn parse_log(log: &str, main: &str) -> Vec<Diagnostic> {
    use std::sync::OnceLock;
    static FILE_LINE: OnceLock<Regex> = OnceLock::new();
    static BANG_LINE: OnceLock<Regex> = OnceLock::new();
    static ON_LINE: OnceLock<Regex> = OnceLock::new();
    static AT_LINES: OnceLock<Regex> = OnceLock::new();
    let file_line = FILE_LINE.get_or_init(|| {
        Regex::new(r"^(?:\./)?([^\s:][^:]*\.(?:tex|sty|cls|bib|cfg|def|ltx|dtx)):(\d+): (.+)$")
            .unwrap()
    });
    let bang_line = BANG_LINE.get_or_init(|| Regex::new(r"^l\.(\d+)").unwrap());
    let on_line = ON_LINE.get_or_init(|| Regex::new(r"on input line (\d+)").unwrap());
    let at_lines = AT_LINES.get_or_init(|| Regex::new(r"lines? (\d+)").unwrap());

    let lines: Vec<&str> = log.lines().collect();
    let mut out: Vec<Diagnostic> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |d: Diagnostic, out: &mut Vec<Diagnostic>| {
        if out.len() < MAX_DIAGNOSTICS
            && seen.insert((d.severity as u8, d.file.clone(), d.line, d.message.clone()))
        {
            out.push(d);
        }
    };

    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end();
        if let Some(c) = file_line.captures(line) {
            push(
                Diagnostic {
                    severity: Severity::Error,
                    file: Some(c[1].to_string()),
                    line: c[2].parse().ok(),
                    message: c[3].trim().to_string(),
                },
                &mut out,
            );
        } else if let Some(msg) = line.strip_prefix("! ") {
            // Only needed when -file-line-error output is unavailable; skip if the
            // preceding line already produced a file:line diagnostic for this error.
            let already = i > 0 && file_line.is_match(lines[i - 1].trim_end());
            if already || msg.starts_with("==> Fatal error") {
                continue;
            }
            let ln = lines
                .iter()
                .skip(i + 1)
                .take(8)
                .find_map(|l| bang_line.captures(l).and_then(|c| c[1].parse().ok()));
            push(
                Diagnostic {
                    severity: Severity::Error,
                    file: Some(main.to_string()),
                    line: ln,
                    message: msg.trim().to_string(),
                },
                &mut out,
            );
        } else if line.starts_with("LaTeX Warning:")
            || line.starts_with("LaTeX Font Warning:")
            || (line.starts_with("Package ") && line.contains(" Warning:"))
            || line.starts_with("Class ") && line.contains(" Warning:")
        {
            let ln = on_line.captures(line).and_then(|c| c[1].parse().ok());
            let message = line
                .split_once("Warning:")
                .map(|x| x.1)
                .unwrap_or(line)
                .trim()
                .to_string();
            // Shell escape is switched off on purpose, so this is not a problem.
            if message.starts_with("Shell escape feature is not enabled") {
                continue;
            }
            push(
                Diagnostic {
                    severity: Severity::Warning,
                    file: ln.map(|_| main.to_string()),
                    line: ln,
                    message,
                },
                &mut out,
            );
        } else if line.starts_with("Overfull \\hbox")
            || line.starts_with("Overfull \\vbox")
            || line.starts_with("Underfull \\hbox")
            || line.starts_with("Underfull \\vbox")
        {
            let ln = at_lines.captures(line).and_then(|c| c[1].parse().ok());
            push(
                Diagnostic {
                    severity: Severity::Warning,
                    file: ln.map(|_| main.to_string()),
                    line: ln,
                    message: line.to_string(),
                },
                &mut out,
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_file_line_errors() {
        let log = "This is pdfTeX\n./main.tex:12: Undefined control sequence.\nl.12 \\foo\n\n./chap/a.tex:3: Missing $ inserted.\n";
        let d = parse_log(log, "main.tex");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].file.as_deref(), Some("main.tex"));
        assert_eq!(d[0].line, Some(12));
        assert_eq!(d[0].message, "Undefined control sequence.");
        assert_eq!(d[1].file.as_deref(), Some("chap/a.tex"));
        assert_eq!(d[1].line, Some(3));
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn parses_bang_errors_without_file_line() {
        let log = "! Undefined control sequence.\nl.7 \\oops\n";
        let d = parse_log(log, "main.tex");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].line, Some(7));
    }

    #[test]
    fn bang_after_file_line_is_not_duplicated() {
        let log =
            "./main.tex:5: Undefined control sequence.\n! Undefined control sequence.\nl.5 \\x\n";
        assert_eq!(parse_log(log, "main.tex").len(), 1);
    }

    #[test]
    fn finds_missing_files() {
        let log = "! LaTeX Error: File `tikz.sty' not found.\n\nType X to quit\n! LaTeX Error: File `tikz.sty' not found.\n! LaTeX Error: File `algorithm2e.sty' not found.\n! LaTeX Error: File \u{2018}siunitx.sty\u{2019} not found.\n";
        assert_eq!(
            missing_files(log),
            ["tikz.sty", "algorithm2e.sty", "siunitx.sty"]
        );
        assert!(missing_files("no problems here").is_empty());
        // Paths are not package files.
        assert!(missing_files("File `../x/evil.sty' not found").is_empty());
    }

    #[test]
    fn ignores_the_expected_shell_escape_notice() {
        let log = "Package pdftexcmds Warning: Shell escape feature is not enabled.\nLaTeX Warning: Reference `a' on page 1 undefined on input line 4.\n";
        let d = parse_log(log, "main.tex");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].line, Some(4));
    }

    #[test]
    fn recognises_missing_ec_fonts() {
        let log = "!pdfTeX error: pdflatex (file ecbx1095): Font ecbx1095 at 600 not found\n";
        assert_eq!(missing_font_packages(log), ["cm-super"]);
        let log = "LaTeX Font Warning: Font T1/cmr/m/n/10=ecrm1000 at 10.0pt not found\n";
        assert_eq!(missing_font_packages(log), ["cm-super"]);
        assert!(missing_font_packages("Font OT1/cmr/m/n/10 loaded fine").is_empty());
        assert!(missing_font_packages("File `tikz.sty' not found.").is_empty());
    }

    #[test]
    fn reads_tlmgr_search_output() {
        let out = "tlmgr: package repository https://mirror.ctan.org/systems/texlive/tlnet (verified)\npgf:\n\ttexmf-dist/tex/latex/pgf/frontendlayer/tikz.sty\npgf-blur:\n\ttexmf-dist/tex/latex/pgf-blur/other.sty\n";
        assert_eq!(parse_tlmgr_search(out, "tikz.sty").as_deref(), Some("pgf"));
        assert_eq!(
            parse_tlmgr_search(out, "other.sty").as_deref(),
            Some("pgf-blur")
        );
        assert_eq!(parse_tlmgr_search(out, "missing.sty"), None);
        // The repository banner is not a package.
        assert_eq!(
            parse_tlmgr_search("tlmgr: hello:\n\tx/tikz.sty\n", "tikz.sty"),
            None
        );
    }

    #[test]
    fn parses_warnings() {
        let log = "LaTeX Warning: Reference `fig:a' on page 1 undefined on input line 20.\nOverfull \\hbox (3.0pt too wide) in paragraph at lines 30--31\nPackage hyperref Warning: Token not allowed in a PDF string.\n";
        let d = parse_log(log, "main.tex");
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].line, Some(20));
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[1].line, Some(30));
        assert_eq!(d[2].line, None);
    }
}
