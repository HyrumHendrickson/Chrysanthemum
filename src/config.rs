//! Command-line configuration.

use std::{net::IpAddr, path::PathBuf, time::Duration};

use clap::Parser;

/// Chrysanthemum: a local-first, collaborative LaTeX editor.
#[derive(Debug, Clone, Parser)]
#[command(name = "chrysanthemum", version, about, long_about = None)]
pub struct Cli {
    /// Directory in which projects are stored.
    #[arg(long, env = "CHRYSANTHEMUM_DATA_DIR", value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// Address to bind. Use 0.0.0.0 (or --lan) to accept connections from other machines.
    #[arg(long, env = "CHRYSANTHEMUM_HOST", default_value = "127.0.0.1")]
    pub host: IpAddr,

    /// Port to listen on.
    #[arg(short, long, env = "CHRYSANTHEMUM_PORT", default_value_t = 3000)]
    pub port: u16,

    /// Share with other machines on the local network (binds 0.0.0.0).
    #[arg(long)]
    pub lan: bool,

    /// Access token required from remote clients. Generated automatically when
    /// the server is reachable from the network and no token is given.
    #[arg(long, env = "CHRYSANTHEMUM_TOKEN")]
    pub token: Option<String>,

    /// Disable access-token authentication even when serving on the network.
    #[arg(long, conflicts_with = "token")]
    pub no_auth: bool,

    /// Maximum wall-clock time for a single compilation, in seconds.
    #[arg(long, env = "CHRYSANTHEMUM_COMPILE_TIMEOUT", default_value_t = 90)]
    pub compile_timeout: u64,

    /// Open the web interface in the default browser on start.
    #[arg(long)]
    pub open: bool,
}

/// Resolved runtime settings.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub bind: std::net::SocketAddr,
    pub token: Option<String>,
    pub compile_timeout: Duration,
    pub open_browser: bool,
}

impl Cli {
    pub fn resolve(self) -> anyhow::Result<Config> {
        let host = if self.lan {
            IpAddr::from([0, 0, 0, 0])
        } else {
            self.host
        };
        let exposed = !host.is_loopback();
        let token = match (self.token, self.no_auth, exposed) {
            (Some(t), _, _) => Some(t),
            (None, true, _) => None,
            (None, false, true) => Some(generate_token()),
            (None, false, false) => None,
        };
        if let Some(t) = &token
            && (t.is_empty()
                || !t
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)))
        {
            anyhow::bail!(
                "the access token may only contain letters, digits, '-', '.', '_' and '~'"
            );
        }
        let data_dir = match self.data_dir {
            Some(d) => d,
            None => default_data_dir()?,
        };
        Ok(Config {
            data_dir,
            bind: std::net::SocketAddr::new(host, self.port),
            token,
            compile_timeout: Duration::from_secs(self.compile_timeout.max(1)),
            open_browser: self.open,
        })
    }
}

fn generate_token() -> String {
    use rand::{Rng, distributions::Alphanumeric};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect()
}

/// Where projects live unless `--data-dir` says otherwise.
///
/// Installations from before the project was renamed keep their data: when the
/// old directory exists and the new one does not, the old one is moved.
fn default_data_dir() -> anyhow::Result<PathBuf> {
    let dir = platform_data_dir("chrysanthemum", "Chrysanthemum")?;
    if !dir.exists() {
        let legacy = platform_data_dir("scriptorium", "Scriptorium")?;
        if legacy.is_dir() {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match std::fs::rename(&legacy, &dir) {
                Ok(()) => eprintln!(
                    "Moved your projects from {} to {}",
                    legacy.display(),
                    dir.display()
                ),
                Err(e) => {
                    eprintln!(
                        "Could not move {} to {} ({e}); continuing to use the old location",
                        legacy.display(),
                        dir.display()
                    );
                    return Ok(legacy);
                }
            }
        }
    }
    Ok(dir)
}

/// `lower` is used on Linux, `title` on macOS and Windows.
fn platform_data_dir(lower: &str, title: &str) -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join(lower));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory; pass --data-dir"))?;
    if cfg!(target_os = "macos") {
        Ok(home.join("Library/Application Support").join(title))
    } else if cfg!(windows) {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"));
        Ok(base.join(title))
    } else {
        Ok(home.join(".local/share").join(lower))
    }
}
