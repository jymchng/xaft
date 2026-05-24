//! Hot-reload: file watcher that re-loads config on change.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::loader::ConfigLoader;
use crate::types::{CliOverrides, XaftConfig};

/// Polls config files every `poll_interval` and re-loads when mtime changes.
///
/// Sends updated `XaftConfig` values to the returned `Receiver`.
///
/// # Limitations
///
/// Uses polling rather than OS-level file events to avoid the `notify` crate.
/// Default poll interval is 2 seconds which is acceptable for config changes.
pub struct ConfigWatcher {
    paths: Vec<PathBuf>,
    last_modified: HashMap<PathBuf, SystemTime>,
    tx: watch::Sender<XaftConfig>,
    overrides: CliOverrides,
}

impl ConfigWatcher {
    /// Spawn the watcher.
    ///
    /// Returns `(receiver, task_handle)`. The receiver yields new config values
    /// whenever a watched file changes. Drop the handle to stop watching.
    pub fn spawn(
        initial_config: XaftConfig,
        paths: Vec<PathBuf>,
        overrides: CliOverrides,
        poll_interval: Duration,
    ) -> (watch::Receiver<XaftConfig>, JoinHandle<()>) {
        let (tx, rx) = watch::channel(initial_config);

        let mut watcher = Self {
            paths,
            last_modified: HashMap::new(),
            tx,
            overrides,
        };

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(poll_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                if watcher.tx.is_closed() {
                    break;
                }

                if watcher.check_for_changes() {
                    match ConfigLoader::load(&watcher.overrides) {
                        Ok(new_config) => {
                            info!("Config hot-reloaded");
                            let _ = watcher.tx.send(new_config);
                        }
                        Err(e) => {
                            warn!("Hot-reload failed (keeping previous config): {}", e);
                        }
                    }
                }
            }
        });

        (rx, handle)
    }

    /// Check all watched paths for mtime changes. Returns `true` if any changed.
    fn check_for_changes(&mut self) -> bool {
        let mut changed = false;

        for path in &self.paths {
            if !path.exists() {
                continue;
            }

            let modified = match std::fs::metadata(path)
                .and_then(|m| m.modified())
            {
                Ok(t) => t,
                Err(_) => continue,
            };

            match self.last_modified.get(path) {
                None => {
                    // First time we've seen this file
                    self.last_modified.insert(path.clone(), modified);
                }
                Some(&prev) if prev != modified => {
                    info!("Config file changed: {}", path.display());
                    self.last_modified.insert(path.clone(), modified);
                    changed = true;
                }
                _ => {}
            }
        }

        changed
    }
}

/// Paths that should be watched for a given `CliOverrides`.
///
/// Returns the ordered list: user global, project, session overrides.
pub fn watched_paths(overrides: &CliOverrides) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Explicit config file
    if let Some(ref path) = overrides.config_file {
        paths.push(path.clone());
        return paths; // explicit path only
    }

    // User global config
    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("xaft/xaft.toml"));
    }

    // Project config (walk up from cwd)
    if let Ok(dir) = overrides
        .project_dir
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
    {
        let mut current = dir;
        loop {
            let candidate = current.join(".xaft/xaft.toml");
            if candidate.exists() {
                paths.push(candidate);
                break;
            }
            if !current.pop() {
                break;
            }
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn watcher_detects_file_change() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("xaft.toml");

        // Write initial config
        fs::write(
            &config_path,
            r#"
[core]
log_level = "info"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#,
        )
        .unwrap();

        let initial = XaftConfig::default();
        let paths = vec![config_path.clone()];
        let overrides = CliOverrides {
            config_file: Some(config_path.clone()),
            ..Default::default()
        };

        let (mut rx, _handle) =
            ConfigWatcher::spawn(initial, paths, overrides, Duration::from_millis(50));

        // Modify the file after a brief delay
        tokio::time::sleep(Duration::from_millis(30)).await;
        fs::write(
            &config_path,
            r#"
[core]
log_level = "debug"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#,
        )
        .unwrap();

        // Wait for the watcher to pick up the change
        let changed = tokio::time::timeout(Duration::from_millis(500), async {
            rx.changed().await.is_ok()
        })
        .await;

        assert!(changed.unwrap_or(false), "expected config change to be detected");
    }

    #[tokio::test]
    async fn watcher_handles_missing_file_gracefully() {
        let tmp = tempfile::tempdir().unwrap();
        let missing_path = tmp.path().join("nonexistent.toml");

        let initial = XaftConfig::default();
        let overrides = CliOverrides::default();
        let (_rx, handle) = ConfigWatcher::spawn(
            initial,
            vec![missing_path],
            overrides,
            Duration::from_millis(50),
        );

        // Should not panic or error
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.abort();
    }
}
