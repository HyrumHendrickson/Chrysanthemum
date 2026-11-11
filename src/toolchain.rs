//! LaTeX compilers: detection, installation and removal.
//!
//! Chrysanthemum uses whatever TeX engines are on the machine, and it can also
//! install two self-contained distributions into its own data directory
//! without administrator rights:
//!
//! * **TeX Live** (the TinyTeX distribution): pdfLaTeX, XeLaTeX, LuaLaTeX and
//!   latexmk, installed from a pre-built archive.
//! * **Tectonic**: a small, self-contained XeTeX-based engine.
//!
//! Everything lives under `<data dir>/.toolchains/<id>`, so removing a compiler
//! is a single directory removal and leaves nothing behind. Downloads come from
//! fixed, version-pinned URLs and are checked against SHA-256 digests compiled
//! into the program before a single byte is unpacked.

use std::{
    borrow::Cow,
    ffi::{OsStr, OsString},
    fs,
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

/// Name of the directory (inside the data directory) that holds compilers.
pub const MANAGED_DIR: &str = ".toolchains";

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// A compiler that Chrysanthemum knows how to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompilerId {
    Texlive,
    Tectonic,
}

impl CompilerId {
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Texlive => "texlive",
            Self::Tectonic => "tectonic",
        }
    }

    /// The program whose presence marks a working installation.
    fn main_program(self) -> &'static str {
        match self {
            Self::Texlive => "pdflatex",
            Self::Tectonic => "tectonic",
        }
    }
}

/// Operating system and CPU combinations that have downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    LinuxX64,
    LinuxMuslX64,
    LinuxArm64,
    MacArm64,
    MacX64,
    WindowsX64,
}

impl Platform {
    pub fn current() -> Option<Self> {
        if cfg!(all(
            target_os = "linux",
            target_arch = "x86_64",
            target_env = "musl"
        )) {
            Some(Self::LinuxMuslX64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Self::LinuxX64)
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(Self::LinuxArm64)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::MacArm64)
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Some(Self::MacX64)
        } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some(Self::WindowsX64)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LinuxX64 => "Linux (x86-64)",
            Self::LinuxMuslX64 => "Linux (x86-64, musl)",
            Self::LinuxArm64 => "Linux (ARM64)",
            Self::MacArm64 => "macOS (Apple silicon)",
            Self::MacX64 => "macOS (Intel)",
            Self::WindowsX64 => "Windows (x86-64)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archive {
    TarGz,
    TarXz,
    Zip,
}

/// One downloadable archive.
#[derive(Debug, Clone)]
pub struct Asset {
    pub url: Cow<'static, str>,
    /// Lower-case hex SHA-256 of the archive.
    pub sha256: Cow<'static, str>,
    pub archive: Archive,
    pub download_bytes: u64,
    /// Unpacked size; shown to the user, never relied upon.
    pub installed_bytes: u64,
    /// The archive holds one top-level directory whose contents are the install.
    pub single_root: bool,
    /// Subdirectory of the install directory that receives the files.
    pub into: &'static str,
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub id: CompilerId,
    pub name: &'static str,
    pub version: &'static str,
    pub summary: &'static str,
    pub engines: &'static [&'static str],
    pub notes: &'static str,
    pub assets: Vec<(Platform, Asset)>,
}

impl Spec {
    fn asset_for(&self, platform: Platform) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|(p, _)| *p == platform)
            .map(|(_, a)| a)
    }
}

const TINYTEX: &str = "https://github.com/rstudio/tinytex-releases/releases/download/v2026.11/";
const TECTONIC: &str =
    "https://github.com/tectonic-typesetting/tectonic/releases/download/tectonic%400.17.0/";

fn tinytex(file: &str, sha256: &'static str, archive: Archive, dl: u64, installed: u64) -> Asset {
    Asset {
        url: Cow::Owned(format!("{TINYTEX}{file}")),
        sha256: Cow::Borrowed(sha256),
        archive,
        download_bytes: dl,
        installed_bytes: installed,
        single_root: true,
        into: "",
    }
}

fn tectonic(file: &str, sha256: &'static str, archive: Archive, dl: u64, installed: u64) -> Asset {
    Asset {
        url: Cow::Owned(format!("{TECTONIC}{file}")),
        sha256: Cow::Borrowed(sha256),
        archive,
        download_bytes: dl,
        installed_bytes: installed,
        single_root: false,
        into: "bin",
    }
}

/// The compilers that can be installed, with pinned versions and digests.
///
/// To update a version, change the URLs and digests together. Release assets
/// are immutable, so a digest never needs to change for a given URL.
pub fn catalog() -> Vec<Spec> {
    use Archive::{TarGz, TarXz, Zip};
    use Platform::*;
    vec![
        Spec {
            id: CompilerId::Texlive,
            name: "TeX Live",
            version: "2026 (TinyTeX v2026.11)",
            summary: "The complete traditional toolchain: pdfLaTeX, XeLaTeX, LuaLaTeX and latexmk. Closest to what Overleaf runs, and it works offline.",
            engines: &["pdflatex", "xelatex", "lualatex", "latexmk"],
            notes: "Missing LaTeX packages are fetched automatically the first time a document needs them.",
            assets: vec![
                (
                    LinuxX64,
                    tinytex(
                        "TinyTeX-1-linux-x86_64-v2026.11.tar.xz",
                        "5b75befa848deae702ae6fa12ff0eef3f110d7e4e2a979ae99f101eba3c9c70d",
                        TarXz,
                        53_616_060,
                        177_108_573,
                    ),
                ),
                (
                    LinuxMuslX64,
                    tinytex(
                        "TinyTeX-1-linuxmusl-x86_64-v2026.11.tar.xz",
                        "4481aa5aeccebe37dc4270d12703ccfd75ac01e7490547be082b1c592092f9c2",
                        TarXz,
                        55_124_708,
                        180_521_461,
                    ),
                ),
                (
                    LinuxArm64,
                    tinytex(
                        "TinyTeX-1-linux-arm64-v2026.11.tar.xz",
                        "b351c365e4085a179c8d76fddadff612d4e4fd16a946953fb8b9d331f4f7344a",
                        TarXz,
                        53_792_928,
                        180_035_226,
                    ),
                ),
                (
                    MacArm64,
                    tinytex(
                        "TinyTeX-1-darwin-v2026.11.tar.xz",
                        "9416a257c4caea2f82351fa2f89252b8bd75dceeb0ce6e442d25530b1922a02d",
                        TarXz,
                        67_211_812,
                        229_303_797,
                    ),
                ),
                (
                    MacX64,
                    tinytex(
                        "TinyTeX-1-darwin-v2026.11.tar.xz",
                        "9416a257c4caea2f82351fa2f89252b8bd75dceeb0ce6e442d25530b1922a02d",
                        TarXz,
                        67_211_812,
                        229_303_797,
                    ),
                ),
                (
                    WindowsX64,
                    tinytex(
                        "TinyTeX-v2026.11.zip",
                        "a13d14ba987873d1bdf81773f5bdfcc34537882b825dd2134f61eb6ee21d5f69",
                        Zip,
                        247_432_160,
                        537_050_830,
                    ),
                ),
            ],
        },
        Spec {
            id: CompilerId::Tectonic,
            name: "Tectonic",
            version: "0.17.0",
            summary: "A small, modern, self-contained engine. Quick to install and fast, but it is XeTeX-based, so a few pdfLaTeX-specific packages may not work.",
            engines: &["tectonic"],
            notes: "Downloads the packages a document needs the first time it is compiled, so the first compile needs an internet connection.",
            assets: vec![
                (
                    LinuxX64,
                    tectonic(
                        "tectonic-0.17.0-x86_64-unknown-linux-musl.tar.gz",
                        "8533d07f9ccbd7a65824b9e0459041bca34af1eb33daba48f59215593753a3b7",
                        TarGz,
                        10_151_914,
                        26_401_904,
                    ),
                ),
                (
                    LinuxMuslX64,
                    tectonic(
                        "tectonic-0.17.0-x86_64-unknown-linux-musl.tar.gz",
                        "8533d07f9ccbd7a65824b9e0459041bca34af1eb33daba48f59215593753a3b7",
                        TarGz,
                        10_151_914,
                        26_401_904,
                    ),
                ),
                (
                    LinuxArm64,
                    tectonic(
                        "tectonic-0.17.0-aarch64-unknown-linux-musl.tar.gz",
                        "b10954a95404f3ab2328d2fa59a5ebab8e657f893fab096f98be8db7c0c979b8",
                        TarGz,
                        9_926_834,
                        25_896_944,
                    ),
                ),
                (
                    MacArm64,
                    tectonic(
                        "tectonic-0.17.0-aarch64-apple-darwin.tar.gz",
                        "a3f1cac7c5678f01661a92212f58480ae3b0634115d880dbc59e2953ded45667",
                        TarGz,
                        21_704_674,
                        53_998_928,
                    ),
                ),
                (
                    MacX64,
                    tectonic(
                        "tectonic-0.17.0-x86_64-apple-darwin.tar.gz",
                        "7c90ef5b6ddb1eb1937e4337add5237b79338e4b9676459fa91187d24d6cdf80",
                        TarGz,
                        21_790_177,
                        55_170_056,
                    ),
                ),
                (
                    WindowsX64,
                    tectonic(
                        "tectonic-0.17.0-x86_64-pc-windows-msvc.zip",
                        "f61ce51f0b0ade1015b7de7ef368541c5424e9756ecbd0d7af97d6d48030845f",
                        Zip,
                        21_060_223,
                        51_538_432,
                    ),
                ),
            ],
        },
    ]
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Which TeX tools can be run right now.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct Toolchain {
    pub latexmk: bool,
    pub pdflatex: bool,
    pub xelatex: bool,
    pub lualatex: bool,
    pub tectonic: bool,
}

impl Toolchain {
    fn probe(search_path: &OsStr, cwd: &Path) -> Self {
        let has = |name: &str| which::which_in(name, Some(search_path), cwd).is_ok();
        Self {
            // latexmk is a Perl script on Unix. Without Perl it cannot run, and
            // compiling falls back to calling the engine directly.
            latexmk: has("latexmk") && (cfg!(windows) || has("perl")),
            pdflatex: has("pdflatex"),
            xelatex: has("xelatex"),
            lualatex: has("lualatex"),
            tectonic: has("tectonic"),
        }
    }

    pub fn supports(&self, engine: crate::store::Engine) -> bool {
        use crate::store::Engine;
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

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// The directory holding a managed compiler's programs, if it is installed.
fn bin_dir_in(install_dir: &Path, id: CompilerId) -> Option<PathBuf> {
    let main = exe(id.main_program());
    match id {
        CompilerId::Tectonic => {
            let dir = install_dir.join("bin");
            dir.join(&main).is_file().then_some(dir)
        }
        CompilerId::Texlive => {
            // TeX Live keeps its programs in bin/<platform>/.
            let mut dirs: Vec<PathBuf> = fs::read_dir(install_dir.join("bin"))
                .ok()?
                .flatten()
                .map(|e| e.path())
                .collect();
            dirs.sort();
            dirs.into_iter().find(|d| d.join(&main).is_file())
        }
    }
}

/// A snapshot of where TeX programs are found. Cheap to clone and share.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Everything that can run, managed or not.
    pub toolchain: Toolchain,
    /// Only what was found outside Chrysanthemum's own directory.
    pub system: Toolchain,
    /// The managed `tlmgr`, used to fetch missing LaTeX packages.
    pub tlmgr: Option<PathBuf>,
    search_path: OsString,
    cwd: PathBuf,
    tectonic_cache: Option<PathBuf>,
}

impl Resolved {
    fn detect(root: &Path) -> Self {
        let cwd = std::env::current_dir().unwrap_or_default();
        let system_path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut tlmgr = None;
        let mut tectonic_cache = None;
        for id in CompilerId::ALL {
            let install = root.join(id.dir_name());
            let Some(bin) = bin_dir_in(&install, id) else {
                continue;
            };
            if id == CompilerId::Texlive {
                let name = if cfg!(windows) { "tlmgr.bat" } else { "tlmgr" };
                tlmgr = Some(bin.join(name)).filter(|p| p.is_file());
            } else {
                tectonic_cache = Some(install.join("cache"));
            }
            dirs.push(bin);
        }
        // Managed compilers come first: installing one is a deliberate choice.
        dirs.extend(std::env::split_paths(&system_path));
        let search_path = std::env::join_paths(dirs).unwrap_or(system_path.clone());
        Self {
            toolchain: Toolchain::probe(&search_path, &cwd),
            system: Toolchain::probe(&system_path, &cwd),
            tlmgr,
            search_path,
            cwd,
            tectonic_cache,
        }
    }

    /// Full path of a program, searching managed compilers first.
    pub fn program(&self, name: &str) -> PathBuf {
        which::which_in(name, Some(&self.search_path), &self.cwd)
            .unwrap_or_else(|_| PathBuf::from(name))
    }

    /// The `PATH` that child processes must see so that tools find each other.
    pub fn path_env(&self) -> &OsStr {
        &self.search_path
    }

    /// Where a managed Tectonic keeps its downloaded packages.
    pub fn tectonic_cache(&self) -> Option<&Path> {
        self.tectonic_cache.as_deref()
    }
}

impl CompilerId {
    pub const ALL: [CompilerId; 2] = [Self::Texlive, Self::Tectonic];
}

// ---------------------------------------------------------------------------
// Status reported to the UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Downloading,
    Verifying,
    Unpacking,
    Testing,
    Done,
    Failed,
    Cancelled,
}

impl Phase {
    pub fn finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobStatus {
    pub id: CompilerId,
    pub phase: Phase,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
}

struct Job {
    id: CompilerId,
    state: Mutex<(Phase, Option<String>)>,
    downloaded: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

impl Job {
    fn new(id: CompilerId, total: u64) -> Self {
        Self {
            id,
            state: Mutex::new((Phase::Downloading, None)),
            downloaded: AtomicU64::new(0),
            total: AtomicU64::new(total),
            cancel: AtomicBool::new(false),
        }
    }

    fn set_phase(&self, phase: Phase) {
        self.state.lock().unwrap().0 = phase;
    }

    fn fail(&self, error: String) {
        *self.state.lock().unwrap() = (Phase::Failed, Some(error));
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn status(&self) -> JobStatus {
        let (phase, error) = self.state.lock().unwrap().clone();
        JobStatus {
            id: self.id,
            phase,
            downloaded: self.downloaded.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            error,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CompilerInfo {
    pub id: CompilerId,
    pub name: &'static str,
    pub version: &'static str,
    pub summary: &'static str,
    pub notes: &'static str,
    pub engines: &'static [&'static str],
    /// False when no download exists for this computer.
    pub available: bool,
    /// Installed by Chrysanthemum (and therefore removable by it).
    pub installed: bool,
    pub recommended: bool,
    pub download_bytes: Option<u64>,
    pub installed_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolchainStatus {
    pub platform: Option<&'static str>,
    /// Whether the requester may install or remove compilers.
    pub can_manage: bool,
    pub compilers: Vec<CompilerInfo>,
    /// Engines that can run right now, from any source.
    pub active: Toolchain,
    /// Engines that were already on this computer before Chrysanthemum.
    pub system: Toolchain,
    pub job: Option<JobStatus>,
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// Marks an installation stopped by the user.
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub struct ToolchainManager {
    root: PathBuf,
    catalog: Vec<Spec>,
    platform: Option<Platform>,
    allow_http: bool,
    resolved: RwLock<Arc<Resolved>>,
    job: Mutex<Option<Arc<Job>>>,
    /// Compilations hold a read guard; removing a compiler needs the write guard.
    usage: Arc<tokio::sync::RwLock<()>>,
    /// Serialises `tlmgr` runs, which cannot safely overlap.
    tlmgr_lock: tokio::sync::Mutex<()>,
}

impl ToolchainManager {
    pub fn new(data_dir: &Path) -> Arc<Self> {
        Self::with_catalog(
            data_dir.join(MANAGED_DIR),
            catalog(),
            Platform::current(),
            false,
        )
    }

    /// Builds a manager with a custom catalog. Intended for tests.
    #[doc(hidden)]
    pub fn with_catalog(
        root: PathBuf,
        catalog: Vec<Spec>,
        platform: Option<Platform>,
        allow_http: bool,
    ) -> Arc<Self> {
        if let Err(e) = fs::create_dir_all(&root) {
            tracing::warn!("cannot create {}: {e}", root.display());
        }
        remove_leftovers(&root);
        let resolved = Arc::new(Resolved::detect(&root));
        Arc::new(Self {
            root,
            catalog,
            platform,
            allow_http,
            resolved: RwLock::new(resolved),
            job: Mutex::new(None),
            usage: Arc::new(tokio::sync::RwLock::new(())),
            tlmgr_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The current view of installed compilers.
    pub fn current(&self) -> Arc<Resolved> {
        self.resolved.read().unwrap().clone()
    }

    /// Re-scans for compilers; call after anything changes on disk.
    pub fn refresh(&self) {
        *self.resolved.write().unwrap() = Arc::new(Resolved::detect(&self.root));
    }

    /// Held for the duration of a compilation so a compiler cannot be removed
    /// underneath it.
    pub async fn use_guard(&self) -> tokio::sync::OwnedRwLockReadGuard<()> {
        self.usage.clone().read_owned().await
    }

    pub fn tlmgr_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.tlmgr_lock
    }

    fn is_installed(&self, id: CompilerId) -> bool {
        bin_dir_in(&self.root.join(id.dir_name()), id).is_some()
    }

    fn running_job(&self) -> bool {
        self.job
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|j| !j.status().phase.finished())
    }

    pub fn status(&self, can_manage: bool) -> ToolchainStatus {
        let resolved = self.current();
        let usable = |spec: &Spec| self.platform.and_then(|p| spec.asset_for(p)).is_some();
        // Prefer the full TeX Live; fall back to Tectonic where it has no build.
        let recommended = self.catalog.iter().find(|s| usable(s)).map(|s| s.id);
        let compilers = self
            .catalog
            .iter()
            .map(|spec| {
                let asset = self.platform.and_then(|p| spec.asset_for(p));
                CompilerInfo {
                    id: spec.id,
                    name: spec.name,
                    version: spec.version,
                    summary: spec.summary,
                    notes: spec.notes,
                    engines: spec.engines,
                    available: asset.is_some(),
                    installed: self.is_installed(spec.id),
                    recommended: recommended == Some(spec.id),
                    download_bytes: asset.map(|a| a.download_bytes),
                    installed_bytes: asset.map(|a| a.installed_bytes),
                }
            })
            .collect();
        ToolchainStatus {
            platform: self.platform.map(Platform::label),
            can_manage,
            compilers,
            active: resolved.toolchain.clone(),
            system: resolved.system.clone(),
            job: self.job.lock().unwrap().as_ref().map(|j| j.status()),
        }
    }

    /// Starts installing a compiler in the background.
    pub fn install(self: &Arc<Self>, id: CompilerId) -> AppResult<JobStatus> {
        let spec = self
            .catalog
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| AppError::not_found("unknown compiler"))?;
        let Some(platform) = self.platform else {
            return Err(AppError::bad_request(
                "no compiler download is available for this operating system",
            ));
        };
        let asset = spec.asset_for(platform).cloned().ok_or_else(|| {
            AppError::bad_request(format!("{} is not available for this computer", spec.name))
        })?;
        if self.is_installed(id) {
            return Err(AppError::conflict(format!(
                "{} is already installed",
                spec.name
            )));
        }
        let job = {
            let mut slot = self.job.lock().unwrap();
            if slot.as_ref().is_some_and(|j| !j.status().phase.finished()) {
                return Err(AppError::conflict(
                    "another installation is already in progress",
                ));
            }
            let job = Arc::new(Job::new(id, asset.download_bytes));
            *slot = Some(job.clone());
            job
        };
        let status = job.status();
        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            let outcome = me.run_install(&job, id, &asset);
            match outcome {
                Ok(()) => {
                    me.refresh();
                    job.set_phase(Phase::Done);
                    tracing::info!("installed {}", id.dir_name());
                }
                Err(_) if job.cancelled() => job.set_phase(Phase::Cancelled),
                Err(e) => {
                    tracing::warn!("installing {} failed: {e:#}", id.dir_name());
                    job.fail(format!("{e:#}"));
                }
            }
        });
        Ok(status)
    }

    /// Asks a running installation to stop. Returns false if none is running.
    pub fn cancel(&self) -> bool {
        match self.job.lock().unwrap().as_ref() {
            Some(job) if !job.status().phase.finished() => {
                job.cancel.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    /// Deletes a compiler that Chrysanthemum installed.
    pub fn remove(&self, id: CompilerId) -> AppResult<()> {
        if self.running_job() {
            return Err(AppError::conflict(
                "an installation is in progress; wait for it to finish or cancel it first",
            ));
        }
        let dir = self.root.join(id.dir_name());
        if !dir.is_dir() {
            return Err(AppError::not_found("that compiler is not installed"));
        }
        let _guard = self.usage.try_write().map_err(|_| {
            AppError::conflict("a document is compiling right now; try again in a moment")
        })?;
        let trash = self
            .root
            .join(format!(".trash-{}-{}", id.dir_name(), nonce()));
        // Renaming first means a half-finished removal can never leave a
        // compiler that looks installed but is broken.
        fs::rename(&dir, &trash)?;
        self.refresh();
        {
            let mut slot = self.job.lock().unwrap();
            if slot.as_ref().is_some_and(|j| j.id == id) {
                *slot = None;
            }
        }
        if let Err(e) = fs::remove_dir_all(&trash) {
            tracing::warn!("could not fully delete {}: {e}", trash.display());
        }
        tracing::info!("removed {}", id.dir_name());
        Ok(())
    }

    fn run_install(&self, job: &Job, id: CompilerId, asset: &Asset) -> anyhow::Result<()> {
        let stage = self
            .root
            .join(format!(".tmp-{}-{}", id.dir_name(), nonce()));
        fs::create_dir_all(&stage)?;
        let result = self.install_in(job, id, asset, &stage);
        let _ = fs::remove_dir_all(&stage);
        result
    }

    fn install_in(
        &self,
        job: &Job,
        id: CompilerId,
        asset: &Asset,
        stage: &Path,
    ) -> anyhow::Result<()> {
        let archive = stage.join("download");
        self.download(job, asset, &archive)?;

        job.set_phase(Phase::Unpacking);
        let raw = stage.join("raw");
        let ready = stage.join("ready");
        let target = if asset.into.is_empty() {
            raw.clone()
        } else {
            raw.join(asset.into)
        };
        fs::create_dir_all(&target)?;
        unpack(&archive, asset.archive, &target, job)
            .context("could not unpack the downloaded archive")?;
        if asset.single_root {
            fs::rename(single_root(&raw)?, &ready)?;
        } else {
            fs::rename(&raw, &ready)?;
        }
        // The archive is no longer needed; free the space before the test.
        let _ = fs::remove_file(&archive);

        job.set_phase(Phase::Testing);
        let bin = bin_dir_in(&ready, id)
            .with_context(|| format!("the archive did not contain {}", id.main_program()))?;
        smoke_test(&bin.join(exe(id.main_program())))?;

        let dest = self.root.join(id.dir_name());
        if dest.exists() {
            bail!("{} appeared while installing", dest.display());
        }
        fs::rename(&ready, &dest).context("could not move the compiler into place")?;
        Ok(())
    }

    fn download(&self, job: &Job, asset: &Asset, dest: &Path) -> anyhow::Result<()> {
        let config = ureq::Agent::config_builder()
            .https_only(!self.allow_http)
            // Trust the operating system's certificates, as browsers and curl
            // do, so networks that inspect HTTPS with their own CA still work.
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(60 * 60)))
            .user_agent(format!("chrysanthemum/{}", env!("CARGO_PKG_VERSION")))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let response = agent.get(asset.url.as_ref()).call().with_context(|| {
            format!(
                "could not download {} (check your internet connection)",
                asset.url
            )
        })?;
        if let Some(len) = response.body().content_length() {
            job.total.store(len, Ordering::Relaxed);
        }
        let mut reader = response.into_body().into_reader();
        let mut file = fs::File::create(dest)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if job.cancelled() {
                return Err(Cancelled.into());
            }
            let n = reader
                .read(&mut buf)
                .context("the download was interrupted")?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            job.downloaded.fetch_add(n as u64, Ordering::Relaxed);
        }
        file.sync_all()?;

        job.set_phase(Phase::Verifying);
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if !digest.eq_ignore_ascii_case(&asset.sha256) {
            bail!(
                "the downloaded file failed its integrity check and was discarded (expected {}, got {digest})",
                asset.sha256
            );
        }
        Ok(())
    }
}

fn nonce() -> String {
    use rand::Rng;
    format!("{:016x}", rand::thread_rng().r#gen::<u64>())
}

/// Removes staging and trash directories left behind by an interrupted run.
fn remove_leftovers(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".tmp-") || name.starts_with(".trash-") {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Wraps a reader so that cancelling an installation interrupts unpacking.
struct Interruptible<'a, R> {
    inner: R,
    job: &'a Job,
}

impl<R: Read> Read for Interruptible<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.job.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, Cancelled));
        }
        self.inner.read(buf)
    }
}

fn unpack(archive: &Path, kind: Archive, dest: &Path, job: &Job) -> anyhow::Result<()> {
    let file = BufReader::new(fs::File::open(archive)?);
    let reader = Interruptible { inner: file, job };
    match kind {
        Archive::TarGz => unpack_tar(flate2::read::GzDecoder::new(reader), dest),
        Archive::TarXz => unpack_tar(liblzma::read::XzDecoder::new(reader), dest),
        Archive::Zip => {
            let mut zip = zip::ZipArchive::new(fs::File::open(archive)?)?;
            zip.extract(dest)?;
            Ok(())
        }
    }
}

fn unpack_tar<R: Read>(reader: R, dest: &Path) -> anyhow::Result<()> {
    let mut archive = tar::Archive::new(reader);
    // Programs must stay executable. `unpack` refuses entries that would land
    // outside `dest`, including through symbolic links.
    archive.set_preserve_permissions(true);
    archive.set_unpack_xattrs(false);
    archive.unpack(dest)?;
    Ok(())
}

/// Returns the only entry of `dir`, which must be a directory.
fn single_root(dir: &Path) -> anyhow::Result<PathBuf> {
    let mut entries = fs::read_dir(dir)?.flatten();
    let first = entries.next().context("the archive was empty")?;
    if entries.next().is_some() || !first.path().is_dir() {
        bail!("the archive does not have the expected layout");
    }
    Ok(first.path())
}

/// Runs `program --version` to prove the unpacked compiler works here.
fn smoke_test(program: &Path) -> anyhow::Result<()> {
    let mut child = std::process::Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "the installed compiler could not start ({})",
                program.display()
            )
        })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait()? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => bail!("the installed compiler failed its self-test ({status})"),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("the installed compiler did not answer its self-test in time");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_well_formed() {
        for spec in catalog() {
            assert!(!spec.assets.is_empty(), "{} has no downloads", spec.name);
            for (platform, asset) in &spec.assets {
                let label = platform.label();
                assert!(
                    asset.url.starts_with("https://github.com/"),
                    "{label}: downloads must come from GitHub releases over https"
                );
                assert_eq!(asset.sha256.len(), 64, "{label}: digest length");
                assert!(
                    asset.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                    "{label}: digest must be hex"
                );
                assert!(asset.download_bytes > 1_000_000, "{label}: download size");
                assert!(asset.installed_bytes > asset.download_bytes / 2);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn latexmk_needs_perl() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str| {
            let path = dir.path().join(name);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        };
        make("latexmk");
        make("pdflatex");
        let search = dir.path().as_os_str().to_os_string();
        let before = Toolchain::probe(&search, dir.path());
        assert!(
            before.pdflatex && !before.latexmk,
            "latexmk cannot run without perl"
        );
        make("perl");
        assert!(Toolchain::probe(&search, dir.path()).latexmk);
    }

    #[test]
    fn every_platform_has_a_tectonic_download() {
        let all = [
            Platform::LinuxX64,
            Platform::LinuxMuslX64,
            Platform::LinuxArm64,
            Platform::MacArm64,
            Platform::MacX64,
            Platform::WindowsX64,
        ];
        let specs = catalog();
        let tectonic = specs.iter().find(|s| s.id == CompilerId::Tectonic).unwrap();
        for p in all {
            assert!(tectonic.asset_for(p).is_some(), "{}", p.label());
        }
    }
}
