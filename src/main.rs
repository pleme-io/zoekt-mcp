#![allow(dead_code)]
//! zoekt-mcp — MCP server wrapping Zoekt's HTTP JSON API for Claude Code
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
    let _ = writeln!(
        out,
        "Found {} files",
        result.file_count
    );
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

// ── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("zoekt-mcp starting");

    let server = ZoektMcp::new().serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}
