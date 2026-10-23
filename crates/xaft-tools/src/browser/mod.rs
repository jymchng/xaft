//! Browser automation: agenthicc's `playwright_*` and `cloakbrowser_*` tools.
//!
//! Parity rows 60-77 (tools) and the `[tools.playwright]`,
//! `[tools.cloakbrowser]` and `tools.browser_backend` config keys
//! (rows 78-108, 117-119). agenthicc implements both tool families as thin
//! closures over one session manager and two Playwright-based transports;
//! xaft keeps that shape and replaces the transports with one Chromium engine
//! driven over the Chrome DevTools Protocol, so neither Python nor Playwright
//! is needed — only a Chromium-family browser.
//!
//! ```text
//! tools.rs     BrowserTool ×9 per family   (agent_tools.py)
//!     │
//! session.rs   BrowserSessionManager       (session.py: quotas, op ids, bounding)
//!     │            ├── policy.rs           (policy.py: allow-list, input bounds)
//!     │            └── artifacts.rs        (artifacts.py: screenshots)
//!     │
//! client.rs    BrowserClient trait          (client.py: the transport contract)
//!     │
//! engine.rs    CdpBrowserClient            (one engine for both families)
//!     ├── launch.rs   find + launch Chromium (pipe transport), or
//!     └── cdp.rs      attach over a loopback DevTools WebSocket
//! ```
//!
//! # Which family a session gets
//!
//! Exactly one, as in agenthicc: `tools.browser_backend` selects
//! `cloakbrowser` (the default), `playwright` or `none`, and the selected
//! section's `enabled` decides whether its nine tools are registered.
//! [`BrowserSessionManager::from_tool_settings`] applies that rule;
//! [`crate::ToolRegistryBuilder::with_browser_session`] registers the result.
//!
//! # Lifecycle
//!
//! Nothing launches until a tool needs a browser. One manager serves one
//! xaft session across its turns ([`BrowserSessionCache`]); the runtime resets
//! the action budget at the start of every run and releases the browser when
//! a run fails, keeping the tools usable — agenthicc's `reset_turn_budget` and
//! `close_runtime`.

pub mod artifacts;
mod cdp;
pub mod client;
pub mod engine;
pub mod errors;
pub mod launch;
pub mod policy;
pub mod session;
pub mod settings;
pub mod tools;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use xaft_config::{BrowserBackend, ToolSettingsConfig};

pub use artifacts::{BrowserArtifact, BrowserArtifactStore};
pub use client::{
    BrowserClient, BrowserHealth, PageSnapshot, PageState, ScreenshotData, UnavailableBrowserClient,
};
pub use engine::{BrowserLaunchOptions, CdpBrowserClient};
pub use errors::{BrowserErrorKind, BrowserFailure, BrowserToolError};
pub use launch::BrowserDiscovery;
pub use policy::{BrowserPolicy, redact_link, redact_url};
pub use session::BrowserSessionManager;
pub use settings::{BrowserFamily, BrowserSettings, BrowserTransport};
pub use tools::{BROWSER_VERBS, BrowserTool, BrowserVerb, browser_tools, read_only_browser_tools};

/// agenthicc `PLAYWRIGHT_AGENT_TOOLS`.
pub const PLAYWRIGHT_TOOL_NAMES: [&str; 9] = [
    "playwright_status",
    "playwright_open",
    "playwright_snapshot",
    "playwright_click",
    "playwright_fill",
    "playwright_press",
    "playwright_wait_for",
    "playwright_screenshot",
    "playwright_close",
];

/// agenthicc `CLOAKBROWSER_AGENT_TOOLS`.
pub const CLOAKBROWSER_TOOL_NAMES: [&str; 9] = [
    "cloakbrowser_status",
    "cloakbrowser_open",
    "cloakbrowser_snapshot",
    "cloakbrowser_click",
    "cloakbrowser_fill",
    "cloakbrowser_press",
    "cloakbrowser_wait_for",
    "cloakbrowser_screenshot",
    "cloakbrowser_close",
];

/// agenthicc `is_browser_tool`: is `name` one of the eighteen browser tools?
pub fn is_browser_tool(name: &str) -> bool {
    PLAYWRIGHT_TOOL_NAMES.contains(&name) || CLOAKBROWSER_TOOL_NAMES.contains(&name)
}

/// How many sessions keep a browser manager alive at once.
const CACHE_CAPACITY: usize = 4;

struct CacheEntry {
    session_id: String,
    workspace_root: PathBuf,
    manager: Arc<BrowserSessionManager>,
}

/// One browser manager per xaft session, reused across that session's runs.
///
/// agenthicc keeps one manager (and one browser context) per stable session
/// conversation, so pages opened in one turn are still open in the next. A
/// runtime serving several runs owns one of these. The cache is bounded: the
/// least recently used session's browser is closed when a fifth session asks,
/// and a session whose browser settings changed (a hot-reloaded config) gets a
/// fresh manager rather than one built from stale settings.
pub struct BrowserSessionCache {
    entries: Mutex<VecDeque<CacheEntry>>,
    options: BrowserLaunchOptions,
}

impl Default for BrowserSessionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for BrowserSessionCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.entries.lock().map(|e| e.len()).unwrap_or(0);
        f.debug_struct("BrowserSessionCache")
            .field("sessions", &len)
            .finish_non_exhaustive()
    }
}

fn close_in_background(manager: Arc<BrowserSessionManager>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move { manager.close_session().await });
    }
}

impl BrowserSessionCache {
    /// An empty cache using the real environment to find browsers.
    pub fn new() -> Self {
        Self::with_options(BrowserLaunchOptions::default())
    }

    /// An empty cache whose managers launch with `options`.
    pub fn with_options(options: BrowserLaunchOptions) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            options,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<CacheEntry>> {
        self.entries.lock().expect("browser session cache poisoned")
    }

    /// The manager `session_id` uses in `workspace_root` under `tools`, or
    /// `None` when `browser_backend = "none"`.
    pub fn session_for(
        &self,
        tools: &ToolSettingsConfig,
        workspace_root: &Path,
        session_id: &str,
    ) -> Option<Arc<BrowserSessionManager>> {
        let wanted = match BrowserBackend::parse(&tools.browser_backend) {
            Some(BrowserBackend::CloakBrowser) => {
                Some(BrowserSettings::from_cloakbrowser(&tools.cloakbrowser))
            }
            Some(BrowserBackend::Playwright) => {
                Some(BrowserSettings::from_playwright(&tools.playwright))
            }
            Some(BrowserBackend::None) | None => None,
        };
        let mut entries = self.lock();
        let position = entries
            .iter()
            .position(|e| e.session_id == session_id && e.workspace_root == workspace_root);
        if let Some(index) = position {
            let entry = entries.remove(index).expect("index from position");
            match &wanted {
                Some(settings) if entry.manager.settings() == settings => {
                    let manager = Arc::clone(&entry.manager);
                    entries.push_back(entry);
                    return Some(manager);
                }
                _ => close_in_background(entry.manager),
            }
        }
        let settings = wanted?;
        let manager = Arc::new(BrowserSessionManager::new(
            settings,
            session_id,
            workspace_root,
            Some(session_id),
            self.options.clone(),
        ));
        entries.push_back(CacheEntry {
            session_id: session_id.to_string(),
            workspace_root: workspace_root.to_path_buf(),
            manager: Arc::clone(&manager),
        });
        while entries.len() > CACHE_CAPACITY {
            if let Some(evicted) = entries.pop_front() {
                close_in_background(evicted.manager);
            }
        }
        Some(manager)
    }

    /// Close and forget `session_id`'s browser (a one-shot run is over).
    pub async fn close_session(&self, session_id: &str) {
        let removed: Vec<Arc<BrowserSessionManager>> = {
            let mut entries = self.lock();
            let (gone, kept): (Vec<_>, Vec<_>) =
                entries.drain(..).partition(|e| e.session_id == session_id);
            entries.extend(kept);
            gone.into_iter().map(|e| e.manager).collect()
        };
        for manager in removed {
            manager.close_session().await;
        }
    }

    /// Close every cached browser.
    pub async fn close_all(&self) {
        let removed: Vec<Arc<BrowserSessionManager>> =
            self.lock().drain(..).map(|e| e.manager).collect();
        for manager in removed {
            manager.close_session().await;
        }
    }

    /// How many sessions currently hold a manager.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no session holds a manager.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn the_name_lists_are_agenthiccs() {
        assert!(is_browser_tool("playwright_open"));
        assert!(is_browser_tool("cloakbrowser_wait_for"));
        assert!(!is_browser_tool("playwright_unknown"));
        assert!(!is_browser_tool("read_file"));
    }

    #[tokio::test]
    async fn the_cache_reuses_a_session_manager_across_runs() {
        let tmp = TempDir::new().unwrap();
        let cache = BrowserSessionCache::new();
        let tools = ToolSettingsConfig::default();
        let first = cache.session_for(&tools, tmp.path(), "s1").unwrap();
        let again = cache.session_for(&tools, tmp.path(), "s1").unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let other = cache.session_for(&tools, tmp.path(), "s2").unwrap();
        assert!(!Arc::ptr_eq(&first, &other));
        assert_eq!(cache.len(), 2);
    }

    #[tokio::test]
    async fn changed_settings_or_backend_replace_the_manager() {
        let tmp = TempDir::new().unwrap();
        let cache = BrowserSessionCache::new();
        let mut tools = ToolSettingsConfig::default();
        let first = cache.session_for(&tools, tmp.path(), "s1").unwrap();
        tools.cloakbrowser.max_pages = 2;
        let second = cache.session_for(&tools, tmp.path(), "s1").unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.settings().max_pages, 2);
        tools.browser_backend = "playwright".into();
        assert_eq!(
            cache
                .session_for(&tools, tmp.path(), "s1")
                .unwrap()
                .family(),
            BrowserFamily::Playwright
        );
        tools.browser_backend = "none".into();
        assert!(cache.session_for(&tools, tmp.path(), "s1").is_none());
        assert!(cache.is_empty());
    }

    #[tokio::test]
    async fn the_cache_is_bounded_and_closable() {
        let tmp = TempDir::new().unwrap();
        let cache = BrowserSessionCache::new();
        let tools = ToolSettingsConfig::default();
        let first = cache.session_for(&tools, tmp.path(), "s0").unwrap();
        for i in 1..=CACHE_CAPACITY {
            cache.session_for(&tools, tmp.path(), &format!("s{i}"));
        }
        assert_eq!(cache.len(), CACHE_CAPACITY);
        let refreshed = cache.session_for(&tools, tmp.path(), "s0").unwrap();
        assert!(!Arc::ptr_eq(&first, &refreshed), "s0 was evicted");
        cache.close_session("s0").await;
        assert_eq!(cache.len(), CACHE_CAPACITY - 1);
        assert_eq!(refreshed.status().await["status"], "closed");
        cache.close_all().await;
        assert!(cache.is_empty());
    }
}
