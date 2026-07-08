//! Typed HTTP client for Zoekt's JSON API.
//!
//! Talks to a local `zoekt-webserver` (or the `zoekt-mcp daemon` that spawns
//! one) over plain HTTP — the same `/api/search` + `/api/list` endpoints this
//! crate's MCP tool layer (`crate::mcp`) calls, extracted into a `pub`
//! surface so a standalone consumer (no MCP session) can use it too.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Default request timeout. Zoekt queries are normally sub-second; this
/// guards against a wedged daemon blocking a caller indefinitely — the
/// client this was extracted from built `reqwest::Client::new()` with no
/// timeout at all.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub const DEFAULT_BASE_URL: &str = "http://localhost:6070";

// ── Client ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ZoektClient {
    http: reqwest::Client,
    base_url: String,
}

impl ZoektClient {
    /// Build a client against an explicit base URL, with the default bounded timeout.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_timeout(base_url, DEFAULT_TIMEOUT)
    }

    /// Build a client with an explicit timeout, for callers that want a
    /// tighter or looser bound than [`DEFAULT_TIMEOUT`].
    pub fn with_timeout(base_url: impl Into<String>, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client build with a fixed timeout cannot fail");
        Self {
            http,
            base_url: base_url.into(),
        }
    }

    /// Build a client from the `ZOEKT_URL` env var, falling back to
    /// [`DEFAULT_BASE_URL`].
    pub fn from_env() -> Self {
        let base_url =
            std::env::var("ZOEKT_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self::new(base_url)
    }

    async fn post<Req: Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: &Req,
    ) -> Result<Resp, String> {
        let url = format!("{}{}", self.base_url, endpoint);
        let resp = self
            .http
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

    pub async fn search(&self, req: &SearchRequest) -> Result<SearchResponse, String> {
        self.post("/api/search", req).await
    }

    pub async fn list_repos(&self, req: &ListRequest) -> Result<ListResponse, String> {
        self.post("/api/list", req).await
    }
}

// ── Base64 helper ────────────────────────────────────────────────────────

/// Decode a base64-encoded field, falling back to the raw string if it's plain text.
pub fn decode_b64(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    B64.decode(s)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| s.to_string())
}

// ── Zoekt Search API ─────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SearchRequest {
    #[serde(rename = "Q")]
    pub q: String,
    #[serde(rename = "Opts")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opts: Option<SearchOpts>,
}

#[derive(Serialize)]
pub struct SearchOpts {
    #[serde(rename = "MaxDocDisplayCount")]
    pub max_doc_display_count: u32,
    #[serde(rename = "NumContextLines")]
    pub num_context_lines: u32,
    #[serde(rename = "ChunkMatches")]
    pub chunk_matches: bool,
    #[serde(rename = "Whole")]
    pub whole: bool,
}

#[derive(Deserialize)]
pub struct SearchResponse {
    #[serde(rename = "Result")]
    pub result: SearchResult,
}

#[derive(Deserialize)]
pub struct SearchResult {
    #[serde(rename = "MatchCount")]
    pub match_count: u64,
    #[serde(rename = "FileCount")]
    pub file_count: u64,
    #[serde(rename = "Duration")]
    #[serde(default)]
    pub duration: u64,
    #[serde(rename = "Files")]
    pub files: Option<Vec<FileMatch>>,
}

#[derive(Deserialize)]
pub struct FileMatch {
    #[serde(rename = "FileName")]
    pub file_name: String,
    #[serde(rename = "Repository")]
    #[serde(default)]
    pub repository: String,
    #[serde(rename = "Language")]
    #[serde(default)]
    pub language: String,
    #[serde(rename = "Branches")]
    #[serde(default)]
    pub branches: Vec<String>,
    #[serde(rename = "Version")]
    #[serde(default)]
    pub version: String,
    #[serde(rename = "ChunkMatches")]
    pub chunk_matches: Option<Vec<ChunkMatch>>,
    #[serde(rename = "LineMatches")]
    pub line_matches: Option<Vec<LineMatch>>,
    #[serde(rename = "Content")]
    #[serde(default)]
    pub content: String,
    #[serde(rename = "Score")]
    #[serde(default)]
    pub score: f64,
}

#[derive(Deserialize)]
pub struct ChunkMatch {
    #[serde(rename = "Content")]
    pub content: String,
    #[serde(rename = "ContentStart")]
    pub content_start: Location,
    #[serde(rename = "Ranges")]
    #[serde(default)]
    pub ranges: Vec<Range>,
    #[serde(rename = "SymbolInfo")]
    pub symbol_info: Option<Vec<Option<SymbolInfo>>>,
    #[serde(rename = "Score")]
    #[serde(default)]
    pub score: f64,
}

#[derive(Deserialize)]
pub struct Location {
    #[serde(rename = "ByteOffset")]
    #[serde(default)]
    pub byte_offset: u32,
    #[serde(rename = "LineNumber")]
    pub line_number: u32,
    #[serde(rename = "Column")]
    #[serde(default)]
    pub column: u32,
}

#[derive(Deserialize)]
pub struct Range {
    #[serde(rename = "Start")]
    pub start: Location,
    #[serde(rename = "End")]
    pub end: Location,
}

#[derive(Deserialize)]
pub struct SymbolInfo {
    #[serde(rename = "Sym")]
    pub sym: String,
    #[serde(rename = "Kind")]
    #[serde(default)]
    pub kind: String,
    #[serde(rename = "Parent")]
    #[serde(default)]
    pub parent: String,
    #[serde(rename = "ParentKind")]
    #[serde(default)]
    pub parent_kind: String,
}

#[derive(Deserialize)]
pub struct LineMatch {
    #[serde(rename = "Line")]
    pub line: String,
    #[serde(rename = "LineNumber")]
    pub line_number: u32,
    #[serde(rename = "Before")]
    pub before: Option<String>,
    #[serde(rename = "After")]
    pub after: Option<String>,
    #[serde(rename = "FileName")]
    #[serde(default)]
    pub file_name_match: bool,
}

// ── Zoekt List API ───────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ListRequest {
    #[serde(rename = "Q")]
    pub q: String,
}

#[derive(Deserialize)]
pub struct ListResponse {
    #[serde(rename = "List")]
    pub list: RepoList,
}

#[derive(Deserialize)]
pub struct RepoList {
    #[serde(rename = "Repos")]
    pub repos: Option<Vec<RepoEntry>>,
}

#[derive(Deserialize)]
pub struct RepoEntry {
    #[serde(rename = "Repository")]
    pub repository: RepoInfo,
    #[serde(rename = "Stats")]
    pub stats: RepoStats,
}

#[derive(Deserialize)]
pub struct RepoInfo {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "URL")]
    #[serde(default)]
    pub url: String,
    #[serde(rename = "Branches")]
    #[serde(default)]
    pub branches: Vec<BranchInfo>,
    #[serde(rename = "HasSymbols")]
    #[serde(default)]
    pub has_symbols: bool,
}

#[derive(Deserialize)]
pub struct BranchInfo {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Version")]
    #[serde(default)]
    pub version: String,
}

#[derive(Deserialize)]
pub struct RepoStats {
    #[serde(rename = "Documents")]
    pub documents: u64,
    #[serde(rename = "ContentBytes")]
    pub content_bytes: u64,
    #[serde(rename = "IndexBytes")]
    #[serde(default)]
    pub index_bytes: u64,
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

    // ── ZoektClient construction ────────────────────────────────────────

    #[test]
    fn test_client_has_bounded_timeout_by_default() {
        // Regression guard for the extraction's whole point: the client this
        // was extracted from built `reqwest::Client::new()` with NO timeout,
        // so a wedged daemon blocked a caller indefinitely. `new()` must go
        // through `with_timeout` with a real, non-zero bound.
        assert!(DEFAULT_TIMEOUT.as_secs() > 0);
        let _client = ZoektClient::new(DEFAULT_BASE_URL);
        // Construction succeeding (no panic building the reqwest::Client with
        // a fixed timeout) is the property under test; reqwest doesn't expose
        // the configured timeout back for a direct assertion.
    }

    #[test]
    fn test_from_env_defaults_when_unset() {
        // SAFETY: test-only env mutation, no concurrent access in this crate's
        // single-threaded test harness for this var.
        unsafe {
            std::env::remove_var("ZOEKT_URL");
        }
        let _client = ZoektClient::from_env();
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
