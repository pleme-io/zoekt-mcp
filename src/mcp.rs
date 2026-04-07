//! MCP server wrapping Zoekt's HTTP JSON API for Claude Code.
//!
//! Tools:
//!   search     — trigram-indexed code search (regex, file:, lang:, sym:, repo: filters)
//!   list_repos — list all indexed repositories with document counts and sizes
//!
//! Environment:
//!   ZOEKT_URL  — Zoekt webserver base URL (default: http://localhost:6070)

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::fmt::Write;

// ── Tool input types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchInput {
    #[schemars(
        description = "Zoekt query. Supports regex and filters: file:pattern lang:rust sym:funcname repo:name case:yes/no branch:name. Examples: 'fn main', 'file:\\.rs$ async fn', 'lang:nix mkDerivation'"
    )]
    query: String,

    #[schemars(description = "Max files to return (default 25)")]
    limit: Option<u32>,

    #[schemars(description = "Context lines around each match (default 2)")]
    context_lines: Option<u32>,

    #[schemars(
        description = "Output mode: \"content\" shows matching lines (supports -A/-B/-C context, -n line numbers, head_limit), \"files_with_matches\" shows file paths (supports head_limit), \"count\" shows match counts (supports head_limit). Defaults to \"files_with_matches\"."
    )]
    output_mode: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListReposInput {
    #[schemars(description = "Optional repo filter query (e.g. 'repo:nexus')")]
    query: Option<String>,
}

// ── Zoekt Search API ────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SearchRequest {
    #[serde(rename = "Q")]
    q: String,
    #[serde(rename = "Opts")]
    #[serde(skip_serializing_if = "Option::is_none")]
    opts: Option<SearchOpts>,
}

#[derive(Serialize)]
struct SearchOpts {
    #[serde(rename = "MaxDocDisplayCount")]
    max_doc_display_count: u32,
    #[serde(rename = "NumContextLines")]
    num_context_lines: u32,
    #[serde(rename = "ChunkMatches")]
    chunk_matches: bool,
    #[serde(rename = "Whole")]
    whole: bool,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(rename = "Result")]
    result: SearchResult,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(rename = "MatchCount")]
    match_count: u64,
    #[serde(rename = "FileCount")]
    file_count: u64,
    #[serde(rename = "Duration")]
    #[serde(default)]
    duration: u64,
    #[serde(rename = "Files")]
    files: Option<Vec<FileMatch>>,
}

#[derive(Deserialize)]
struct FileMatch {
    #[serde(rename = "FileName")]
    file_name: String,
    #[serde(rename = "Repository")]
    #[serde(default)]
    repository: String,
    #[serde(rename = "Language")]
    #[serde(default)]
    language: String,
    #[serde(rename = "Branches")]
    #[serde(default)]
    branches: Vec<String>,
    #[serde(rename = "Version")]
    #[serde(default)]
    version: String,
    #[serde(rename = "ChunkMatches")]
    chunk_matches: Option<Vec<ChunkMatch>>,
    #[serde(rename = "LineMatches")]
    line_matches: Option<Vec<LineMatch>>,
    #[serde(rename = "Content")]
    #[serde(default)]
    content: String,
    #[serde(rename = "Score")]
    #[serde(default)]
    score: f64,
}

#[derive(Deserialize)]
struct ChunkMatch {
    #[serde(rename = "Content")]
    content: String,
    #[serde(rename = "ContentStart")]
    content_start: Location,
    #[serde(rename = "Ranges")]
    #[serde(default)]
    ranges: Vec<Range>,
    #[serde(rename = "SymbolInfo")]
    symbol_info: Option<Vec<Option<SymbolInfo>>>,
    #[serde(rename = "Score")]
    #[serde(default)]
    score: f64,
}

#[derive(Deserialize)]
struct Location {
    #[serde(rename = "ByteOffset")]
    #[serde(default)]
    byte_offset: u32,
    #[serde(rename = "LineNumber")]
    line_number: u32,
    #[serde(rename = "Column")]
    #[serde(default)]
    column: u32,
}

#[derive(Deserialize)]
struct Range {
    #[serde(rename = "Start")]
    start: Location,
    #[serde(rename = "End")]
    end: Location,
}

#[derive(Deserialize)]
struct SymbolInfo {
    #[serde(rename = "Sym")]
    sym: String,
    #[serde(rename = "Kind")]
    #[serde(default)]
    kind: String,
    #[serde(rename = "Parent")]
    #[serde(default)]
    parent: String,
    #[serde(rename = "ParentKind")]
    #[serde(default)]
    parent_kind: String,
}

#[derive(Deserialize)]
struct LineMatch {
    #[serde(rename = "Line")]
    line: String,
    #[serde(rename = "LineNumber")]
    line_number: u32,
    #[serde(rename = "Before")]
    before: Option<String>,
    #[serde(rename = "After")]
    after: Option<String>,
    #[serde(rename = "FileName")]
    #[serde(default)]
    file_name_match: bool,
}

// ── Zoekt List API ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ListRequest {
    #[serde(rename = "Q")]
    q: String,
}

#[derive(Deserialize)]
struct ListResponse {
    #[serde(rename = "List")]
    list: RepoList,
}

#[derive(Deserialize)]
struct RepoList {
    #[serde(rename = "Repos")]
    repos: Option<Vec<RepoEntry>>,
}

#[derive(Deserialize)]
struct RepoEntry {
    #[serde(rename = "Repository")]
    repository: RepoInfo,
    #[serde(rename = "Stats")]
    stats: RepoStats,
}

#[derive(Deserialize)]
struct RepoInfo {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "URL")]
    #[serde(default)]
    url: String,
    #[serde(rename = "Branches")]
    #[serde(default)]
    branches: Vec<BranchInfo>,
    #[serde(rename = "HasSymbols")]
    #[serde(default)]
    has_symbols: bool,
}

#[derive(Deserialize)]
struct BranchInfo {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Version")]
    #[serde(default)]
    version: String,
}

#[derive(Deserialize)]
struct RepoStats {
    #[serde(rename = "Documents")]
    documents: u64,
    #[serde(rename = "ContentBytes")]
    content_bytes: u64,
    #[serde(rename = "IndexBytes")]
    #[serde(default)]
    index_bytes: u64,
}

// ── Base64 helpers ──────────────────────────────────────────────────────────

/// Decode a base64-encoded field, falling back to the raw string if it's plain text.
fn decode_b64(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    B64.decode(s)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| s.to_string())
}

// ── MCP Server ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ZoektMcp {
    client: reqwest::Client,
    base_url: String,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ZoektMcp {
    fn new() -> Self {
        let base_url =
            std::env::var("ZOEKT_URL").unwrap_or_else(|_| "http://localhost:6070".to_string());
        Self {
            client: reqwest::Client::new(),
            base_url,
            tool_router: Self::tool_router(),
        }
    }

    async fn post<Req: Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: &Req,
    ) -> Result<Resp, String> {
        let url = format!("{}{}", self.base_url, endpoint);
        let resp = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("Cannot reach Zoekt at {url}: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Zoekt returned {status}: {text}"));
        }

        resp.json::<Resp>()
            .await
            .map_err(|e| format!("Failed to parse Zoekt response: {e}"))
    }

    #[tool(
        description = "Search code using Zoekt trigram index. Instant results over pre-indexed repositories. Supports regex and query filters: file:pattern lang:rust sym:funcname repo:name case:yes/no branch:name"
    )]
    async fn search(&self, Parameters(input): Parameters<SearchInput>) -> String {
        let mode = input
            .output_mode
            .as_deref()
            .unwrap_or("files_with_matches");
        let context = input.context_lines.unwrap_or(if mode == "content" { 2 } else { 0 });
        let limit = input.limit.unwrap_or(25);

        let req = SearchRequest {
            q: input.query,
            opts: Some(SearchOpts {
                max_doc_display_count: limit,
                num_context_lines: context,
                chunk_matches: mode == "content",
                whole: false,
            }),
        };

        match self.post::<_, SearchResponse>("/api/search", &req).await {
            Ok(parsed) => match mode {
                "content" => format_content(&parsed.result),
                "count" => format_count(&parsed.result),
                _ => format_files(&parsed.result),
            },
            Err(e) => e,
        }
    }

    #[tool(description = "List all repositories indexed by Zoekt with document counts and sizes")]
    async fn list_repos(&self, Parameters(input): Parameters<ListReposInput>) -> String {
        let req = ListRequest {
            q: input.query.unwrap_or_default(),
        };

        match self.post::<_, ListResponse>("/api/list", &req).await {
            Ok(parsed) => format_repos(&parsed),
            Err(e) => e,
        }
    }
}

#[tool_handler]
impl ServerHandler for ZoektMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Zoekt code search — instant trigram-indexed search over local repositories."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

// ── Formatting: content mode (matching lines with context) ──────────────────

fn format_content(result: &SearchResult) -> String {
    let files = result.files.as_deref().unwrap_or_default();
    let mut out = String::with_capacity(4096);
    let _ = writeln!(
        out,
        "{} matches in {} files",
        result.match_count, result.file_count
    );

    for file in files {
        let lang = if file.language.is_empty() {
            String::new()
        } else {
            format!(" ({})", file.language)
        };
        let _ = writeln!(out, "\n--- {}{} ---", file.file_name, lang);

        if let Some(chunks) = &file.chunk_matches {
            for chunk in chunks {
                let content = decode_b64(&chunk.content);
                let start = chunk.content_start.line_number;

                // Show symbol info if present
                if let Some(syms) = &chunk.symbol_info {
                    for sym in syms.iter().flatten() {
                        let parent = if sym.parent.is_empty() {
                            String::new()
                        } else {
                            format!(" in {}", sym.parent)
                        };
                        let kind = if sym.kind.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", sym.kind)
                        };
                        let _ = writeln!(out, "  symbol: {}{}{}", sym.sym, kind, parent);
                    }
                }

                for (i, line) in content.lines().enumerate() {
                    let line_num = start + i as u32;
                    // Mark lines that contain a match range
                    let is_match = chunk.ranges.iter().any(|r| {
                        line_num >= r.start.line_number && line_num <= r.end.line_number
                    });
                    let marker = if is_match { ">" } else { " " };
                    let _ = writeln!(out, "{marker}{line_num}:{line}");
                }
            }
        } else if let Some(lines) = &file.line_matches {
            for m in lines {
                let decoded = decode_b64(&m.line);
                // Show context before
                if let Some(ref b) = m.before {
                    let before = decode_b64(b);
                    if !before.is_empty() {
                        for (i, ctx_line) in before.lines().enumerate() {
                            let num = m.line_number.saturating_sub(
                                before.lines().count() as u32 - i as u32,
                            );
                            let _ = writeln!(out, " {num}:{ctx_line}");
                        }
                    }
                }
                let _ = writeln!(out, ">{}: {}", m.line_number, decoded.trim_end());
                // Show context after
                if let Some(ref a) = m.after {
                    let after = decode_b64(a);
                    if !after.is_empty() {
                        for (i, ctx_line) in after.lines().enumerate() {
                            let num = m.line_number + 1 + i as u32;
                            let _ = writeln!(out, " {num}:{ctx_line}");
                        }
                    }
                }
            }
        }
    }
    out
}

// ── Formatting: files_with_matches mode ─────────────────────────────────────

fn format_files(result: &SearchResult) -> String {
    let files = result.files.as_deref().unwrap_or_default();
    let mut out = String::with_capacity(1024);
    let _ = writeln!(out, "Found {} files", result.file_count);
    for file in files {
        let _ = writeln!(out, "{}", file.file_name);
    }
    out
}

// ── Formatting: count mode ──────────────────────────────────────────────────

fn format_count(result: &SearchResult) -> String {
    let files = result.files.as_deref().unwrap_or_default();
    let mut out = String::with_capacity(1024);
    let _ = writeln!(
        out,
        "{} matches in {} files",
        result.match_count, result.file_count
    );
    for file in files {
        let count = file
            .chunk_matches
            .as_ref()
            .map(|c| c.iter().map(|cm| cm.ranges.len()).sum::<usize>())
            .or_else(|| file.line_matches.as_ref().map(|l| l.len()))
            .unwrap_or(0);
        let _ = writeln!(out, "{}:{}", file.file_name, count);
    }
    out
}

// ── Formatting: repos ───────────────────────────────────────────────────────

fn format_repos(resp: &ListResponse) -> String {
    let repos = resp.list.repos.as_deref().unwrap_or_default();
    let mut out = String::with_capacity(512);
    let _ = writeln!(out, "{} repositories indexed", repos.len());
    for repo in repos {
        let mb = repo.stats.content_bytes as f64 / 1_048_576.0;
        let idx_mb = repo.stats.index_bytes as f64 / 1_048_576.0;
        let symbols = if repo.repository.has_symbols {
            " [symbols]"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {} — {} files, {:.1} MB content, {:.1} MB index{}",
            repo.repository.name, repo.stats.documents, mb, idx_mb, symbols
        );
        for branch in &repo.repository.branches {
            let short_ver = if branch.version.len() > 10 {
                &branch.version[..10]
            } else {
                &branch.version
            };
            let _ = writeln!(out, "    branch: {} ({})", branch.name, short_ver);
        }
    }
    out
}

// ── Entry point ─────────────────────────────────────────────────────────────

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let server = ZoektMcp::new().serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── decode_b64 ──────────────────────────────────────────────────────

    #[test]
    fn test_decode_b64_empty_string() {
        assert_eq!(decode_b64(""), "");
    }

    #[test]
    fn test_decode_b64_valid_base64() {
        let encoded = B64.encode("hello world");
        assert_eq!(decode_b64(&encoded), "hello world");
    }

    #[test]
    fn test_decode_b64_plain_text_fallback() {
        // If a string isn't valid base64, decode_b64 falls back to the raw string.
        // "hello world" is not valid base64 (contains a space).
        assert_eq!(decode_b64("hello world"), "hello world");
    }

    #[test]
    fn test_decode_b64_valid_base64_with_special_chars() {
        let text = "fn main() {\n    println!(\"hello\");\n}";
        let encoded = B64.encode(text);
        assert_eq!(decode_b64(&encoded), text);
    }

    #[test]
    fn test_decode_b64_invalid_utf8_falls_back() {
        // Encode raw bytes that aren't valid UTF-8
        let bad_bytes: [u8; 4] = [0xff, 0xfe, 0xfd, 0xfc];
        let encoded = B64.encode(bad_bytes);
        // decode succeeds as bytes but from_utf8 fails → falls back to raw string
        let result = decode_b64(&encoded);
        assert_eq!(result, encoded);
    }

    // ── format_files ────────────────────────────────────────────────────

    #[test]
    fn test_format_files_empty() {
        let result = SearchResult {
            match_count: 0,
            file_count: 0,
            duration: 0,
            files: None,
        };
        let out = format_files(&result);
        assert!(out.contains("Found 0 files"));
    }

    #[test]
    fn test_format_files_with_results() {
        let result = SearchResult {
            match_count: 5,
            file_count: 2,
            duration: 100,
            files: Some(vec![
                FileMatch {
                    file_name: "src/main.rs".to_string(),
                    repository: "myrepo".to_string(),
                    language: "Rust".to_string(),
                    branches: vec![],
                    version: String::new(),
                    chunk_matches: None,
                    line_matches: None,
                    content: String::new(),
                    score: 1.0,
                },
                FileMatch {
                    file_name: "src/lib.rs".to_string(),
                    repository: "myrepo".to_string(),
                    language: "Rust".to_string(),
                    branches: vec![],
                    version: String::new(),
                    chunk_matches: None,
                    line_matches: None,
                    content: String::new(),
                    score: 0.5,
                },
            ]),
        };
        let out = format_files(&result);
        assert!(out.contains("Found 2 files"));
        assert!(out.contains("src/main.rs"));
        assert!(out.contains("src/lib.rs"));
    }

    #[test]
    fn test_format_files_empty_vec() {
        let result = SearchResult {
            match_count: 0,
            file_count: 0,
            duration: 0,
            files: Some(vec![]),
        };
        let out = format_files(&result);
        assert!(out.contains("Found 0 files"));
    }

    // ── format_count ────────────────────────────────────────────────────

    #[test]
    fn test_format_count_empty() {
        let result = SearchResult {
            match_count: 0,
            file_count: 0,
            duration: 0,
            files: None,
        };
        let out = format_count(&result);
        assert!(out.contains("0 matches in 0 files"));
    }

    #[test]
    fn test_format_count_with_chunk_matches() {
        let result = SearchResult {
            match_count: 3,
            file_count: 1,
            duration: 50,
            files: Some(vec![FileMatch {
                file_name: "src/main.rs".to_string(),
                repository: String::new(),
                language: String::new(),
                branches: vec![],
                version: String::new(),
                chunk_matches: Some(vec![
                    ChunkMatch {
                        content: B64.encode("line1\nline2"),
                        content_start: Location { byte_offset: 0, line_number: 1, column: 0 },
                        ranges: vec![
                            Range {
                                start: Location { byte_offset: 0, line_number: 1, column: 0 },
                                end: Location { byte_offset: 5, line_number: 1, column: 5 },
                            },
                            Range {
                                start: Location { byte_offset: 6, line_number: 2, column: 0 },
                                end: Location { byte_offset: 11, line_number: 2, column: 5 },
                            },
                        ],
                        symbol_info: None,
                        score: 1.0,
                    },
                ]),
                line_matches: None,
                content: String::new(),
                score: 1.0,
            }]),
        };
        let out = format_count(&result);
        assert!(out.contains("3 matches in 1 files"));
        assert!(out.contains("src/main.rs:2"));
    }

    #[test]
    fn test_format_count_with_line_matches() {
        let result = SearchResult {
            match_count: 2,
            file_count: 1,
            duration: 50,
            files: Some(vec![FileMatch {
                file_name: "test.py".to_string(),
                repository: String::new(),
                language: String::new(),
                branches: vec![],
                version: String::new(),
                chunk_matches: None,
                line_matches: Some(vec![
                    LineMatch {
                        line: B64.encode("import foo"),
                        line_number: 1,
                        before: None,
                        after: None,
                        file_name_match: false,
                    },
                    LineMatch {
                        line: B64.encode("import bar"),
                        line_number: 5,
                        before: None,
                        after: None,
                        file_name_match: false,
                    },
                ]),
                content: String::new(),
                score: 1.0,
            }]),
        };
        let out = format_count(&result);
        assert!(out.contains("test.py:2"));
    }

    #[test]
    fn test_format_count_no_matches_in_file() {
        let result = SearchResult {
            match_count: 0,
            file_count: 1,
            duration: 0,
            files: Some(vec![FileMatch {
                file_name: "empty.rs".to_string(),
                repository: String::new(),
                language: String::new(),
                branches: vec![],
                version: String::new(),
                chunk_matches: None,
                line_matches: None,
                content: String::new(),
                score: 0.0,
            }]),
        };
        let out = format_count(&result);
        assert!(out.contains("empty.rs:0"));
    }

    // ── format_repos ────────────────────────────────────────────────────

    #[test]
    fn test_format_repos_empty() {
        let resp = ListResponse {
            list: RepoList { repos: None },
        };
        let out = format_repos(&resp);
        assert!(out.contains("0 repositories indexed"));
    }

    #[test]
    fn test_format_repos_with_data() {
        let resp = ListResponse {
            list: RepoList {
                repos: Some(vec![
                    RepoEntry {
                        repository: RepoInfo {
                            name: "myrepo".to_string(),
                            url: "https://github.com/test/myrepo".to_string(),
                            branches: vec![
                                BranchInfo {
                                    name: "main".to_string(),
                                    version: "abc123def456".to_string(),
                                },
                            ],
                            has_symbols: true,
                        },
                        stats: RepoStats {
                            documents: 150,
                            content_bytes: 5_242_880, // 5 MB
                            index_bytes: 2_621_440,   // 2.5 MB
                        },
                    },
                ]),
            },
        };
        let out = format_repos(&resp);
        assert!(out.contains("1 repositories indexed"));
        assert!(out.contains("myrepo"));
        assert!(out.contains("150 files"));
        assert!(out.contains("5.0 MB content"));
        assert!(out.contains("2.5 MB index"));
        assert!(out.contains("[symbols]"));
        assert!(out.contains("branch: main"));
        // Version should be truncated to 10 chars
        assert!(out.contains("abc123def4"));
    }

    #[test]
    fn test_format_repos_no_symbols() {
        let resp = ListResponse {
            list: RepoList {
                repos: Some(vec![
                    RepoEntry {
                        repository: RepoInfo {
                            name: "nosyms".to_string(),
                            url: String::new(),
                            branches: vec![],
                            has_symbols: false,
                        },
                        stats: RepoStats {
                            documents: 10,
                            content_bytes: 1024,
                            index_bytes: 512,
                        },
                    },
                ]),
            },
        };
        let out = format_repos(&resp);
        assert!(!out.contains("[symbols]"));
    }

    #[test]
    fn test_format_repos_short_version_not_truncated() {
        let resp = ListResponse {
            list: RepoList {
                repos: Some(vec![
                    RepoEntry {
                        repository: RepoInfo {
                            name: "r".to_string(),
                            url: String::new(),
                            branches: vec![
                                BranchInfo {
                                    name: "main".to_string(),
                                    version: "short".to_string(),
                                },
                            ],
                            has_symbols: false,
                        },
                        stats: RepoStats {
                            documents: 1,
                            content_bytes: 0,
                            index_bytes: 0,
                        },
                    },
                ]),
            },
        };
        let out = format_repos(&resp);
        assert!(out.contains("(short)"));
    }

    // ── format_content ──────────────────────────────────────────────────

    #[test]
    fn test_format_content_empty() {
        let result = SearchResult {
            match_count: 0,
            file_count: 0,
            duration: 0,
            files: None,
        };
        let out = format_content(&result);
        assert!(out.contains("0 matches in 0 files"));
    }

    #[test]
    fn test_format_content_with_chunk_matches() {
        let result = SearchResult {
            match_count: 1,
            file_count: 1,
            duration: 10,
            files: Some(vec![FileMatch {
                file_name: "src/lib.rs".to_string(),
                repository: "testrepo".to_string(),
                language: "Rust".to_string(),
                branches: vec![],
                version: String::new(),
                chunk_matches: Some(vec![ChunkMatch {
                    content: B64.encode("fn hello() {\n    println!(\"hi\");\n}"),
                    content_start: Location { byte_offset: 0, line_number: 10, column: 0 },
                    ranges: vec![Range {
                        start: Location { byte_offset: 0, line_number: 10, column: 0 },
                        end: Location { byte_offset: 12, line_number: 10, column: 12 },
                    }],
                    symbol_info: None,
                    score: 1.0,
                }]),
                line_matches: None,
                content: String::new(),
                score: 1.0,
            }]),
        };
        let out = format_content(&result);
        assert!(out.contains("1 matches in 1 files"));
        assert!(out.contains("--- src/lib.rs (Rust) ---"));
        assert!(out.contains(">10:fn hello()"));
        // Non-match lines should have space prefix
        assert!(out.contains(" 11:"));
        assert!(out.contains(" 12:"));
    }

    #[test]
    fn test_format_content_with_symbol_info() {
        let result = SearchResult {
            match_count: 1,
            file_count: 1,
            duration: 10,
            files: Some(vec![FileMatch {
                file_name: "main.go".to_string(),
                repository: String::new(),
                language: String::new(),
                branches: vec![],
                version: String::new(),
                chunk_matches: Some(vec![ChunkMatch {
                    content: B64.encode("func Run()"),
                    content_start: Location { byte_offset: 0, line_number: 1, column: 0 },
                    ranges: vec![Range {
                        start: Location { byte_offset: 0, line_number: 1, column: 0 },
                        end: Location { byte_offset: 10, line_number: 1, column: 10 },
                    }],
                    symbol_info: Some(vec![Some(SymbolInfo {
                        sym: "Run".to_string(),
                        kind: "function".to_string(),
                        parent: "main".to_string(),
                        parent_kind: "package".to_string(),
                    })]),
                    score: 1.0,
                }]),
                line_matches: None,
                content: String::new(),
                score: 1.0,
            }]),
        };
        let out = format_content(&result);
        assert!(out.contains("symbol: Run [function] in main"));
    }

    #[test]
    fn test_format_content_symbol_no_parent_no_kind() {
        let result = SearchResult {
            match_count: 1,
            file_count: 1,
            duration: 0,
            files: Some(vec![FileMatch {
                file_name: "test.rs".to_string(),
                repository: String::new(),
                language: String::new(),
                branches: vec![],
                version: String::new(),
                chunk_matches: Some(vec![ChunkMatch {
                    content: B64.encode("x"),
                    content_start: Location { byte_offset: 0, line_number: 1, column: 0 },
                    ranges: vec![],
                    symbol_info: Some(vec![Some(SymbolInfo {
                        sym: "Foo".to_string(),
                        kind: String::new(),
                        parent: String::new(),
                        parent_kind: String::new(),
                    })]),
                    score: 0.0,
                }]),
                line_matches: None,
                content: String::new(),
                score: 0.0,
            }]),
        };
        let out = format_content(&result);
        assert!(out.contains("symbol: Foo\n"));
        // No kind bracket and no parent " in " should appear in the symbol line
        let sym_line = out.lines().find(|l| l.contains("symbol:")).unwrap();
        assert!(!sym_line.contains("["), "kind bracket should be absent: {sym_line}");
        assert!(!sym_line.contains(" in "), "parent should be absent: {sym_line}");
    }

    #[test]
    fn test_format_content_with_line_matches() {
        let result = SearchResult {
            match_count: 1,
            file_count: 1,
            duration: 0,
            files: Some(vec![FileMatch {
                file_name: "script.py".to_string(),
                repository: String::new(),
                language: "Python".to_string(),
                branches: vec![],
                version: String::new(),
                chunk_matches: None,
                line_matches: Some(vec![LineMatch {
                    line: B64.encode("import os"),
                    line_number: 3,
                    before: Some(B64.encode("# header\n# comment")),
                    after: Some(B64.encode("import sys")),
                    file_name_match: false,
                }]),
                content: String::new(),
                score: 1.0,
            }]),
        };
        let out = format_content(&result);
        assert!(out.contains("--- script.py (Python) ---"));
        assert!(out.contains(">3: import os"));
        // Context after
        assert!(out.contains("4:import sys"));
    }

    #[test]
    fn test_format_content_line_match_no_context() {
        let result = SearchResult {
            match_count: 1,
            file_count: 1,
            duration: 0,
            files: Some(vec![FileMatch {
                file_name: "a.txt".to_string(),
                repository: String::new(),
                language: String::new(),
                branches: vec![],
                version: String::new(),
                chunk_matches: None,
                line_matches: Some(vec![LineMatch {
                    line: B64.encode("match line"),
                    line_number: 1,
                    before: None,
                    after: None,
                    file_name_match: false,
                }]),
                content: String::new(),
                score: 0.0,
            }]),
        };
        let out = format_content(&result);
        assert!(out.contains(">1: match line"));
    }

    #[test]
    fn test_format_content_no_language() {
        let result = SearchResult {
            match_count: 0,
            file_count: 1,
            duration: 0,
            files: Some(vec![FileMatch {
                file_name: "Makefile".to_string(),
                repository: String::new(),
                language: String::new(),
                branches: vec![],
                version: String::new(),
                chunk_matches: None,
                line_matches: None,
                content: String::new(),
                score: 0.0,
            }]),
        };
        let out = format_content(&result);
        // No language → no parenthetical
        assert!(out.contains("--- Makefile ---"));
        assert!(!out.contains("()"));
    }

    // ── Serde round-trip tests for API types ────────────────────────────

    #[test]
    fn test_search_request_serialization() {
        let req = SearchRequest {
            q: "fn main".to_string(),
            opts: Some(SearchOpts {
                max_doc_display_count: 10,
                num_context_lines: 2,
                chunk_matches: true,
                whole: false,
            }),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"Q\":\"fn main\""));
        assert!(json.contains("\"MaxDocDisplayCount\":10"));
        assert!(json.contains("\"NumContextLines\":2"));
        assert!(json.contains("\"ChunkMatches\":true"));
        assert!(json.contains("\"Whole\":false"));
    }

    #[test]
    fn test_search_request_without_opts() {
        let req = SearchRequest {
            q: "test".to_string(),
            opts: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"Q\":\"test\""));
        assert!(!json.contains("Opts"));
    }

    #[test]
    fn test_list_request_serialization() {
        let req = ListRequest {
            q: "repo:test".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"Q\":\"repo:test\""));
    }

    #[test]
    fn test_search_response_deserialization() {
        let json = r#"{
            "Result": {
                "MatchCount": 42,
                "FileCount": 5,
                "Duration": 1000,
                "Files": null
            }
        }"#;
        let resp: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result.match_count, 42);
        assert_eq!(resp.result.file_count, 5);
        assert_eq!(resp.result.duration, 1000);
        assert!(resp.result.files.is_none());
    }

    #[test]
    fn test_search_response_missing_duration_defaults() {
        let json = r#"{
            "Result": {
                "MatchCount": 1,
                "FileCount": 1,
                "Files": []
            }
        }"#;
        let resp: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result.duration, 0);
        assert!(resp.result.files.unwrap().is_empty());
    }

    #[test]
    fn test_list_response_deserialization() {
        let json = r#"{
            "List": {
                "Repos": [
                    {
                        "Repository": {
                            "Name": "testrepo",
                            "URL": "https://github.com/test/repo",
                            "Branches": [
                                {"Name": "main", "Version": "abc123"}
                            ],
                            "HasSymbols": true
                        },
                        "Stats": {
                            "Documents": 100,
                            "ContentBytes": 1048576,
                            "IndexBytes": 524288
                        }
                    }
                ]
            }
        }"#;
        let resp: ListResponse = serde_json::from_str(json).unwrap();
        let repos = resp.list.repos.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].repository.name, "testrepo");
        assert!(repos[0].repository.has_symbols);
        assert_eq!(repos[0].repository.branches.len(), 1);
        assert_eq!(repos[0].repository.branches[0].name, "main");
        assert_eq!(repos[0].stats.documents, 100);
        assert_eq!(repos[0].stats.content_bytes, 1_048_576);
    }

    #[test]
    fn test_list_response_null_repos() {
        let json = r#"{"List": {"Repos": null}}"#;
        let resp: ListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.list.repos.is_none());
    }

    #[test]
    fn test_file_match_deserialization_with_defaults() {
        let json = r#"{
            "FileName": "test.rs",
            "ChunkMatches": null
        }"#;
        let fm: FileMatch = serde_json::from_str(json).unwrap();
        assert_eq!(fm.file_name, "test.rs");
        assert_eq!(fm.repository, "");
        assert_eq!(fm.language, "");
        assert!(fm.branches.is_empty());
        assert_eq!(fm.version, "");
        assert!(fm.chunk_matches.is_none());
        assert!(fm.line_matches.is_none());
        assert_eq!(fm.content, "");
        assert_eq!(fm.score, 0.0);
    }

    #[test]
    fn test_chunk_match_deserialization() {
        let json = r#"{
            "Content": "aGVsbG8=",
            "ContentStart": {"ByteOffset": 0, "LineNumber": 1, "Column": 0},
            "Ranges": [
                {
                    "Start": {"ByteOffset": 0, "LineNumber": 1, "Column": 0},
                    "End": {"ByteOffset": 5, "LineNumber": 1, "Column": 5}
                }
            ],
            "SymbolInfo": null
        }"#;
        let cm: ChunkMatch = serde_json::from_str(json).unwrap();
        assert_eq!(cm.content, "aGVsbG8=");
        assert_eq!(cm.content_start.line_number, 1);
        assert_eq!(cm.ranges.len(), 1);
        assert_eq!(cm.ranges[0].start.column, 0);
        assert_eq!(cm.ranges[0].end.column, 5);
        assert!(cm.symbol_info.is_none());
    }

    #[test]
    fn test_line_match_deserialization() {
        let json = r#"{
            "Line": "aW1wb3J0IG9z",
            "LineNumber": 1,
            "Before": null,
            "After": null
        }"#;
        let lm: LineMatch = serde_json::from_str(json).unwrap();
        assert_eq!(lm.line, "aW1wb3J0IG9z");
        assert_eq!(lm.line_number, 1);
        assert!(lm.before.is_none());
        assert!(lm.after.is_none());
        assert!(!lm.file_name_match);
    }
}
