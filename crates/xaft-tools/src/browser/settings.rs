//! Resolved browser settings: one shape for both tool families.
//!
//! agenthicc types its session manager against a structural `BrowserSettings`
//! protocol that both `CloakBrowserSettings` and `PlaywrightSettings`
//! satisfy. [`BrowserSettings`] is that protocol made concrete: built from
//! whichever config section the session's backend selects, with every value
//! the engine needs already normalised and range-checked.

use std::time::Duration;

use xaft_config::{CloakBrowserConfig, PlaywrightConfig, normalize_allowed_domains};

/// Which agenthicc tool family a manager serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserFamily {
    /// `cloakbrowser_*` tools over the CloakBrowser stealth Chromium build.
    CloakBrowser,
    /// `playwright_*` tools over a stock Chromium-family browser.
    Playwright,
}

impl BrowserFamily {
    /// The tool-name prefix (`cloakbrowser` / `playwright`).
    pub fn prefix(self) -> &'static str {
        match self {
            Self::CloakBrowser => "cloakbrowser",
            Self::Playwright => "playwright",
        }
    }

    /// agenthicc's `backend_name`, used in agent-facing sentences.
    pub fn backend_name(self) -> &'static str {
        match self {
            Self::CloakBrowser => "CloakBrowser",
            Self::Playwright => "Playwright",
        }
    }
}

/// How the browser is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserTransport {
    /// xaft launches the browser itself.
    Local,
    /// xaft attaches to an operator-run browser over a loopback CDP endpoint.
    Cdp,
}

impl BrowserTransport {
    /// The wire value reported in `*_status`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cdp => "cdp",
        }
    }
}

/// The settings one browser session runs under.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserSettings {
    /// The tool family.
    pub family: BrowserFamily,
    /// Whether the family's tools are registered at all.
    pub enabled: bool,
    /// Local launch or CDP attach.
    pub transport: BrowserTransport,
    /// The loopback CDP endpoint (`cdp` transport only).
    pub cdp_endpoint: String,
    /// Normalised allow-list entries.
    pub allowed_domains: Vec<String>,
    /// Admit every HTTP(S) destination.
    pub allow_all_domains: bool,
    /// Launch without a window.
    pub headless: bool,
    /// Navigation deadline.
    pub navigation_timeout: Duration,
    /// Per-action deadline.
    pub action_timeout: Duration,
    /// Maximum open pages per session.
    pub max_pages: usize,
    /// Maximum click/fill/press actions per turn.
    pub max_actions_per_turn: usize,
    /// Maximum snapshot text, in characters.
    pub max_snapshot_chars: usize,
    /// Maximum screenshot size, in bytes.
    pub max_screenshot_bytes: usize,
    /// Keep the profile on disk between sessions.
    pub allow_persistent_profiles: bool,
    /// Workspace-relative profile directory.
    pub profile_root: String,
    /// CloakBrowser licence-key variable name (`None` for Playwright).
    pub license_key_env: Option<String>,
    /// `chromium`, `firefox` or `webkit` (always `chromium` for CloakBrowser).
    pub browser_type: String,
    /// A branded Chromium channel, or empty.
    pub browser_channel: String,
    /// An explicit executable, or empty.
    pub executable_path: String,
}

/// A positive, finite number of seconds as a `Duration`, or `fallback`.
///
/// Config validation already refuses non-positive and non-finite timeouts;
/// this guard exists so a config built in code (tests, embedders) can never
/// make `Duration::from_secs_f64` panic.
fn seconds(value: f64, fallback: f64) -> Duration {
    Duration::try_from_secs_f64(value)
        .ok()
        .filter(|d| !d.is_zero())
        .unwrap_or_else(|| Duration::from_secs_f64(fallback))
}

impl BrowserSettings {
    /// Settings for the `cloakbrowser_*` family.
    pub fn from_cloakbrowser(config: &CloakBrowserConfig) -> Self {
        let transport = if config.normalized_transport() == "cdp" {
            BrowserTransport::Cdp
        } else {
            BrowserTransport::Local
        };
        Self {
            family: BrowserFamily::CloakBrowser,
            enabled: config.enabled,
            transport,
            cdp_endpoint: config.cdp_endpoint.trim().to_string(),
            allowed_domains: normalize_allowed_domains(&config.allowed_domains),
            allow_all_domains: config.allow_all_domains,
            headless: config.headless,
            navigation_timeout: seconds(config.navigation_timeout_s, 15.0),
            action_timeout: seconds(config.action_timeout_s, 10.0),
            max_pages: config.max_pages.max(1),
            max_actions_per_turn: config.max_actions_per_turn.max(1),
            max_snapshot_chars: config.max_snapshot_chars.max(256),
            max_screenshot_bytes: config.max_screenshot_bytes.max(1024),
            allow_persistent_profiles: config.allow_persistent_profiles,
            profile_root: config.profile_root.clone(),
            license_key_env: Some(config.license_key_env.trim().to_string())
                .filter(|v| !v.is_empty()),
            browser_type: "chromium".to_string(),
            browser_channel: String::new(),
            executable_path: String::new(),
        }
    }

    /// Settings for the `playwright_*` family.
    pub fn from_playwright(config: &PlaywrightConfig) -> Self {
        Self {
            family: BrowserFamily::Playwright,
            enabled: config.enabled,
            transport: BrowserTransport::Local,
            cdp_endpoint: String::new(),
            allowed_domains: normalize_allowed_domains(&config.allowed_domains),
            allow_all_domains: config.allow_all_domains,
            headless: config.headless,
            navigation_timeout: seconds(config.navigation_timeout_s, 15.0),
            action_timeout: seconds(config.action_timeout_s, 10.0),
            max_pages: config.max_pages.max(1),
            max_actions_per_turn: config.max_actions_per_turn.max(1),
            max_snapshot_chars: config.max_snapshot_chars.max(256),
            max_screenshot_bytes: config.max_screenshot_bytes.max(1024),
            allow_persistent_profiles: config.allow_persistent_profiles,
            profile_root: config.profile_root.clone(),
            license_key_env: None,
            browser_type: config.normalized_browser_type(),
            browser_channel: config.browser_channel.trim().to_lowercase(),
            executable_path: config.executable_path.trim().to_string(),
        }
    }

    /// The transport label `*_status` reports.
    pub fn transport_label(&self) -> &'static str {
        self.transport.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve_to_agenthiccs_values() {
        let s = BrowserSettings::from_cloakbrowser(&CloakBrowserConfig::default());
        assert_eq!(s.family, BrowserFamily::CloakBrowser);
        assert!(s.enabled && s.headless && s.allow_all_domains);
        assert_eq!(s.transport, BrowserTransport::Local);
        assert_eq!(s.navigation_timeout, Duration::from_secs(15));
        assert_eq!(s.action_timeout, Duration::from_secs(10));
        assert_eq!((s.max_pages, s.max_actions_per_turn), (4, 20));
        assert_eq!(
            s.license_key_env.as_deref(),
            Some("CLOAKBROWSER_LICENSE_KEY")
        );

        let p = BrowserSettings::from_playwright(&PlaywrightConfig::default());
        assert_eq!(p.family, BrowserFamily::Playwright);
        assert_eq!(p.browser_type, "chromium");
        assert!(p.license_key_env.is_none());
    }

    #[test]
    fn values_are_normalised() {
        let cloak = CloakBrowserConfig {
            transport: " CDP ".into(),
            allowed_domains: vec![".Example.com".into(), "example.com".into()],
            ..Default::default()
        };
        let s = BrowserSettings::from_cloakbrowser(&cloak);
        assert_eq!(s.transport, BrowserTransport::Cdp);
        assert_eq!(s.allowed_domains, vec!["example.com"]);

        let pw = PlaywrightConfig {
            browser_type: " Chromium ".into(),
            browser_channel: " Chrome ".into(),
            ..Default::default()
        };
        let p = BrowserSettings::from_playwright(&pw);
        assert_eq!(
            (p.browser_type.as_str(), p.browser_channel.as_str()),
            ("chromium", "chrome")
        );
    }

    /// A timeout built in code that validation would have refused cannot
    /// panic the engine.
    #[test]
    fn invalid_timeouts_fall_back_instead_of_panicking() {
        let cloak = CloakBrowserConfig {
            navigation_timeout_s: -1.0,
            action_timeout_s: f64::NAN,
            ..Default::default()
        };
        let s = BrowserSettings::from_cloakbrowser(&cloak);
        assert_eq!(s.navigation_timeout, Duration::from_secs(15));
        assert_eq!(s.action_timeout, Duration::from_secs(10));
    }
}
