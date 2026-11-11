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
};

const MAX_CAPTURE: usize = 4 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 300;

/// Which TeX tools are installed on this machine.
#[derive(Debug, Clone, Serialize)]
pub struct Toolchain {
    pub latexmk: bool,
    pub pdflatex: bool,
    pub xelatex: bool,
    pub lualatex: bool,
    pub tectonic: bool,
}

impl Toolchain {
    pub fn detect() -> Self {
        let has = |n: &str| which::which(n).is_ok();
        Self {
            latexmk: has("latexmk"),
            pdflatex: has("pdflatex"),
            xelatex: has("xelatex"),
            lualatex: has("lualatex"),
            tectonic: has("tectonic"),
        }
    }

    pub fn supports(&self, engine: Engine) -> bool {
        match engine {
            Engine::Pdflatex => self.pdflatex,
            Engine::Xelatex => self.xelatex,
            Engine::Lualatex => self.lualatex,
            Engine::Tectonic => self.tectonic,
        }
    }

    pub fn any(&self) -> bool {
        self.pdflatex || self.xelatex || self.lualatex || self.tectonic
    }
}

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
    toolchain: Toolchain,
    timeout: Duration,
    slots: std::sync::Mutex<HashMap<String, Arc<ProjectSlot>>>,
    last: std::sync::Mutex<HashMap<String, Arc<CompileResult>>>,
}

impl CompileManager {
    pub fn new(store: Store, toolchain: Toolchain, timeout: Duration) -> Self {
        Self {
            store,
            toolchain,
            timeout,
            slots: Default::default(),
            last: Default::default(),
        }
    }

    pub fn toolchain(&self) -> &Toolchain {
        &self.toolchain
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
        let id = slot.counter.fetch_add(1, Ordering::SeqCst) + 1;

        let plan = self.plan(settings.engine, &build, &settings.main)?;
        let started = Instant::now();
        let run = run_plan(&plan, &dir, self.timeout).await;
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

    fn plan(&self, engine: Engine, build: &Path, main: &str) -> AppResult<Plan> {
        let tc = &self.toolchain;
        if !tc.supports(engine) {
            return Err(AppError::bad_request(format!(
                "the '{}' engine is not installed on this machine",
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

async fn run_plan(plan: &Plan, cwd: &Path, timeout: Duration) -> Run {
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
        let step = run_command(argv, cwd, remaining).await;
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
) -> std::io::Result<(bool, bool, String)> {
    let mut cmd = Command::new(&argv[0]);
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
