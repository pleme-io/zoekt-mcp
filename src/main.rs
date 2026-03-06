#![allow(dead_code)]

mod daemon;
mod mcp;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(name = "zoekt-mcp", about = "MCP server + daemon for Zoekt code search")]
enum Cli {
    /// Run as daemon managing zoekt-webserver + periodic indexer
    Daemon {
        /// Path to YAML config file
        #[arg(long)]
        config: PathBuf,
    },
    /// Run as MCP server (default)
    Mcp,
}

impl Default for Cli {
    fn default() -> Self {
        Cli::Mcp
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // If invoked with no args, default to MCP mode (backwards-compatible)
    let cli = if std::env::args().len() <= 1 {
        Cli::Mcp
    } else {
        Cli::parse()
    };

    match cli {
        Cli::Daemon { config } => {
            // File + console logging for long-running daemon
            let log_dir = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("Library/Logs");
            let file_appender =
                tracing_appender::rolling::daily(&log_dir, "zoekt-daemon.log");
            let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "zoekt_mcp=info".into()),
                )
                .with_writer(non_blocking)
                .with_ansi(false)
                .init();

            tracing::info!("zoekt-mcp daemon starting");

            let daemon_config = daemon::config::DaemonConfig::load(&config)?;
            daemon::run(daemon_config).await?;
        }
        Cli::Mcp => {
            // Stderr-only tracing for MCP mode (stdout is for MCP protocol)
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_writer(std::io::stderr)
                .init();

            tracing::info!("zoekt-mcp starting");
            mcp::run().await?;
        }
    }

    Ok(())
}
