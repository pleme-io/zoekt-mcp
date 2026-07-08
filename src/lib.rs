//! zoekt-mcp library surface.
//!
//! Exposes the typed Zoekt HTTP client so consumers other than this crate's
//! own MCP binary (e.g. `ponte`, or any future tool that needs zoekt
//! search/list without going through an MCP session) can talk to a local
//! `zoekt-webserver` directly.

pub mod client;
