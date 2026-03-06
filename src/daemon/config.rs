//! YAML-driven daemon configuration.
//!
//! Load order: defaults → YAML file → env var overrides.
//! Mirrors the codesearch daemon config pattern exactly.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Full daemon configuration loaded from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Zoekt webserver listen port
    #[serde(default = "default_port")]
    pub port: u16,

    /// Directory for Zoekt index shards
    #[serde(default = "default_index_dir")]
    pub index_dir: String,

    /// Re-index interval in seconds
    #[serde(default = "default_index_interval")]
    pub index_interval: u64,

    /// Path to zoekt binaries (zoekt-webserver, zoekt-git-index)
    #[serde(default)]
    pub zoekt_bin: Option<String>,

    /// Path to git binary
    #[serde(default)]
    pub git_bin: Option<String>,

    /// Path to universal-ctags binary (null if disabled)
    #[serde(default)]
    pub ctags_bin: Option<String>,

    /// Incremental indexing (-delta)
    #[serde(default = "default_true")]
    pub delta: bool,

    /// Comma-separated branch list to index
    #[serde(default = "default_branches")]
    pub branches: String,

    /// Number of concurrent indexing processes
    #[serde(default = "default_parallelism")]
    pub parallelism: u32,

    /// Maximum file size in bytes to index
    #[serde(default = "default_file_limit")]
    pub file_limit: u64,

    /// Glob patterns for large files to index regardless of size
    #[serde(default)]
    pub large_files: Vec<String>,

    /// Ctags configuration
    #[serde(default)]
    pub ctags: CtagsConfig,

    /// Webserver configuration
    #[serde(default)]
    pub webserver: WebserverConfig,

    /// Explicit repository paths to index
    #[serde(default)]
    pub repos: Vec<String>,

    /// GitHub auto-discovery configuration
    #[serde(default)]
    pub github: Option<GitHubConfig>,
}

/// Ctags integration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtagsConfig {
    /// Enable ctags for symbol extraction
    #[serde(default = "default_true")]
    pub enable: bool,

    /// Require ctags to succeed (-require_ctags)
    #[serde(default = "default_true")]
    pub require: bool,
}

impl Default for CtagsConfig {
    fn default() -> Self {
        Self {
            enable: true,
            require: true,
        }
    }
}

/// Zoekt webserver settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebserverConfig {
    /// Enable RPC interface (-rpc)
    #[serde(default = "default_true")]
    pub rpc: bool,

    /// Enable HTML web UI
    #[serde(default = "default_true")]
    pub html: bool,

    /// Enable pprof profiling endpoint
    #[serde(default)]
    pub pprof: bool,

    /// Log rotation directory
    #[serde(default)]
    pub log_dir: Option<String>,

    /// Log rotation interval (Go duration format)
    #[serde(default = "default_log_refresh")]
    pub log_refresh: String,
}

impl Default for WebserverConfig {
    fn default() -> Self {
        Self {
            rpc: true,
            html: true,
            pprof: false,
            log_dir: None,
            log_refresh: default_log_refresh(),
        }
    }
}

/// GitHub auto-discovery: resolve repos from GitHub orgs/users.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitHubConfig {
    /// Path to file containing GitHub token (supports ~ expansion)
    pub token_file: Option<String>,
    /// Sources to discover repos from
    #[serde(default)]
    pub sources: Vec<GitHubSource>,
}

/// A single GitHub owner (org or user) to discover repos from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubSource {
    /// GitHub owner name (org or username)
    pub owner: String,
    /// Whether this is an org or user account
    #[serde(default)]
    pub kind: OwnerKind,
    /// Local directory where repos are/should be cloned
    pub clone_base: String,
    /// Clone repos that don't exist locally
    #[serde(default)]
    pub auto_clone: bool,
    /// Skip archived repositories
    #[serde(default = "default_true")]
    pub skip_archived: bool,
    /// Skip forked repositories
    #[serde(default)]
    pub skip_forks: bool,
    /// Glob patterns to exclude repo names
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Whether a GitHub source is an organization or user.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OwnerKind {
    #[default]
    Org,
    User,
}

fn default_port() -> u16 {
    6070
}

fn default_index_dir() -> String {
    "~/.zoekt/index".to_string()
}

fn default_index_interval() -> u64 {
    300
}

fn default_true() -> bool {
    true
}

fn default_branches() -> String {
    "HEAD".to_string()
}

fn default_parallelism() -> u32 {
    4
}

fn default_file_limit() -> u64 {
    2_097_152
}

fn default_log_refresh() -> String {
    "24h".to_string()
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            index_dir: default_index_dir(),
            index_interval: default_index_interval(),
            zoekt_bin: None,
            git_bin: None,
            ctags_bin: None,
            delta: true,
            branches: default_branches(),
            parallelism: default_parallelism(),
            file_limit: default_file_limit(),
            large_files: Vec::new(),
            ctags: CtagsConfig::default(),
            webserver: WebserverConfig::default(),
            repos: Vec::new(),
            github: None,
        }
    }
}

impl DaemonConfig {
    /// Load config from a YAML file path.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file {}: {}", path.display(), e))?;
        let mut config: Self = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config {}: {}", path.display(), e))?;

        // Env var overrides
        if let Ok(port) = std::env::var("ZOEKT_DAEMON_PORT") {
            if let Ok(p) = port.parse() {
                config.port = p;
            }
        }
        if let Ok(interval) = std::env::var("ZOEKT_INDEX_INTERVAL") {
            if let Ok(i) = interval.parse() {
                config.index_interval = i;
            }
        }
        if let Ok(dir) = std::env::var("ZOEKT_INDEX_DIR") {
            config.index_dir = dir;
        }

        Ok(config)
    }

    /// Expand ~ in all path fields using shellexpand.
    pub fn expand_paths(&mut self) {
        self.index_dir = shellexpand::tilde(&self.index_dir).into_owned();

        if let Some(ref bin) = self.zoekt_bin {
            self.zoekt_bin = Some(shellexpand::tilde(bin).into_owned());
        }
        if let Some(ref bin) = self.git_bin {
            self.git_bin = Some(shellexpand::tilde(bin).into_owned());
        }
        if let Some(ref bin) = self.ctags_bin {
            self.ctags_bin = Some(shellexpand::tilde(bin).into_owned());
        }
        if let Some(ref dir) = self.webserver.log_dir {
            self.webserver.log_dir = Some(shellexpand::tilde(dir).into_owned());
        }

        // Expand repo paths
        self.repos = self
            .repos
            .iter()
            .map(|r| shellexpand::tilde(r).into_owned())
            .collect();
    }

    /// Build a PATH string from configured binary directories.
    /// Falls back to looking up binaries on the existing PATH if not configured.
    pub fn build_path(&self) -> String {
        let mut dirs = Vec::new();

        if let Some(ref bin) = self.ctags_bin {
            dirs.push(bin.clone());
        }
        if let Some(ref bin) = self.zoekt_bin {
            dirs.push(bin.clone());
        }
        if let Some(ref bin) = self.git_bin {
            dirs.push(bin.clone());
        }

        // Append system PATH
        if let Ok(system_path) = std::env::var("PATH") {
            dirs.push(system_path);
        }

        dirs.join(":")
    }

    /// Build zoekt-webserver arguments from config.
    pub fn webserver_args(&self) -> Vec<String> {
        let mut args = vec![
            "-index".to_string(),
            self.index_dir.clone(),
            "-listen".to_string(),
            format!(":{}", self.port),
        ];

        if let Some(ref log_dir) = self.webserver.log_dir {
            args.extend(["-log_dir".to_string(), log_dir.clone()]);
        }
        args.extend(["-log_refresh".to_string(), self.webserver.log_refresh.clone()]);

        if self.webserver.rpc {
            args.push("-rpc".to_string());
        }
        if self.webserver.pprof {
            args.push("-pprof".to_string());
        }
        if !self.webserver.html {
            args.push("-html=false".to_string());
        }

        args
    }

    /// Build zoekt-git-index arguments from config (without repo paths).
    pub fn indexer_args(&self) -> Vec<String> {
        let mut args = vec!["-index".to_string(), self.index_dir.clone()];

        // Ctags
        if self.ctags.enable {
            if self.ctags.require {
                args.push("-require_ctags".to_string());
            }
        } else {
            args.push("-disable_ctags".to_string());
        }

        // Delta
        if self.delta {
            args.push("-delta".to_string());
        }

        // Branches
        if self.branches != "HEAD" {
            args.extend(["-branches".to_string(), self.branches.clone()]);
        }

        // Large files
        for pattern in &self.large_files {
            args.extend(["-large_file".to_string(), pattern.clone()]);
        }

        // Parallelism
        args.extend([
            "-parallelism".to_string(),
            self.parallelism.to_string(),
        ]);

        // File limit
        args.extend([
            "-file_limit".to_string(),
            self.file_limit.to_string(),
        ]);

        args
    }

    /// Resolve the zoekt-git-index binary path.
    pub fn indexer_bin(&self) -> PathBuf {
        if let Some(ref bin) = self.zoekt_bin {
            PathBuf::from(bin).join("zoekt-git-index")
        } else {
            PathBuf::from("zoekt-git-index")
        }
    }

    /// Resolve the zoekt-webserver binary path.
    pub fn webserver_bin(&self) -> PathBuf {
        if let Some(ref bin) = self.zoekt_bin {
            PathBuf::from(bin).join("zoekt-webserver")
        } else {
            PathBuf::from("zoekt-webserver")
        }
    }
}
