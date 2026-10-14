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

            let modified = match std::fs::metadata(path).and_then(|m| m.modified()) {
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

        assert!(
            changed.unwrap_or(false),
            "expected config change to be detected"
        );
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

    // ── `watched_paths` ────────────────────────────────────────────────────
    //
    // The function was entirely uncovered: every test above drives
    // `ConfigWatcher::spawn` with a hand-built path list, so nothing pinned
    // which paths a real `CliOverrides` resolves to.

    #[test]
    fn an_explicit_config_file_is_the_only_watched_path() {
        let overrides = CliOverrides {
            config_file: Some(PathBuf::from("/tmp/explicit.toml")),
            // Even with a project dir set, an explicit file wins outright —
            // it is not merged with the discovered paths.
            project_dir: Some(PathBuf::from("/tmp")),
            ..Default::default()
        };
        assert_eq!(
            watched_paths(&overrides),
            vec![PathBuf::from("/tmp/explicit.toml")]
        );
    }

    #[test]
    fn the_project_config_is_found_by_walking_up_from_the_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project_config = tmp.path().join(".xaft/xaft.toml");
        fs::create_dir_all(project_config.parent().unwrap()).unwrap();
        fs::write(&project_config, "").unwrap();

        // Start three levels below the project root: the walk must climb.
        let nested = tmp.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        let overrides = CliOverrides {
            project_dir: Some(nested),
            ..Default::default()
        };
        let paths = watched_paths(&overrides);
        assert!(
            paths.contains(&project_config),
            "expected the discovered project config in {paths:?}"
        );
        // The walk stops at the first match, so it is the last entry.
        assert_eq!(paths.last(), Some(&project_config));
    }

    #[test]
    fn a_project_dir_with_no_config_contributes_no_project_path() {
        // A bare tempdir has no `.xaft/xaft.toml` on any ancestor up to `/`.
        let tmp = tempfile::tempdir().unwrap();
        let overrides = CliOverrides {
            project_dir: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let paths = watched_paths(&overrides);
        assert!(
            !paths.iter().any(|p| p.ends_with(".xaft/xaft.toml")),
            "no project config exists, so none may be watched: {paths:?}"
        );
        // Whatever is returned must be the user-global path only, and it must
        // be absolute — a relative path here would break the watcher's
        // `exists()` check whenever the process changes directory.
        for p in &paths {
            assert!(p.is_absolute(), "watched path must be absolute: {p:?}");
        }
    }

    #[tokio::test]
    async fn the_watcher_stops_once_its_receiver_is_dropped() {
        // Covers the `is_closed()` break: without it the task would spin
        // forever on a receiver nobody holds.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("xaft.toml");
        fs::write(&config_path, "").unwrap();

        let overrides = CliOverrides {
            config_file: Some(config_path.clone()),
            ..Default::default()
        };
        let (rx, handle) = ConfigWatcher::spawn(
            XaftConfig::default(),
            vec![config_path],
            overrides,
            Duration::from_millis(20),
        );

        drop(rx);
        let stopped = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            stopped.is_ok(),
            "the watcher task must exit after its receiver is dropped"
        );
    }

    #[tokio::test]
    async fn a_reload_that_fails_to_parse_keeps_the_previous_config() {
        // Covers the `ConfigLoader::load` error arm: a changed-but-broken file
        // must warn and keep the old value rather than pushing a partial one.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("xaft.toml");
        fs::write(&config_path, "[core]\nlog_level = \"info\"\n").unwrap();

        let overrides = CliOverrides {
            config_file: Some(config_path.clone()),
            ..Default::default()
        };
        let (rx, handle) = ConfigWatcher::spawn(
            XaftConfig::default(),
            vec![config_path.clone()],
            overrides,
            Duration::from_millis(20),
        );

        // Let the first tick record the mtime of the valid file.
        tokio::time::sleep(Duration::from_millis(60)).await;

        fs::write(&config_path, "this is not = = valid toml").unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(
            !rx.has_changed().unwrap_or(false),
            "a parse failure must not publish a new config"
        );
        handle.abort();
    }
}
