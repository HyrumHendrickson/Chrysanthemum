//! Installing and removing compilers, against a local download server so no
//! real downloads are needed.

#![cfg(unix)]

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::Arc,
    time::Duration,
};

use chrysanthemum::{
    error::AppError,
    toolchain::{Archive, Asset, CompilerId, Phase, Platform, Spec, ToolchainManager},
};
use sha2::{Digest, Sha256};

/// A `.tar.gz` holding one executable file named `tectonic`.
fn tarball(script: &str) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_size(script.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "tectonic", script.as_bytes())
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Serves `body` on a random local port; `slow` trickles it out.
fn serve(body: Vec<u8>, slow: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/tectonic.tar.gz", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            if slow {
                for chunk in body.chunks(16) {
                    if stream.write_all(chunk).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
            } else {
                let _ = stream.write_all(&body);
            }
        }
    });
    url
}

fn manager(
    root: &Path,
    url: String,
    sha: String,
    platform: Option<Platform>,
) -> Arc<ToolchainManager> {
    let spec = Spec {
        id: CompilerId::Tectonic,
        name: "Fake Tectonic",
        version: "0",
        summary: "",
        engines: &["tectonic"],
        notes: "",
        assets: vec![(
            Platform::LinuxX64,
            Asset {
                url: url.into(),
                sha256: sha.into(),
                archive: Archive::TarGz,
                download_bytes: 100,
                installed_bytes: 100,
                single_root: false,
                into: "bin",
            },
        )],
    };
    ToolchainManager::with_catalog(root.join(".toolchains"), vec![spec], platform, true)
}

async fn finish(m: &ToolchainManager) -> chrysanthemum::toolchain::JobStatus {
    for _ in 0..400 {
        if let Some(job) = m.status(true).job
            && job.phase.finished()
        {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("installation did not finish");
}

fn leftovers(root: &Path) -> Vec<String> {
    std::fs::read_dir(root.join(".toolchains"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".tmp-") || n.starts_with(".trash-"))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn installs_then_removes_a_compiler() {
    let tmp = tempfile::tempdir().unwrap();
    let body = tarball("#!/bin/sh\necho fake tectonic\n");
    let m = manager(
        tmp.path(),
        serve(body.clone(), false),
        sha256(&body),
        Some(Platform::LinuxX64),
    );
    let had_system_tectonic = m.current().system.tectonic;

    let started = m.install(CompilerId::Tectonic).unwrap();
    assert_eq!(started.id, CompilerId::Tectonic);
    let job = finish(&m).await;
    assert_eq!(job.phase, Phase::Done, "error: {:?}", job.error);

    let status = m.status(true);
    assert!(status.compilers[0].installed);
    assert!(status.active.tectonic);
    let program = m.current().program("tectonic");
    assert!(
        program.starts_with(tmp.path().join(".toolchains/tectonic/bin")),
        "{program:?}"
    );
    assert!(leftovers(tmp.path()).is_empty());

    // Installing again is refused.
    assert!(matches!(
        m.install(CompilerId::Tectonic),
        Err(AppError::Conflict(_))
    ));

    m.remove(CompilerId::Tectonic).unwrap();
    let status = m.status(true);
    assert!(!status.compilers[0].installed);
    assert_eq!(status.active.tectonic, had_system_tectonic);
    assert!(!tmp.path().join(".toolchains/tectonic").exists());
    assert!(leftovers(tmp.path()).is_empty());
    assert!(matches!(
        m.remove(CompilerId::Tectonic),
        Err(AppError::NotFound(_))
    ));
}

#[tokio::test]
async fn rejects_a_download_that_fails_its_checksum() {
    let tmp = tempfile::tempdir().unwrap();
    let body = tarball("#!/bin/sh\nexit 0\n");
    let wrong = "0".repeat(64);
    let m = manager(
        tmp.path(),
        serve(body, false),
        wrong,
        Some(Platform::LinuxX64),
    );

    m.install(CompilerId::Tectonic).unwrap();
    let job = finish(&m).await;
    assert_eq!(job.phase, Phase::Failed);
    assert!(job.error.unwrap().contains("integrity"));
    assert!(!m.status(true).compilers[0].installed);
    assert!(leftovers(tmp.path()).is_empty());
}

#[tokio::test]
async fn rejects_a_compiler_that_does_not_run() {
    let tmp = tempfile::tempdir().unwrap();
    let body = tarball("#!/bin/sh\nexit 3\n");
    let m = manager(
        tmp.path(),
        serve(body.clone(), false),
        sha256(&body),
        Some(Platform::LinuxX64),
    );

    m.install(CompilerId::Tectonic).unwrap();
    let job = finish(&m).await;
    assert_eq!(job.phase, Phase::Failed);
    assert!(job.error.unwrap().contains("self-test"));
    assert!(!tmp.path().join(".toolchains/tectonic").exists());
    assert!(leftovers(tmp.path()).is_empty());
}

#[tokio::test]
async fn an_installation_can_be_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    // Large enough that the trickling server is still sending when we cancel.
    let script = format!("#!/bin/sh\n# {}\necho x\n", "x".repeat(4000));
    let body = tarball(&script);
    let m = manager(
        tmp.path(),
        serve(body.clone(), true),
        sha256(&body),
        Some(Platform::LinuxX64),
    );

    m.install(CompilerId::Tectonic).unwrap();
    // A second installation cannot start while one is running.
    assert!(matches!(
        m.install(CompilerId::Tectonic),
        Err(AppError::Conflict(_))
    ));
    // Neither can a removal.
    assert!(matches!(
        m.remove(CompilerId::Tectonic),
        Err(AppError::Conflict(_))
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(m.cancel());
    let job = finish(&m).await;
    assert_eq!(job.phase, Phase::Cancelled);
    assert!(!m.status(true).compilers[0].installed);
    assert!(leftovers(tmp.path()).is_empty());
    assert!(!m.cancel(), "nothing is running any more");
}

#[tokio::test]
async fn unsupported_platforms_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(
        tmp.path(),
        "http://127.0.0.1:9/x".into(),
        "0".repeat(64),
        None,
    );
    assert!(matches!(
        m.install(CompilerId::Tectonic),
        Err(AppError::BadRequest(_))
    ));

    // A platform without a build for this compiler is refused too.
    let m = manager(
        tmp.path(),
        "http://127.0.0.1:9/x".into(),
        "0".repeat(64),
        Some(Platform::MacArm64),
    );
    assert!(matches!(
        m.install(CompilerId::Tectonic),
        Err(AppError::BadRequest(_))
    ));
    assert!(!m.status(true).compilers[0].available);
}

#[tokio::test]
async fn compilations_block_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let body = tarball("#!/bin/sh\necho ok\n");
    let m = manager(
        tmp.path(),
        serve(body.clone(), false),
        sha256(&body),
        Some(Platform::LinuxX64),
    );
    m.install(CompilerId::Tectonic).unwrap();
    assert_eq!(finish(&m).await.phase, Phase::Done);

    let guard = m.use_guard().await;
    assert!(matches!(
        m.remove(CompilerId::Tectonic),
        Err(AppError::Conflict(_))
    ));
    drop(guard);
    m.remove(CompilerId::Tectonic).unwrap();
}
