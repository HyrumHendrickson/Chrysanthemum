use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use chrysanthemum::{
    api::{AppState, ShareInfo, router},
    collab::Collab,
    compile::CompileManager,
    config::{Cli, Config},
    guard::Guard,
    store::Store,
    toolchain::{Toolchain, ToolchainManager},
};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("chrysanthemum=info,tower_http=warn")),
        )
        .with_target(false)
        .init();

    let cfg = Cli::parse().resolve()?;
    let store = Store::open(cfg.data_dir.clone())
        .with_context(|| format!("cannot open data directory {}", cfg.data_dir.display()))?;
    let toolchains = ToolchainManager::new(&cfg.data_dir);
    let toolchain = toolchains.current().toolchain.clone();
    let share = Arc::new(share_info(&cfg));
    let state = AppState {
        store: store.clone(),
        collab: Collab::new(store.clone()),
        compiler: Arc::new(CompileManager::new(store, toolchains, cfg.compile_timeout)),
        guard: Arc::new(Guard {
            token: cfg.token.clone(),
            loopback_only: cfg.bind.ip().is_loopback(),
        }),
        share: share.clone(),
    };

    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("cannot listen on {}", cfg.bind))?;
    let port = listener.local_addr()?.port();
    print_banner(&cfg, &toolchain, &share, port);

    if cfg.open_browser {
        let url = local_url(&cfg, port);
        if let Err(e) = webbrowser::open(&url) {
            tracing::warn!("could not open a browser: {e}");
        }
    }

    let collab = state.collab.clone();
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    collab.flush_all().await;
    tracing::info!("stopped; all documents saved");
    Ok(())
}

fn local_url(cfg: &Config, port: u16) -> String {
    let base = format!("http://localhost:{port}/");
    match &cfg.token {
        Some(t) => format!("{base}?token={t}"),
        None => base,
    }
}

fn share_info(cfg: &Config) -> ShareInfo {
    if cfg.bind.ip().is_loopback() {
        return ShareInfo::default();
    }
    let mut urls = Vec::new();
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in ifaces {
            if let std::net::IpAddr::V4(v4) = ip
                && !v4.is_loopback()
                && !v4.is_link_local()
                && !v4.is_unspecified()
            {
                urls.push(format!("http://{v4}:{}/", cfg.bind.port()));
            }
        }
    }
    ShareInfo {
        lan: true,
        urls,
        token: cfg.token.clone(),
    }
}

fn print_banner(cfg: &Config, tc: &Toolchain, share: &ShareInfo, port: u16) {
    println!("Chrysanthemum {}", env!("CARGO_PKG_VERSION"));
    println!("  projects:  {}", cfg.data_dir.display());
    println!("  local:     {}", local_url(cfg, port));
    if share.lan {
        for url in &share.urls {
            match &share.token {
                Some(t) => println!("  network:   {url}?token={t}"),
                None => println!("  network:   {url}"),
            }
        }
        if share.token.is_none() {
            println!(
                "  warning:   authentication is disabled; anyone on this network can edit and compile"
            );
        }
    } else {
        println!("  network:   off (start with --lan to collaborate with others on your network)");
    }
    let mut engines = Vec::new();
    for (name, present) in [
        ("latexmk", tc.latexmk),
        ("pdflatex", tc.pdflatex),
        ("xelatex", tc.xelatex),
        ("lualatex", tc.lualatex),
        ("tectonic", tc.tectonic),
    ] {
        if present {
            engines.push(name);
        }
    }
    if tc.any() {
        println!("  tex:       {}", engines.join(", "));
    } else {
        println!("  tex:       none found; open the Compilers menu in the browser to install one");
    }
    println!();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
