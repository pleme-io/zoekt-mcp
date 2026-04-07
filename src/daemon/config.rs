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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_defaults_are_sensible() {
        let config = DaemonConfig::default();
        assert_eq!(config.port, 6070);
        assert_eq!(config.index_dir, "~/.zoekt/index");
        assert_eq!(config.index_interval, 300);
        assert!(config.delta);
        assert_eq!(config.branches, "HEAD");
        assert_eq!(config.parallelism, 4);
        assert_eq!(config.file_limit, 2_097_152);
        assert!(config.large_files.is_empty());
        assert!(config.repos.is_empty());
        assert!(config.github.is_none());
        assert!(config.zoekt_bin.is_none());
        assert!(config.git_bin.is_none());
        assert!(config.ctags_bin.is_none());
    }

    #[test]
    fn test_ctags_config_default() {
        let ctags = CtagsConfig::default();
        assert!(ctags.enable);
        assert!(ctags.require);
    }

    #[test]
    fn test_webserver_config_default() {
        let ws = WebserverConfig::default();
        assert!(ws.rpc);
        assert!(ws.html);
        assert!(!ws.pprof);
        assert!(ws.log_dir.is_none());
        assert_eq!(ws.log_refresh, "24h");
    }

    #[test]
    fn test_load_minimal_yaml() {
        let yaml = "port: 9090\n";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();
        let config = DaemonConfig::load(tmp.path()).unwrap();
        assert_eq!(config.port, 9090);
        assert_eq!(config.index_dir, "~/.zoekt/index");
        assert_eq!(config.index_interval, 300);
    }

    #[test]
    fn test_load_full_yaml() {
        let yaml = r#"
port: 8080
index_dir: /tmp/zoekt-test
index_interval: 60
zoekt_bin: /opt/zoekt/bin
git_bin: /usr/local/bin
ctags_bin: /usr/bin
delta: false
branches: "HEAD,main"
parallelism: 8
file_limit: 1048576
large_files:
  - "*.pb.go"
  - "*.min.js"
ctags:
  enable: false
  require: false
webserver:
  rpc: false
  html: false
  pprof: true
  log_dir: /var/log/zoekt
  log_refresh: "12h"
repos:
  - /home/user/project1
  - /home/user/project2
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();
        let config = DaemonConfig::load(tmp.path()).unwrap();
        assert_eq!(config.port, 8080);
        assert_eq!(config.index_dir, "/tmp/zoekt-test");
        assert_eq!(config.index_interval, 60);
        assert_eq!(config.zoekt_bin.as_deref(), Some("/opt/zoekt/bin"));
        assert_eq!(config.git_bin.as_deref(), Some("/usr/local/bin"));
        assert_eq!(config.ctags_bin.as_deref(), Some("/usr/bin"));
        assert!(!config.delta);
        assert_eq!(config.branches, "HEAD,main");
        assert_eq!(config.parallelism, 8);
        assert_eq!(config.file_limit, 1_048_576);
        assert_eq!(config.large_files, vec!["*.pb.go", "*.min.js"]);
        assert!(!config.ctags.enable);
        assert!(!config.ctags.require);
        assert!(!config.webserver.rpc);
        assert!(!config.webserver.html);
        assert!(config.webserver.pprof);
        assert_eq!(config.webserver.log_dir.as_deref(), Some("/var/log/zoekt"));
        assert_eq!(config.webserver.log_refresh, "12h");
        assert_eq!(config.repos.len(), 2);
    }

    #[test]
    fn test_load_invalid_yaml_returns_error() {
        let yaml = "{{{{not valid yaml";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();
        let result = DaemonConfig::load(tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to parse config"), "got: {err}");
    }

    #[test]
    fn test_load_nonexistent_file_returns_error() {
        let result = DaemonConfig::load(Path::new("/nonexistent/config.yaml"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to read config file"), "got: {err}");
    }

    #[test]
    fn test_load_empty_yaml_uses_defaults() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"{}").unwrap();
        let config = DaemonConfig::load(tmp.path()).unwrap();
        assert_eq!(config.port, 6070);
        assert_eq!(config.index_interval, 300);
    }

    #[test]
    fn test_expand_paths_tilde() {
        let mut config = DaemonConfig {
            index_dir: "~/zoekt-test".to_string(),
            zoekt_bin: Some("~/bin/zoekt".to_string()),
            git_bin: Some("~/bin/git".to_string()),
            ctags_bin: Some("~/bin/ctags".to_string()),
            webserver: WebserverConfig {
                log_dir: Some("~/logs".to_string()),
                ..Default::default()
            },
            repos: vec!["~/code/repo1".to_string(), "~/code/repo2".to_string()],
            ..Default::default()
        };
        config.expand_paths();

        assert!(!config.index_dir.starts_with('~'), "index_dir not expanded: {}", config.index_dir);
        assert!(!config.zoekt_bin.as_ref().unwrap().starts_with('~'));
        assert!(!config.git_bin.as_ref().unwrap().starts_with('~'));
        assert!(!config.ctags_bin.as_ref().unwrap().starts_with('~'));
        assert!(!config.webserver.log_dir.as_ref().unwrap().starts_with('~'));
        for repo in &config.repos {
            assert!(!repo.starts_with('~'), "repo not expanded: {repo}");
        }
    }

    #[test]
    fn test_expand_paths_no_tilde_is_noop() {
        let mut config = DaemonConfig {
            index_dir: "/absolute/path".to_string(),
            zoekt_bin: None,
            git_bin: None,
            ctags_bin: None,
            webserver: WebserverConfig {
                log_dir: None,
                ..Default::default()
            },
            repos: vec!["/abs/repo".to_string()],
            ..Default::default()
        };
        let original_index = config.index_dir.clone();
        config.expand_paths();
        assert_eq!(config.index_dir, original_index);
        assert_eq!(config.repos[0], "/abs/repo");
    }

    #[test]
    fn test_build_path_with_all_bins() {
        let config = DaemonConfig {
            ctags_bin: Some("/opt/ctags/bin".to_string()),
            zoekt_bin: Some("/opt/zoekt/bin".to_string()),
            git_bin: Some("/opt/git/bin".to_string()),
            ..Default::default()
        };
        let path = config.build_path();
        assert!(path.starts_with("/opt/ctags/bin:/opt/zoekt/bin:/opt/git/bin"));
    }

    #[test]
    fn test_build_path_no_custom_bins() {
        let config = DaemonConfig::default();
        let path = config.build_path();
        // Should still include system PATH
        assert!(!path.is_empty());
    }

    #[test]
    fn test_webserver_args_default() {
        let config = DaemonConfig::default();
        let args = config.webserver_args();
        assert!(args.contains(&"-index".to_string()));
        assert!(args.contains(&config.index_dir));
        assert!(args.contains(&"-listen".to_string()));
        assert!(args.contains(&":6070".to_string()));
        assert!(args.contains(&"-rpc".to_string()));
        assert!(args.contains(&"-log_refresh".to_string()));
        assert!(args.contains(&"24h".to_string()));
        assert!(!args.contains(&"-pprof".to_string()));
        assert!(!args.contains(&"-html=false".to_string()));
    }

    #[test]
    fn test_webserver_args_custom() {
        let config = DaemonConfig {
            port: 9090,
            index_dir: "/tmp/idx".to_string(),
            webserver: WebserverConfig {
                rpc: false,
                html: false,
                pprof: true,
                log_dir: Some("/var/log".to_string()),
                log_refresh: "1h".to_string(),
            },
            ..Default::default()
        };
        let args = config.webserver_args();
        assert!(args.contains(&":9090".to_string()));
        assert!(args.contains(&"/tmp/idx".to_string()));
        assert!(args.contains(&"-pprof".to_string()));
        assert!(args.contains(&"-html=false".to_string()));
        assert!(args.contains(&"-log_dir".to_string()));
        assert!(args.contains(&"/var/log".to_string()));
        assert!(!args.contains(&"-rpc".to_string()));
    }

    #[test]
    fn test_indexer_args_default() {
        let config = DaemonConfig::default();
        let args = config.indexer_args();
        assert!(args.contains(&"-index".to_string()));
        assert!(args.contains(&"-require_ctags".to_string()));
        assert!(args.contains(&"-delta".to_string()));
        assert!(args.contains(&"-parallelism".to_string()));
        assert!(args.contains(&"4".to_string()));
        assert!(args.contains(&"-file_limit".to_string()));
        assert!(args.contains(&"2097152".to_string()));
        assert!(!args.contains(&"-branches".to_string()));
    }

    #[test]
    fn test_indexer_args_ctags_disabled() {
        let config = DaemonConfig {
            ctags: CtagsConfig {
                enable: false,
                require: false,
            },
            ..Default::default()
        };
        let args = config.indexer_args();
        assert!(args.contains(&"-disable_ctags".to_string()));
        assert!(!args.contains(&"-require_ctags".to_string()));
    }

    #[test]
    fn test_indexer_args_ctags_enabled_no_require() {
        let config = DaemonConfig {
            ctags: CtagsConfig {
                enable: true,
                require: false,
            },
            ..Default::default()
        };
        let args = config.indexer_args();
        assert!(!args.contains(&"-require_ctags".to_string()));
        assert!(!args.contains(&"-disable_ctags".to_string()));
    }

    #[test]
    fn test_indexer_args_custom_branches() {
        let config = DaemonConfig {
            branches: "HEAD,main,develop".to_string(),
            ..Default::default()
        };
        let args = config.indexer_args();
        assert!(args.contains(&"-branches".to_string()));
        assert!(args.contains(&"HEAD,main,develop".to_string()));
    }

    #[test]
    fn test_indexer_args_large_files() {
        let config = DaemonConfig {
            large_files: vec!["*.pb.go".to_string(), "*.min.js".to_string()],
            ..Default::default()
        };
        let args = config.indexer_args();
        let lf_positions: Vec<_> = args.iter().enumerate()
            .filter(|(_, a)| *a == "-large_file")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(lf_positions.len(), 2);
        assert_eq!(args[lf_positions[0] + 1], "*.pb.go");
        assert_eq!(args[lf_positions[1] + 1], "*.min.js");
    }

    #[test]
    fn test_indexer_args_no_delta() {
        let config = DaemonConfig {
            delta: false,
            ..Default::default()
        };
        let args = config.indexer_args();
        assert!(!args.contains(&"-delta".to_string()));
    }

    #[test]
    fn test_indexer_bin_with_zoekt_bin() {
        let config = DaemonConfig {
            zoekt_bin: Some("/opt/zoekt/bin".to_string()),
            ..Default::default()
        };
        assert_eq!(config.indexer_bin(), PathBuf::from("/opt/zoekt/bin/zoekt-git-index"));
    }

    #[test]
    fn test_indexer_bin_without_zoekt_bin() {
        let config = DaemonConfig::default();
        assert_eq!(config.indexer_bin(), PathBuf::from("zoekt-git-index"));
    }

    #[test]
    fn test_webserver_bin_with_zoekt_bin() {
        let config = DaemonConfig {
            zoekt_bin: Some("/opt/zoekt/bin".to_string()),
            ..Default::default()
        };
        assert_eq!(config.webserver_bin(), PathBuf::from("/opt/zoekt/bin/zoekt-webserver"));
    }

    #[test]
    fn test_webserver_bin_without_zoekt_bin() {
        let config = DaemonConfig::default();
        assert_eq!(config.webserver_bin(), PathBuf::from("zoekt-webserver"));
    }

    #[test]
    fn test_github_config_deserialization() {
        let yaml = r#"
port: 6070
github:
  token_file: "~/.config/gh/token"
  sources:
    - owner: myorg
      kind: org
      clone_base: ~/code/org
      auto_clone: true
      skip_archived: true
      skip_forks: true
      exclude:
        - "*.wiki"
        - "legacy-*"
    - owner: myuser
      kind: user
      clone_base: ~/code/personal
      auto_clone: false
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();
        let config = DaemonConfig::load(tmp.path()).unwrap();
        let gh = config.github.unwrap();
        assert_eq!(gh.token_file.as_deref(), Some("~/.config/gh/token"));
        assert_eq!(gh.sources.len(), 2);

        let src0 = &gh.sources[0];
        assert_eq!(src0.owner, "myorg");
        assert!(matches!(src0.kind, OwnerKind::Org));
        assert!(src0.auto_clone);
        assert!(src0.skip_archived);
        assert!(src0.skip_forks);
        assert_eq!(src0.exclude.len(), 2);

        let src1 = &gh.sources[1];
        assert_eq!(src1.owner, "myuser");
        assert!(matches!(src1.kind, OwnerKind::User));
        assert!(!src1.auto_clone);
    }

    #[test]
    fn test_owner_kind_default_is_org() {
        let kind = OwnerKind::default();
        assert!(matches!(kind, OwnerKind::Org));
    }

    #[test]
    fn test_config_roundtrip_serde() {
        let config = DaemonConfig {
            port: 7777,
            index_dir: "/custom/idx".to_string(),
            index_interval: 120,
            repos: vec!["/repo/a".to_string()],
            ..Default::default()
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: DaemonConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.port, 7777);
        assert_eq!(parsed.index_dir, "/custom/idx");
        assert_eq!(parsed.index_interval, 120);
        assert_eq!(parsed.repos, vec!["/repo/a"]);
    }
}
