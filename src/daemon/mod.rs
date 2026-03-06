//! Daemon mode: manages zoekt-webserver + periodic zoekt-git-index.
//!
//! Reads a YAML config file and orchestrates the Go binaries as child processes,
//! mirroring the codesearch daemon architecture.
//!
//! ```yaml
//! port: 6070
//! index_dir: ~/.zoekt/index
//! index_interval: 300
//! repos:
//!   - ~/code/myrepo
//! ```

pub mod config;
pub mod github;

use std::time::Duration;

use config::DaemonConfig;
use tokio::process::{Child, Command};
use tracing::{error, info, warn};

/// Main daemon entry point.
pub async fn run(mut config: DaemonConfig) -> anyhow::Result<()> {
    config.expand_paths();

    info!("Starting zoekt daemon on port {}", config.port);
    info!("Index dir: {}", config.index_dir);
    info!("Index interval: {}s", config.index_interval);

    // Create directories
    tokio::fs::create_dir_all(&config.index_dir).await?;
    if let Some(ref log_dir) = config.webserver.log_dir {
        tokio::fs::create_dir_all(log_dir).await?;
    }

    let path = config.build_path();
    info!("PATH: {}", path);

    // Resolve repos (explicit + GitHub discovery)
    let repos = github::resolve_all_repos(
        config.repos.clone(),
        config.github.as_ref(),
        &config.git_bin,
    )
    .await;

    if repos.is_empty() {
        return Err(anyhow::anyhow!(
            "No repos to manage. Configure repos or github.sources in the config file."
        ));
    }

    info!("Managing {} repos", repos.len());

    // Start zoekt-webserver as long-running child
    let mut webserver = start_webserver(&config, &path)?;
    info!("zoekt-webserver started (pid: {:?})", webserver.id());

    // Initial index run
    info!("Running initial index...");
    run_indexer(&config, &path, &repos).await;

    // Periodic loop: re-discover repos + re-index
    let interval = Duration::from_secs(config.index_interval);
    let mut timer = tokio::time::interval(interval);
    timer.tick().await; // skip first tick (just indexed)

    loop {
        tokio::select! {
            _ = timer.tick() => {
                // Check if webserver is still alive, restart if crashed
                match webserver.try_wait() {
                    Ok(Some(status)) => {
                        warn!("zoekt-webserver exited with {}, restarting...", status);
                        match start_webserver(&config, &path) {
                            Ok(child) => {
                                webserver = child;
                                info!("zoekt-webserver restarted (pid: {:?})", webserver.id());
                            }
                            Err(e) => error!("Failed to restart webserver: {}", e),
                        }
                    }
                    Ok(None) => {} // still running
                    Err(e) => warn!("Failed to check webserver status: {}", e),
                }

                // Re-discover repos (picks up new GitHub repos)
                let repos = github::resolve_all_repos(
                    config.repos.clone(),
                    config.github.as_ref(),
                    &config.git_bin,
                ).await;

                if !repos.is_empty() {
                    info!("Periodic re-index ({} repos)...", repos.len());
                    run_indexer(&config, &path, &repos).await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Received shutdown signal, stopping...");
                let _ = webserver.kill().await;
                info!("zoekt-webserver stopped");
                return Ok(());
            }
        }
    }
}

/// Start zoekt-webserver as a child process.
fn start_webserver(config: &DaemonConfig, path: &str) -> anyhow::Result<Child> {
    let bin = config.webserver_bin();
    let args = config.webserver_args();

    info!("Starting: {} {}", bin.display(), args.join(" "));

    let child = Command::new(&bin)
        .args(&args)
        .env("PATH", path)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to start zoekt-webserver ({}): {}", bin.display(), e))?;

    Ok(child)
}

/// Run zoekt-git-index once with the given repo list.
async fn run_indexer(config: &DaemonConfig, path: &str, repos: &[String]) {
    let bin = config.indexer_bin();
    let mut args = config.indexer_args();

    // Append repo paths
    args.extend(repos.iter().cloned());

    info!(
        "Running: {} {} [{} repos]",
        bin.display(),
        config.indexer_args().join(" "),
        repos.len()
    );

    match Command::new(&bin)
        .args(&args)
        .env("PATH", path)
        .status()
        .await
    {
        Ok(status) if status.success() => {
            info!("Index complete");
        }
        Ok(status) => {
            error!("zoekt-git-index exited with {}", status);
        }
        Err(e) => {
            error!("Failed to run zoekt-git-index ({}): {}", bin.display(), e);
        }
    }
}

/// Validate that required binaries exist on the configured PATH.
pub fn validate_binaries(config: &DaemonConfig) -> anyhow::Result<()> {
    let webserver = config.webserver_bin();
    let indexer = config.indexer_bin();

    if !webserver.exists() && config.zoekt_bin.is_some() {
        return Err(anyhow::anyhow!(
            "zoekt-webserver not found at {}",
            webserver.display()
        ));
    }
    if !indexer.exists() && config.zoekt_bin.is_some() {
        return Err(anyhow::anyhow!(
            "zoekt-git-index not found at {}",
            indexer.display()
        ));
    }

    Ok(())
}
