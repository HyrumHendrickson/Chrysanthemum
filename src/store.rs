//! On-disk project storage.
//!
//! Every project is a plain directory under the data root, so users can always
//! reach their files with ordinary tools. Scriptorium keeps its own metadata and
//! build output in a hidden `.scriptorium` subdirectory.

use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

pub const META_DIR: &str = ".scriptorium";
const SETTINGS_FILE: &str = "project.json";
pub const BUILD_DIR: &str = "build";
const MAX_ID_LEN: usize = 64;

const DEFAULT_TEMPLATE: &str = r"\documentclass[11pt]{article}

\usepackage[utf8]{inputenc}
\usepackage[T1]{fontenc}
\usepackage{amsmath, amssymb}
\usepackage{graphicx}
\usepackage[hidelinks]{hyperref}

\title{Untitled Document}
\author{}
\date{\today}

\begin{document}

\maketitle

\section{Introduction}
Begin writing here. The preview on the right updates whenever you compile
with \textbf{Ctrl}\,+\,\textbf{Enter}, or automatically while auto-compile is enabled.

\begin{equation}
  e^{i\pi} + 1 = 0
\end{equation}

\end{document}
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Pdflatex,
    Xelatex,
    Lualatex,
    Tectonic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub main: String,
    #[serde(default)]
    pub engine: Engine,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            main: "main.tex".into(),
            engine: Engine::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub id: String,
    /// Last modification time in milliseconds since the Unix epoch.
    pub modified: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

/// Returns true when `id` is a valid project identifier.
pub fn valid_project_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && !id.starts_with(['-', '.'])
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Validates and normalises a project-relative path (forward slashes).
pub fn clean_rel_path(raw: &str) -> AppResult<String> {
    if raw.is_empty() || raw.contains(['\0', '\\']) {
        return Err(AppError::bad_request("invalid path"));
    }
    let mut parts = Vec::new();
    for comp in Path::new(raw).components() {
        match comp {
            Component::Normal(s) => {
                let s = s
                    .to_str()
                    .ok_or_else(|| AppError::bad_request("path must be valid UTF-8"))?;
                if s == META_DIR {
                    return Err(AppError::bad_request("path is reserved"));
                }
                parts.push(s.to_string());
            }
            Component::CurDir => {}
            _ => return Err(AppError::bad_request("invalid path")),
        }
    }
    if parts.is_empty() {
        return Err(AppError::bad_request("invalid path"));
    }
    Ok(parts.join("/"))
}

/// Heuristic used to decide whether a file can be opened in the text editor.
pub fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

fn mtime_ms(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Writes `data` to `path` atomically (temporary file plus rename).
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile_in(dir)?;
    tmp.0.write_all(data)?;
    tmp.0.sync_all()?;
    fs::rename(&tmp.1, path)?;
    Ok(())
}

fn tempfile_in(dir: &Path) -> std::io::Result<(fs::File, PathBuf)> {
    use rand::Rng;
    let name = format!(".tmp-{:016x}", rand::thread_rng().r#gen::<u64>());
    let path = dir.join(name);
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    Ok((file, path))
}

impl Store {
    pub fn open(root: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project_dir(&self, id: &str) -> AppResult<PathBuf> {
        if !valid_project_id(id) {
            return Err(AppError::bad_request("invalid project name"));
        }
        let dir = self.root.join(id);
        if !dir.is_dir() {
            return Err(AppError::not_found("project not found"));
        }
        Ok(dir)
    }

    /// Resolves a project-relative path to an absolute path that is guaranteed
    /// to stay inside the project directory, including through symlinks.
    pub fn resolve(&self, id: &str, rel: &str) -> AppResult<PathBuf> {
        let dir = self.project_dir(id)?;
        let rel = clean_rel_path(rel)?;
        let full = dir.join(&rel);
        let canon_root = dir.canonicalize()?;
        let mut probe = full.as_path();
        loop {
            if let Ok(c) = probe.canonicalize() {
                if !c.starts_with(&canon_root) {
                    return Err(AppError::bad_request("path escapes project"));
                }
                break;
            }
            match probe.parent() {
                Some(p) => probe = p,
                None => return Err(AppError::bad_request("invalid path")),
            }
        }
        Ok(full)
    }

    pub fn list_projects(&self) -> AppResult<Vec<ProjectInfo>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !valid_project_id(&name) || !entry.file_type()?.is_dir() {
                continue;
            }
            let modified = self.latest_mtime(&entry.path());
            out.push(ProjectInfo { id: name, modified });
        }
        out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    fn latest_mtime(&self, dir: &Path) -> u64 {
        let mut latest = fs::metadata(dir).map(|m| mtime_ms(&m)).unwrap_or(0);
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_name() == META_DIR {
                    continue;
                }
                if let Ok(m) = e.metadata() {
                    latest = latest.max(mtime_ms(&m));
                }
            }
        }
        latest
    }

    pub fn create_project(&self, id: &str) -> AppResult<ProjectInfo> {
        if !valid_project_id(id) {
            return Err(AppError::bad_request(
                "project names may contain letters, digits, '-' and '_' (max 64)",
            ));
        }
        let dir = self.root.join(id);
        if dir.exists() {
            return Err(AppError::conflict(
                "a project with that name already exists",
            ));
        }
        fs::create_dir_all(&dir)?;
        write_atomic(&dir.join("main.tex"), DEFAULT_TEMPLATE.as_bytes())?;
        self.save_settings(id, &ProjectSettings::default())?;
        let modified = self.latest_mtime(&dir);
        Ok(ProjectInfo {
            id: id.to_string(),
            modified,
        })
    }

    pub fn delete_project(&self, id: &str) -> AppResult<()> {
        let dir = self.project_dir(id)?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    pub fn load_settings(&self, id: &str) -> AppResult<ProjectSettings> {
        let dir = self.project_dir(id)?;
        let path = dir.join(META_DIR).join(SETTINGS_FILE);
        match fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProjectSettings::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_settings(&self, id: &str, settings: &ProjectSettings) -> AppResult<()> {
        let dir = self.project_dir(id)?;
        let main = clean_rel_path(&settings.main)?;
        if !main.ends_with(".tex") {
            return Err(AppError::bad_request("main file must end in .tex"));
        }
        let normalized = ProjectSettings {
            main,
            engine: settings.engine,
        };
        let path = dir.join(META_DIR).join(SETTINGS_FILE);
        write_atomic(
            &path,
            &serde_json::to_vec_pretty(&normalized).map_err(anyhow::Error::from)?,
        )?;
        Ok(())
    }

    fn ydoc_path(&self, id: &str, rel: &str) -> AppResult<PathBuf> {
        use sha2::{Digest, Sha256};
        let rel = clean_rel_path(rel)?;
        let digest = Sha256::digest(rel.as_bytes());
        let name: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
        Ok(self
            .project_dir(id)?
            .join(META_DIR)
            .join("docs")
            .join(format!("{name}.ydoc")))
    }

    /// Reads the persisted collaborative-editing state of a file, if any.
    pub fn load_ydoc(&self, id: &str, rel: &str) -> Option<Vec<u8>> {
        fs::read(self.ydoc_path(id, rel).ok()?).ok()
    }

    pub fn save_ydoc(&self, id: &str, rel: &str, data: &[u8]) -> AppResult<()> {
        write_atomic(&self.ydoc_path(id, rel)?, data)?;
        Ok(())
    }

    pub fn remove_ydoc(&self, id: &str, rel: &str) {
        if let Ok(p) = self.ydoc_path(id, rel) {
            let _ = fs::remove_file(p);
        }
    }

    pub fn build_dir(&self, id: &str) -> AppResult<PathBuf> {
        Ok(self.project_dir(id)?.join(META_DIR).join(BUILD_DIR))
    }

    pub fn list_files(&self, id: &str) -> AppResult<Vec<Entry>> {
        let dir = self.project_dir(id)?;
        let mut out = Vec::new();
        walk(&dir, &dir, &mut out)?;
        out.sort_by_cached_key(sort_key);
        Ok(out)
    }

    pub fn read_file(&self, id: &str, rel: &str) -> AppResult<Vec<u8>> {
        let path = self.resolve(id, rel)?;
        if !path.is_file() {
            return Err(AppError::not_found("file not found"));
        }
        Ok(fs::read(path)?)
    }

    pub fn write_file(&self, id: &str, rel: &str, data: &[u8]) -> AppResult<()> {
        let path = self.resolve(id, rel)?;
        if path.is_dir() {
            return Err(AppError::conflict("a directory exists at that path"));
        }
        write_atomic(&path, data)?;
        Ok(())
    }

    pub fn create_entry(&self, id: &str, rel: &str, kind: EntryKind) -> AppResult<()> {
        let path = self.resolve(id, rel)?;
        if path.exists() {
            return Err(AppError::conflict("already exists"));
        }
        match kind {
            EntryKind::Dir => fs::create_dir_all(path)?,
            EntryKind::File => write_atomic(&path, b"")?,
        }
        Ok(())
    }

    pub fn delete_entry(&self, id: &str, rel: &str) -> AppResult<()> {
        let path = self.resolve(id, rel)?;
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn rename_entry(&self, id: &str, from: &str, to: &str) -> AppResult<()> {
        let src = self.resolve(id, from)?;
        let dst = self.resolve(id, to)?;
        if !src.exists() {
            return Err(AppError::not_found("source not found"));
        }
        if dst.exists() {
            return Err(AppError::conflict("destination already exists"));
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(src, dst)?;
        Ok(())
    }

    /// Builds a ZIP archive of the project sources (build output excluded).
    pub fn zip_project(&self, id: &str) -> AppResult<Vec<u8>> {
        use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
        let entries = self.list_files(id)?;
        let dir = self.project_dir(id)?;
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for e in entries {
            match e.kind {
                EntryKind::Dir => writer
                    .add_directory(format!("{}/", e.path), opts)
                    .map_err(anyhow::Error::from)?,
                EntryKind::File => {
                    writer
                        .start_file(e.path.clone(), opts)
                        .map_err(anyhow::Error::from)?;
                    writer.write_all(&fs::read(dir.join(&e.path))?)?;
                }
            }
        }
        let cursor = writer.finish().map_err(anyhow::Error::from)?;
        Ok(cursor.into_inner())
    }
}

fn sort_key(e: &Entry) -> (String, u8, String) {
    // Directories before files at the same level, otherwise case-insensitive path order.
    let parent = e
        .path
        .rsplit_once('/')
        .map(|(p, _)| p)
        .unwrap_or("")
        .to_lowercase();
    let rank = if e.kind == EntryKind::Dir { 0 } else { 1 };
    (parent, rank, e.path.to_lowercase())
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> AppResult<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if name_s == META_DIR || name_s.starts_with(".tmp-") {
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(anyhow::Error::from)?
            .to_string_lossy()
            .replace('\\', "/");
        if ft.is_dir() {
            out.push(Entry {
                path: rel,
                kind: EntryKind::Dir,
                size: 0,
            });
            walk(root, &path, out)?;
        } else if ft.is_file() {
            out.push(Entry {
                path: rel,
                kind: EntryKind::File,
                size: entry.metadata()?.len(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ids() {
        assert!(valid_project_id("thesis_2026"));
        assert!(valid_project_id("A-b_9"));
        assert!(!valid_project_id(""));
        assert!(!valid_project_id("../x"));
        assert!(!valid_project_id("-lead"));
        assert!(!valid_project_id(".hidden"));
        assert!(!valid_project_id("has space"));
        assert!(!valid_project_id(&"a".repeat(65)));
    }

    #[test]
    fn clean_paths() {
        assert_eq!(clean_rel_path("a/b.tex").unwrap(), "a/b.tex");
        assert_eq!(clean_rel_path("./a//b.tex").unwrap(), "a/b.tex");
        assert!(clean_rel_path("../a").is_err());
        assert!(clean_rel_path("/etc/passwd").is_err());
        assert!(clean_rel_path("a/../../b").is_err());
        assert!(clean_rel_path(".scriptorium/project.json").is_err());
        assert!(clean_rel_path("a\\b").is_err());
        assert!(clean_rel_path("").is_err());
    }

    #[test]
    fn text_detection() {
        assert!(is_text("héllo \\LaTeX".as_bytes()));
        assert!(!is_text(&[0x89, b'P', b'N', b'G', 0]));
        assert!(!is_text(&[0xff, 0xfe, 0xfd]));
    }

    #[test]
    fn project_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_path_buf()).unwrap();
        store.create_project("demo").unwrap();
        assert!(store.create_project("demo").is_err());
        let files = store.list_files("demo").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "main.tex");

        store
            .write_file("demo", "sections/intro.tex", b"hi")
            .unwrap();
        assert_eq!(
            store.read_file("demo", "sections/intro.tex").unwrap(),
            b"hi"
        );
        store
            .rename_entry("demo", "sections/intro.tex", "sections/a.tex")
            .unwrap();
        store.delete_entry("demo", "sections/a.tex").unwrap();
        assert!(store.read_file("demo", "sections/a.tex").is_err());
        assert!(store.resolve("demo", "../other").is_err());

        let zip = store.zip_project("demo").unwrap();
        assert!(zip.starts_with(b"PK"));
        store.delete_project("demo").unwrap();
        assert!(store.list_projects().unwrap().is_empty());
    }
}
