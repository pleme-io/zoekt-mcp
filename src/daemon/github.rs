//! GitHub auto-discovery: list repos from orgs/users, clone missing ones.
//!
//! All errors are non-fatal — GitHub failure never blocks the daemon.
//! Missing tokens, API errors, and clone failures are logged and skipped.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{error, info, warn};

use super::config::{GitHubConfig, GitHubSource, OwnerKind};

/// Minimal GitHub repo response (only fields we need).
#[derive(Debug, Deserialize)]
struct GitHubRepo {
    name: String,
    clone_url: String,
    archived: bool,
    fork: bool,
}

// ── tend discovery-cache reader ─────────────────────────────────────
//
// tend (a sibling daemon on this fleet, its own 300s reconcile loop)
// already discovers + caches every configured org/user's repo list at
// `{XDG_CACHE_HOME:-~/.cache}/tend/discovery/{org}.json` with a 6-hour
// TTL (see `tend/src/cache.rs::DiscoveryCache`). Without this, every
// zoekt daemon tick (every `index_interval`, default 300s) re-derives
// that same fact with a raw, uncached GitHub API pagination — the
// single most wasteful call in the daemon's tick. Reading tend's
// cache first turns a per-tick network listing into a per-6h one,
// with zero coupling to the tend crate: this is a ~20-line local
// reader of a 3-field JSON document, not a dependency.
//
// Deliberately NOT a `Cargo.toml` dependency on `tend` — the format is
// simple enough that mirroring the read is cheaper and more stable
// than a cross-repo type dependency.

/// Mirror of tend's on-disk discovery-cache entry — a flat
/// `{org, repos, timestamp}` JSON document. Only the fields this
/// reader consumes are declared; `org` is present in the real file
/// (redundant with the filename) but unused here — serde ignores it.
#[derive(Debug, Deserialize)]
struct TendCacheEntry {
    repos: Vec<String>,
    timestamp: u64,
}

/// tend's discovery-cache freshness window — mirrors
/// `tend::cache::DEFAULT_TTL_SECS` (tend/src/cache.rs). A cache file
/// older than this is treated as a miss, same as an absent file.
const TEND_CACHE_TTL_SECS: u64 = 6 * 60 * 60;

/// Resolve tend's cache root the same way tend does:
/// `$XDG_CACHE_HOME/tend/discovery`, falling back to
/// `~/.cache/tend/discovery` when unset. Mirrors
/// `tend::cache::tend_cache_root()` (tend/src/cache.rs).
fn tend_discovery_cache_dir() -> PathBuf {
    // okiba resolves the tier and REFUSES a relative override rather than
    // resolving it against the cwd (the XDG spec's rule, which the hand-rolled
    // version above ignored — a relative XDG_CACHE_HOME scattered the cache
    // per-working-directory). `from_env` rather than `for_app` so
    // `dirs::home_dir()` still backs $HOME: a launchd/systemd unit with HOME
    // unset resolves via getpwuid, which a bare `for_app` would have lost.
    okiba::Okiba::from_env("tend", |k| match k {
        "HOME" => std::env::var("HOME").ok().or_else(|| {
            dirs::home_dir().map(|p| p.to_string_lossy().into_owned())
        }),
        other => std::env::var(other).ok(),
    })
    .base(okiba::Tier::Cache)
    .unwrap_or_else(|_| std::env::temp_dir())
    .join("tend")
        .join("discovery")
}

/// Read tend's discovery cache for `owner` from `cache_dir`, honoring
/// its 6-hour TTL. Returns `None` uniformly on any miss — absent
/// file, unparseable JSON, or a stale timestamp — so the caller
/// always has a safe, unconditional fallback to live discovery.
/// `cache_dir` is a parameter (rather than baked in) so tests can
/// point it at a temp directory without touching the real
/// `~/.cache/tend`.
fn read_tend_cache_from(cache_dir: &Path, owner: &str) -> Option<Vec<String>> {
    let path = cache_dir.join(format!("{owner}.json"));
    let content = std::fs::read_to_string(path).ok()?;
    let entry: TendCacheEntry = serde_json::from_str(&content).ok()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if now.saturating_sub(entry.timestamp) > TEND_CACHE_TTL_SECS {
        return None;
    }

    Some(entry.repos)
}

/// Read tend's discovery cache for `owner` at its real on-disk
/// location — the first-choice check before falling back to a live
/// GitHub API listing. `None` whenever tend isn't installed or its
/// cache is stale/absent for this owner; the caller falls through to
/// `GitHubClient::list_repos` exactly as it always has.
fn read_tend_cache(owner: &str) -> Option<Vec<String>> {
    read_tend_cache_from(&tend_discovery_cache_dir(), owner)
}

/// GitHub API client with bearer token auth.
struct GitHubClient {
    client: reqwest::Client,
    token: String,
}

impl GitHubClient {
    fn new(token: String) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("zoekt-mcp-daemon")
            .build()?;
        Ok(Self { client, token })
    }

    /// List all repos for a source (paginated, 100 per page).
    async fn list_repos(&self, source: &GitHubSource) -> anyhow::Result<Vec<GitHubRepo>> {
        let base_url = match source.kind {
            OwnerKind::Org => format!("https://api.github.com/orgs/{}/repos", source.owner),
            OwnerKind::User => format!("https://api.github.com/users/{}/repos", source.owner),
        };

        let mut all_repos = Vec::new();
        let mut page = 1u32;

        loop {
            let resp = self
                .client
                .get(&base_url)
                .query(&[
                    ("per_page", "100"),
                    ("page", &page.to_string()),
                ])
                .header("Authorization", format!("Bearer {}", self.token))
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await?;

            // Check rate limit
            if let Some(remaining) = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u32>().ok())
            {
                if remaining == 0 {
                    warn!("GitHub API rate limit exhausted, stopping pagination");
                    break;
                }
            }

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!("GitHub API returned {}: {}", status, body));
            }

            let repos: Vec<GitHubRepo> = resp.json().await?;
            let count = repos.len();
            all_repos.extend(repos);

            if count < 100 {
                break;
            }
            page += 1;
        }

        Ok(all_repos)
    }
}

/// Resolve token from token_file (with ~ expansion) or GITHUB_TOKEN env var.
fn resolve_token(config: &GitHubConfig) -> Option<String> {
    // Try token_file first
    if let Some(ref path) = config.token_file {
        let expanded = shellexpand::tilde(path);
        match std::fs::read_to_string(expanded.as_ref()) {
            Ok(token) => {
                let token = token.trim().to_string();
                if !token.is_empty() {
                    return Some(token);
                }
                warn!("Token file {} is empty", path);
            }
            Err(e) => {
                warn!("Failed to read token file {}: {}", path, e);
            }
        }
    }

    // Fall back to env var
    match std::env::var("GITHUB_TOKEN") {
        Ok(token) if !token.is_empty() => Some(token),
        _ => None,
    }
}

/// Simple wildcard pattern matcher (supports `*` only).
fn matches_pattern(name: &str, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.len() == 1 {
        return name == pattern;
    }

    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if i == 0 {
            if !name.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            if !name[pos..].ends_with(part) {
                return false;
            }
            pos = name.len();
        } else {
            match name[pos..].find(part) {
                Some(found) => pos += found + part.len(),
                None => return false,
            }
        }
    }

    true
}

fn is_excluded(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| matches_pattern(name, p))
}

fn filter_repos(repos: Vec<GitHubRepo>, source: &GitHubSource) -> Vec<GitHubRepo> {
    repos
        .into_iter()
        .filter(|r| {
            if source.skip_archived && r.archived {
                return false;
            }
            if source.skip_forks && r.fork {
                return false;
            }
            if is_excluded(&r.name, &source.exclude) {
                return false;
            }
            true
        })
        .collect()
}

/// Clone a repo using git CLI (via tokio::process::Command).
async fn clone_repo(
    clone_url: &str,
    dest: &PathBuf,
    git_bin: &Option<String>,
) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let git = git_bin
        .as_ref()
        .map(|b| format!("{}/git", b))
        .unwrap_or_else(|| "git".to_string());

    let status = tokio::process::Command::new(&git)
        .args(["clone", "--quiet", clone_url])
        .arg(dest)
        .status()
        .await?;

    if !status.success() {
        return Err(anyhow::anyhow!(
            "git clone failed with exit code {:?}",
            status.code()
        ));
    }

    Ok(())
}

/// Resolve all repos from GitHub sources + explicit list.
///
/// Returns a deduplicated list of repo paths. All GitHub errors are non-fatal.
pub async fn resolve_all_repos(
    explicit: Vec<String>,
    github_config: Option<&GitHubConfig>,
    git_bin: &Option<String>,
) -> Vec<String> {
    let mut all_paths: Vec<String> = explicit;
    let mut seen = HashSet::new();

    let config = match github_config {
        Some(c) if !c.sources.is_empty() => c,
        _ => return all_paths,
    };

    let token = match resolve_token(config) {
        Some(t) => t,
        None => {
            warn!("No GitHub token available — skipping repo discovery (set token_file or GITHUB_TOKEN)");
            return all_paths;
        }
    };

    let client: GitHubClient = match GitHubClient::new(token) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to create GitHub client: {}", e);
            return all_paths;
        }
    };

    for source in &config.sources {
        let repos: Vec<GitHubRepo> = if let Some(names) = read_tend_cache(&source.owner) {
            // Cache hit — tend already has a fresh (<6h) repo list for
            // this owner. Skip the network listing call entirely; only
            // the LISTING is replaced, everything downstream (filter,
            // local-path resolution, auto_clone) is unchanged. Cached
            // entries carry no archived/fork flags (tend's cache is
            // name-only, and tend already excludes archived repos
            // before caching), so those two are synthesized as
            // `false` — equivalent to tend's own upstream filtering
            // for `archived`, and a documented relaxation for `fork`
            // (forks survive tend's cache; `skip_forks` won't catch
            // them via this path). `exclude` glob filtering is
            // unaffected — it's name-based.
            info!(
                "Using tend's discovery cache for {} {} ({} repos) — skipping GitHub API listing",
                match source.kind {
                    OwnerKind::Org => "org",
                    OwnerKind::User => "user",
                },
                source.owner,
                names.len()
            );
            names
                .into_iter()
                .map(|name| {
                    let clone_url = format!("https://github.com/{}/{}.git", source.owner, name);
                    GitHubRepo {
                        name,
                        clone_url,
                        archived: false,
                        fork: false,
                    }
                })
                .collect()
        } else {
            info!(
                "Discovering repos from {} {} (clone_base: {})",
                match source.kind {
                    OwnerKind::Org => "org",
                    OwnerKind::User => "user",
                },
                source.owner,
                source.clone_base
            );

            match client.list_repos(source).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Failed to list repos for {}: {}", source.owner, e);
                    continue;
                }
            }
        };

        let total = repos.len();
        let filtered = filter_repos(repos, source);
        info!(
            "Found {} repos for {} ({} after filtering)",
            total, source.owner, filtered.len()
        );

        let clone_base = shellexpand::tilde(&source.clone_base).into_owned();
        let clone_base = PathBuf::from(&clone_base);

        for repo in &filtered {
            let local_path = clone_base.join(&repo.name);

            if local_path.exists() {
                info!("Found local clone: {}", local_path.display());
                all_paths.push(local_path.to_string_lossy().to_string());
            } else if source.auto_clone {
                info!("Cloning {} → {}", repo.name, local_path.display());
                match clone_repo(&repo.clone_url, &local_path, git_bin).await {
                    Ok(()) => {
                        info!("Cloned {}", repo.name);
                        all_paths.push(local_path.to_string_lossy().to_string());
                    }
                    Err(e) => {
                        error!("Failed to clone {}: {}", repo.name, e);
                    }
                }
            } else {
                info!("Skipping {} (not cloned, auto_clone=false)", repo.name);
            }
        }
    }

    // Deduplicate by canonical path
    all_paths.retain(|p| {
        let key = std::path::Path::new(p)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(p));
        seen.insert(key)
    });

    all_paths
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── read_tend_cache_from ─────────────────────────────────────────────

    fn write_cache_file(dir: &std::path::Path, owner: &str, contents: &str) {
        std::fs::write(dir.join(format!("{owner}.json")), contents).unwrap();
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn test_read_tend_cache_fresh_hit() {
        let dir = tempfile::tempdir().unwrap();
        let json = format!(
            r#"{{"org":"pleme-io","repos":["repo-a","repo-b"],"timestamp":{}}}"#,
            now_secs()
        );
        write_cache_file(dir.path(), "pleme-io", &json);

        let result = read_tend_cache_from(dir.path(), "pleme-io");
        assert_eq!(result, Some(vec!["repo-a".to_string(), "repo-b".to_string()]));
    }

    #[test]
    fn test_read_tend_cache_stale_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        // timestamp older than the 6h TTL
        let stale_timestamp = now_secs().saturating_sub(TEND_CACHE_TTL_SECS + 60);
        let json = format!(
            r#"{{"org":"pleme-io","repos":["repo-a"],"timestamp":{stale_timestamp}}}"#
        );
        write_cache_file(dir.path(), "pleme-io", &json);

        let result = read_tend_cache_from(dir.path(), "pleme-io");
        assert_eq!(result, None, "stale cache entry must be treated as a miss");
    }

    #[test]
    fn test_read_tend_cache_at_ttl_boundary_is_hit() {
        let dir = tempfile::tempdir().unwrap();
        // just inside the TTL window
        let almost_stale = now_secs().saturating_sub(TEND_CACHE_TTL_SECS - 60);
        let json = format!(
            r#"{{"org":"pleme-io","repos":["repo-a"],"timestamp":{almost_stale}}}"#
        );
        write_cache_file(dir.path(), "pleme-io", &json);

        let result = read_tend_cache_from(dir.path(), "pleme-io");
        assert_eq!(result, Some(vec!["repo-a".to_string()]));
    }

    #[test]
    fn test_read_tend_cache_missing_file_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_tend_cache_from(dir.path(), "no-such-owner");
        assert_eq!(result, None);
    }

    #[test]
    fn test_read_tend_cache_malformed_json_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        write_cache_file(dir.path(), "pleme-io", "not valid json!!!");

        let result = read_tend_cache_from(dir.path(), "pleme-io");
        assert_eq!(result, None);
    }

    #[test]
    fn test_read_tend_cache_missing_required_field_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        // valid JSON, but missing `repos` — must fail to deserialize,
        // not silently default to an empty list.
        write_cache_file(dir.path(), "pleme-io", r#"{"org":"pleme-io","timestamp":123}"#);

        let result = read_tend_cache_from(dir.path(), "pleme-io");
        assert_eq!(result, None);
    }

    #[test]
    fn test_read_tend_cache_ignores_extra_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let json = format!(
            r#"{{"org":"pleme-io","repos":["repo-a"],"timestamp":{},"extra_field":"whatever"}}"#,
            now_secs()
        );
        write_cache_file(dir.path(), "pleme-io", &json);

        let result = read_tend_cache_from(dir.path(), "pleme-io");
        assert_eq!(result, Some(vec!["repo-a".to_string()]));
    }

    #[test]
    fn test_read_tend_cache_empty_repos_list() {
        let dir = tempfile::tempdir().unwrap();
        let json = format!(r#"{{"org":"empty-org","repos":[],"timestamp":{}}}"#, now_secs());
        write_cache_file(dir.path(), "empty-org", &json);

        let result = read_tend_cache_from(dir.path(), "empty-org");
        assert_eq!(result, Some(vec![]));
    }

    #[test]
    fn test_tend_discovery_cache_dir_ends_with_tend_discovery() {
        let dir = tend_discovery_cache_dir();
        assert!(dir.ends_with("tend/discovery"), "got: {}", dir.display());
    }

    // ── matches_pattern ─────────────────────────────────────────────────

    #[test]
    fn test_matches_pattern_exact() {
        assert!(matches_pattern("foo", "foo"));
        assert!(!matches_pattern("foo", "bar"));
    }

    #[test]
    fn test_matches_pattern_suffix_wildcard() {
        assert!(matches_pattern("legacy-api", "legacy-*"));
        assert!(matches_pattern("legacy-", "legacy-*"));
        assert!(!matches_pattern("new-api", "legacy-*"));
    }

    #[test]
    fn test_matches_pattern_prefix_wildcard() {
        assert!(matches_pattern("repo.wiki", "*.wiki"));
        assert!(matches_pattern(".wiki", "*.wiki"));
        assert!(!matches_pattern("repo.git", "*.wiki"));
    }

    #[test]
    fn test_matches_pattern_middle_wildcard() {
        assert!(matches_pattern("test-foo-old", "test-*-old"));
        assert!(matches_pattern("test--old", "test-*-old"));
        assert!(!matches_pattern("test-foo-new", "test-*-old"));
    }

    #[test]
    fn test_matches_pattern_star_only() {
        assert!(matches_pattern("anything", "*"));
        assert!(matches_pattern("", "*"));
    }

    #[test]
    fn test_matches_pattern_empty_pattern_only_matches_empty() {
        assert!(matches_pattern("", ""));
        assert!(!matches_pattern("notempty", ""));
    }

    #[test]
    fn test_matches_pattern_multiple_wildcards() {
        assert!(matches_pattern("a-b-c-d", "a-*-*-d"));
        assert!(matches_pattern("a--d", "a-*-d"));
        assert!(matches_pattern("start-mid1-mid2-end", "start-*-*-end"));
        assert!(!matches_pattern("a-b-c-e", "a-*-*-d"));
    }

    #[test]
    fn test_matches_pattern_consecutive_wildcards() {
        assert!(matches_pattern("abc", "**"));
        assert!(matches_pattern("", "**"));
        assert!(matches_pattern("anything-at-all", "***"));
    }

    #[test]
    fn test_matches_pattern_wildcard_at_both_ends() {
        assert!(matches_pattern("xfoox", "*foo*"));
        assert!(matches_pattern("foo", "*foo*"));
        assert!(matches_pattern("foobar", "*foo*"));
        assert!(matches_pattern("barfoo", "*foo*"));
        assert!(!matches_pattern("bar", "*foo*"));
    }

    #[test]
    fn test_matches_pattern_no_partial_match_without_wildcard() {
        assert!(!matches_pattern("foobar", "foo"));
        assert!(!matches_pattern("barfoo", "foo"));
    }

    // ── is_excluded ─────────────────────────────────────────────────────

    #[test]
    fn test_is_excluded() {
        let patterns = vec!["*.wiki".to_string(), "legacy-*".to_string()];
        assert!(is_excluded("repo.wiki", &patterns));
        assert!(is_excluded("legacy-api", &patterns));
        assert!(!is_excluded("codesearch", &patterns));
    }

    #[test]
    fn test_is_excluded_empty_patterns() {
        assert!(!is_excluded("anything", &[]));
    }

    #[test]
    fn test_is_excluded_exact_match() {
        let patterns = vec!["secret-repo".to_string()];
        assert!(is_excluded("secret-repo", &patterns));
        assert!(!is_excluded("secret-repo2", &patterns));
    }

    // ── filter_repos ────────────────────────────────────────────────────

    fn make_repo(name: &str, archived: bool, fork: bool) -> GitHubRepo {
        GitHubRepo {
            name: name.to_string(),
            clone_url: format!("https://github.com/test/{name}.git"),
            archived,
            fork,
        }
    }

    fn make_source(skip_archived: bool, skip_forks: bool, exclude: Vec<String>) -> GitHubSource {
        GitHubSource {
            owner: "testowner".to_string(),
            kind: OwnerKind::Org,
            clone_base: "/tmp/test".to_string(),
            auto_clone: false,
            skip_archived,
            skip_forks,
            exclude,
        }
    }

    #[test]
    fn test_filter_repos_skip_archived() {
        let repos = vec![
            make_repo("active", false, false),
            make_repo("old-stuff", true, false),
        ];
        let source = make_source(true, false, vec![]);
        let filtered = filter_repos(repos, &source);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "active");
    }

    #[test]
    fn test_filter_repos_keep_archived_when_not_skipping() {
        let repos = vec![
            make_repo("active", false, false),
            make_repo("old-stuff", true, false),
        ];
        let source = make_source(false, false, vec![]);
        let filtered = filter_repos(repos, &source);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_repos_skip_forks() {
        let repos = vec![
            make_repo("original", false, false),
            make_repo("forked", false, true),
        ];
        let source = make_source(false, true, vec![]);
        let filtered = filter_repos(repos, &source);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "original");
    }

    #[test]
    fn test_filter_repos_exclude_patterns() {
        let repos = vec![
            make_repo("myapp", false, false),
            make_repo("myapp.wiki", false, false),
            make_repo("legacy-api", false, false),
            make_repo("new-service", false, false),
        ];
        let source = make_source(false, false, vec!["*.wiki".to_string(), "legacy-*".to_string()]);
        let filtered = filter_repos(repos, &source);
        assert_eq!(filtered.len(), 2);
        let names: Vec<_> = filtered.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"myapp"));
        assert!(names.contains(&"new-service"));
    }

    #[test]
    fn test_filter_repos_combined_filters() {
        let repos = vec![
            make_repo("good", false, false),
            make_repo("archived-good", true, false),
            make_repo("forked-good", false, true),
            make_repo("excluded.wiki", false, false),
            make_repo("archived-fork", true, true),
        ];
        let source = make_source(true, true, vec!["*.wiki".to_string()]);
        let filtered = filter_repos(repos, &source);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "good");
    }

    #[test]
    fn test_filter_repos_empty_input() {
        let source = make_source(true, true, vec!["*.wiki".to_string()]);
        let filtered = filter_repos(vec![], &source);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_repos_no_filters_keeps_all() {
        let repos = vec![
            make_repo("a", true, true),
            make_repo("b", false, false),
        ];
        let source = make_source(false, false, vec![]);
        let filtered = filter_repos(repos, &source);
        assert_eq!(filtered.len(), 2);
    }

    // ── resolve_all_repos (no GitHub, explicit-only paths) ──────────────

    #[tokio::test]
    async fn test_resolve_all_repos_no_github_config() {
        let explicit = vec!["/tmp/repo1".to_string(), "/tmp/repo2".to_string()];
        let result = resolve_all_repos(explicit.clone(), None, &None).await;
        assert_eq!(result, explicit);
    }

    #[tokio::test]
    async fn test_resolve_all_repos_empty_sources() {
        let explicit = vec!["/tmp/repo1".to_string()];
        let gh = GitHubConfig {
            token_file: None,
            sources: vec![],
        };
        let result = resolve_all_repos(explicit.clone(), Some(&gh), &None).await;
        assert_eq!(result, explicit);
    }

    #[tokio::test]
    async fn test_resolve_all_repos_empty_explicit() {
        let result = resolve_all_repos(vec![], None, &None).await;
        assert!(result.is_empty());
    }
}
