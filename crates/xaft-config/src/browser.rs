//! Browser-automation settings: `[tools.cloakbrowser]`, `[tools.playwright]`
//! and `tools.browser_backend`.
//!
//! Parity with agenthicc's `CloakBrowserSettings`, `PlaywrightSettings` and
//! `ToolSettings.browser_backend` (`agenthicc/config.py`). Every key, default
//! and validation rule is agenthicc's; the only default that differs is
//! `profile_root`, which lives under xaft's `.xaft/` project directory rather
//! than `.agenthicc/`.
//!
//! These structs are *read* by `xaft_tools::browser` (the CDP engine, the
//! session manager and the eighteen `playwright_*` / `cloakbrowser_*` tools)
//! and by `xaft-runtime`, which selects the backend per run. Nothing here
//! starts a browser: like agenthicc, a config naming a browser that is not
//! installed loads fine and the tools report the missing runtime when used.
//!
//! # Validation
//!
//! [`validate_browser_settings`] mirrors agenthicc's `__post_init__` checks,
//! which make config *loading* fail. One agenthicc rule is deliberately **not**
//! a load-time check: a malformed `allowed_domains` entry makes the browser
//! tools report `policy_denied` ("Browser allow-list configuration is
//! invalid.") instead of stopping xaft from starting — agenthicc's own
//! "fail closed, don't fail to boot" choice.
//!
//! One rule is xaft's addition: `license_key_env` must be an
//! environment-variable *name*. `xaft config show` prints it, and a reference
//! is only safe to print if something enforces that it is a reference (the
//! `env_headers` lesson recorded for the MCP config).

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// The values `tools.browser_backend` accepts.
pub const BROWSER_BACKENDS: &[&str] = &["cloakbrowser", "playwright", "none"];

/// Which browser tool family a session exposes (`tools.browser_backend`).
///
/// agenthicc exposes exactly one family per session: `cloakbrowser_*` tools
/// driven by `[tools.cloakbrowser]`, `playwright_*` tools driven by
/// `[tools.playwright]`, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserBackend {
    /// `cloakbrowser` — agenthicc's default.
    CloakBrowser,
    /// `playwright`.
    Playwright,
    /// `none` — no browser tools.
    None,
}

impl BrowserBackend {
    /// Parse a configured value the way agenthicc does (`strip().lower()`).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "cloakbrowser" => Some(Self::CloakBrowser),
            "playwright" => Some(Self::Playwright),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// The canonical spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CloakBrowser => "cloakbrowser",
            Self::Playwright => "playwright",
            Self::None => "none",
        }
    }
}

/// `[tools.cloakbrowser]` — agenthicc `CloakBrowserSettings`.
///
/// CloakBrowser is a stealth Chromium build. `transport = "local"` launches
/// the installed CloakBrowser binary; `transport = "cdp"` attaches to an
/// operator-run browser at the loopback `cdp_endpoint` and isolates the
/// session in a fresh browser context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloakBrowserConfig {
    /// Register the `cloakbrowser_*` tools when this backend is selected.
    pub enabled: bool,
    /// `local` (launch the CloakBrowser binary) or `cdp` (attach to `cdp_endpoint`).
    pub transport: String,
    /// The loopback HTTP(S) DevTools endpoint used by `transport = "cdp"`.
    pub cdp_endpoint: String,
    /// Hosts (`example.com`, `*.example.com`) or origins
    /// (`https://example.com:8443`) the browser may reach.
    pub allowed_domains: Vec<String>,
    /// Run the browser without a window.
    pub headless: bool,
    /// Navigation deadline, seconds.
    pub navigation_timeout_s: f64,
    /// Per-action deadline (click, fill, wait, screenshot), seconds.
    pub action_timeout_s: f64,
    /// Maximum simultaneously open pages per session.
    pub max_pages: usize,
    /// Maximum click/fill/press actions per agent turn.
    pub max_actions_per_turn: usize,
    /// Maximum snapshot text, in characters.
    pub max_snapshot_chars: usize,
    /// Maximum screenshot size, in bytes.
    pub max_screenshot_bytes: usize,
    /// Keep a per-session browser profile on disk under `profile_root`.
    pub allow_persistent_profiles: bool,
    /// Workspace-relative directory for persistent profiles.
    pub profile_root: String,
    /// Name of the environment variable holding the CloakBrowser licence key.
    pub license_key_env: String,
    /// Admit every HTTP(S) host, port, loopback and private address.
    pub allow_all_domains: bool,
}

impl Default for CloakBrowserConfig {
    /// agenthicc's `CloakBrowserSettings` defaults: enabled and unrestricted,
    /// for the operator's own sandbox.
    fn default() -> Self {
        Self {
            enabled: true,
            transport: "local".to_string(),
            cdp_endpoint: "http://127.0.0.1:9222".to_string(),
            allowed_domains: Vec::new(),
            headless: true,
            navigation_timeout_s: 15.0,
            action_timeout_s: 10.0,
            max_pages: 4,
            max_actions_per_turn: 20,
            max_snapshot_chars: 20_000,
            max_screenshot_bytes: 10_000_000,
            allow_persistent_profiles: false,
            profile_root: ".xaft/browser-profiles".to_string(),
            license_key_env: "CLOAKBROWSER_LICENSE_KEY".to_string(),
            allow_all_domains: true,
        }
    }
}

/// `[tools.playwright]` — agenthicc `PlaywrightSettings`.
///
/// xaft drives Chromium over the Chrome DevTools Protocol itself, so it needs
/// no Playwright package — only a Chromium-family browser (Playwright's own
/// download, a `browser_channel` such as `chrome`, an `executable_path`, or a
/// Chromium on `PATH`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaywrightConfig {
    /// Register the `playwright_*` tools when this backend is selected.
    pub enabled: bool,
    /// Always `local`.
    pub transport: String,
    /// `chromium`, `firefox` or `webkit`. Only `chromium` can be driven over CDP.
    pub browser_type: String,
    /// A branded Chromium channel (`chrome`, `msedge`, `chrome-beta`, ...).
    pub browser_channel: String,
    /// An explicit browser executable.
    pub executable_path: String,
    /// Hosts or origins the browser may reach.
    pub allowed_domains: Vec<String>,
    /// Run the browser without a window.
    pub headless: bool,
    /// Navigation deadline, seconds.
    pub navigation_timeout_s: f64,
    /// Per-action deadline, seconds.
    pub action_timeout_s: f64,
    /// Maximum simultaneously open pages per session.
    pub max_pages: usize,
    /// Maximum click/fill/press actions per agent turn.
    pub max_actions_per_turn: usize,
    /// Maximum snapshot text, in characters.
    pub max_snapshot_chars: usize,
    /// Maximum screenshot size, in bytes.
    pub max_screenshot_bytes: usize,
    /// Keep a per-session browser profile on disk under `profile_root`.
    pub allow_persistent_profiles: bool,
    /// Workspace-relative directory for persistent profiles.
    pub profile_root: String,
    /// Admit every HTTP(S) host and private address (loopback and every port
    /// are always admitted for Playwright, which serves local dev servers).
    pub allow_all_domains: bool,
}

impl Default for PlaywrightConfig {
    /// agenthicc's `PlaywrightSettings` defaults.
    fn default() -> Self {
        Self {
            enabled: true,
            transport: "local".to_string(),
            browser_type: "chromium".to_string(),
            browser_channel: String::new(),
            executable_path: String::new(),
            allowed_domains: Vec::new(),
            headless: true,
            navigation_timeout_s: 15.0,
            action_timeout_s: 10.0,
            max_pages: 4,
            max_actions_per_turn: 20,
            max_snapshot_chars: 20_000,
            max_screenshot_bytes: 10_000_000,
            allow_persistent_profiles: false,
            profile_root: ".xaft/browser-profiles/playwright".to_string(),
            allow_all_domains: true,
        }
    }
}

/// agenthicc's allow-list normalisation: strip, lower-case, drop a leading
/// `.`, de-duplicate and sort, dropping empty entries.
pub fn normalize_allowed_domains(domains: &[String]) -> Vec<String> {
    let mut out: Vec<String> = domains
        .iter()
        .map(|d| d.trim().to_lowercase().trim_start_matches('.').to_string())
        .filter(|d| !d.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn timeouts_valid(values: &[f64]) -> bool {
    values.iter().all(|v| v.is_finite() && *v > 0.0)
}

fn profile_root_valid(profile_root: &str) -> bool {
    if profile_root.trim().is_empty() {
        return false;
    }
    let path = Path::new(profile_root);
    if path.is_absolute() || profile_root.starts_with('/') || profile_root.starts_with('\\') {
        return false;
    }
    !path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
}

/// agenthicc's `cdp_endpoint` rules, in agenthicc's order.
fn validate_cdp_endpoint(endpoint: &str) -> Result<(), ConfigError> {
    let section = "tools.cloakbrowser.cdp_endpoint";
    let not_loopback_url = || ConfigError::validation(section, "must be an HTTP(S) loopback URL");
    let trimmed = endpoint.trim();
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(not_loopback_url)?;
    let scheme = scheme.to_ascii_lowercase();
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    // Split host and port, honouring IPv6 brackets. A malformed port is
    // Python's `ValueError` from `SplitResult.port`.
    let (host, port): (String, Option<u32>) = if let Some(inner) = hostport.strip_prefix('[') {
        let end = inner.find(']').ok_or_else(not_loopback_url)?;
        let after = &inner[end + 1..];
        let port = if after.is_empty() {
            None
        } else if let Some(p) = after.strip_prefix(':') {
            Some(p)
        } else {
            return Err(not_loopback_url());
        };
        (inner[..end].to_ascii_lowercase(), parse_port(port)?)
    } else {
        if hostport.contains(['[', ']']) {
            return Err(not_loopback_url());
        }
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_ascii_lowercase(), parse_port(Some(p))?),
            None => (hostport.to_ascii_lowercase(), None),
        }
    };
    if !matches!(scheme.as_str(), "http" | "https") || host.is_empty() {
        return Err(not_loopback_url());
    }
    if userinfo.is_some() || !(tail.is_empty() || tail == "/") {
        return Err(ConfigError::validation(
            section,
            "must not contain credentials, a path, a query, or a fragment",
        ));
    }
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return Err(ConfigError::validation(section, "must use a loopback host"));
    }
    if let Some(port) = port {
        if !(1..=65535).contains(&port) {
            return Err(ConfigError::validation(section, "has an invalid port"));
        }
    }
    Ok(())
}

/// Python's `SplitResult.port`: digits in 0-65535, empty meaning "none".
fn parse_port(port: Option<&str>) -> Result<Option<u32>, ConfigError> {
    let not_loopback_url = || {
        ConfigError::validation(
            "tools.cloakbrowser.cdp_endpoint",
            "must be an HTTP(S) loopback URL",
        )
    };
    match port {
        None | Some("") => Ok(None),
        Some(p) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            let value: u32 = p.parse().map_err(|_| not_loopback_url())?;
            if value > 65535 {
                return Err(not_loopback_url());
            }
            Ok(Some(value))
        }
        Some(_) => Err(not_loopback_url()),
    }
}

/// The bounds both browser classes share. `prefix` is `tools.cloakbrowser` or
/// `tools.playwright`; `label` is agenthicc's class label for the timeout
/// sentence.
#[allow(clippy::too_many_arguments)]
fn validate_common(
    prefix: &str,
    label: &str,
    navigation_timeout_s: f64,
    action_timeout_s: f64,
    max_pages: usize,
    max_actions_per_turn: usize,
    max_snapshot_chars: usize,
    max_screenshot_bytes: usize,
    profile_root: &str,
) -> Result<(), ConfigError> {
    if !timeouts_valid(&[navigation_timeout_s]) {
        return Err(ConfigError::validation(
            format!("{prefix}.navigation_timeout_s"),
            format!("{label} timeouts must be greater than zero"),
        ));
    }
    if !timeouts_valid(&[action_timeout_s]) {
        return Err(ConfigError::validation(
            format!("{prefix}.action_timeout_s"),
            format!("{label} timeouts must be greater than zero"),
        ));
    }
    if max_pages < 1 {
        return Err(ConfigError::validation(
            format!("{prefix}.max_pages"),
            "must be at least 1",
        ));
    }
    if max_actions_per_turn < 1 {
        return Err(ConfigError::validation(
            format!("{prefix}.max_actions_per_turn"),
            "must be at least 1",
        ));
    }
    if max_snapshot_chars < 256 {
        return Err(ConfigError::validation(
            format!("{prefix}.max_snapshot_chars"),
            "must be at least 256",
        ));
    }
    if max_screenshot_bytes < 1024 {
        return Err(ConfigError::validation(
            format!("{prefix}.max_screenshot_bytes"),
            "must be at least 1024",
        ));
    }
    if !profile_root_valid(profile_root) {
        return Err(ConfigError::validation(
            format!("{prefix}.profile_root"),
            "must be a non-empty relative path inside the workspace",
        ));
    }
    Ok(())
}

impl CloakBrowserConfig {
    /// The transport, normalised as agenthicc does (`strip().lower()`).
    pub fn normalized_transport(&self) -> String {
        self.transport.trim().to_lowercase()
    }

    /// agenthicc `CloakBrowserSettings.__post_init__`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let transport = self.normalized_transport();
        if transport != "local" && transport != "cdp" {
            return Err(ConfigError::validation(
                "tools.cloakbrowser.transport",
                "must be 'local' or 'cdp'",
            ));
        }
        if transport == "cdp" {
            validate_cdp_endpoint(&self.cdp_endpoint)?;
        }
        validate_common(
            "tools.cloakbrowser",
            "CloakBrowser",
            self.navigation_timeout_s,
            self.action_timeout_s,
            self.max_pages,
            self.max_actions_per_turn,
            self.max_snapshot_chars,
            self.max_screenshot_bytes,
            &self.profile_root,
        )?;
        let env = self.license_key_env.trim();
        if env.is_empty() {
            return Err(ConfigError::validation(
                "tools.cloakbrowser.license_key_env",
                "must not be empty",
            ));
        }
        if !crate::types::is_env_var_name(env) {
            // Never echo the value: the likeliest cause is a licence key pasted
            // where the variable *name* belongs.
            return Err(ConfigError::validation(
                "tools.cloakbrowser.license_key_env",
                "must name an environment variable (for example CLOAKBROWSER_LICENSE_KEY), \
                 not hold the key itself",
            ));
        }
        Ok(())
    }
}

impl PlaywrightConfig {
    /// The transport, normalised (`strip().lower()`).
    pub fn normalized_transport(&self) -> String {
        self.transport.trim().to_lowercase()
    }

    /// The browser type, normalised (`strip().lower()`).
    pub fn normalized_browser_type(&self) -> String {
        self.browser_type.trim().to_lowercase()
    }

    /// agenthicc `PlaywrightSettings.__post_init__`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.normalized_transport() != "local" {
            return Err(ConfigError::validation(
                "tools.playwright.transport",
                "must be 'local'",
            ));
        }
        let browser_type = self.normalized_browser_type();
        if !matches!(browser_type.as_str(), "chromium" | "firefox" | "webkit") {
            return Err(ConfigError::validation(
                "tools.playwright.browser_type",
                "must be 'chromium', 'firefox', or 'webkit'",
            ));
        }
        validate_common(
            "tools.playwright",
            "Playwright",
            self.navigation_timeout_s,
            self.action_timeout_s,
            self.max_pages,
            self.max_actions_per_turn,
            self.max_snapshot_chars,
            self.max_screenshot_bytes,
            &self.profile_root,
        )?;
        if !self.browser_channel.trim().is_empty() && browser_type != "chromium" {
            return Err(ConfigError::validation(
                "tools.playwright.browser_channel",
                "is supported only for chromium",
            ));
        }
        Ok(())
    }
}

/// Validate `tools.browser_backend` and both browser sections.
///
/// agenthicc validates both classes whatever the selected backend, because
/// both are constructed at load time; so does this.
pub fn validate_browser_settings(
    browser_backend: &str,
    cloakbrowser: &CloakBrowserConfig,
    playwright: &PlaywrightConfig,
) -> Result<(), ConfigError> {
    if BrowserBackend::parse(browser_backend).is_none() {
        return Err(ConfigError::validation(
            "tools.browser_backend",
            "must be 'cloakbrowser', 'playwright', or 'none'",
        ));
    }
    cloakbrowser.validate()?;
    playwright.validate()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(err: ConfigError) -> String {
        err.to_string()
    }

    #[test]
    fn defaults_are_agenthiccs() {
        let c = CloakBrowserConfig::default();
        assert!(c.enabled && c.headless && c.allow_all_domains);
        assert_eq!(c.transport, "local");
        assert_eq!(c.cdp_endpoint, "http://127.0.0.1:9222");
        assert!(c.allowed_domains.is_empty());
        assert_eq!((c.navigation_timeout_s, c.action_timeout_s), (15.0, 10.0));
        assert_eq!(
            (
                c.max_pages,
                c.max_actions_per_turn,
                c.max_snapshot_chars,
                c.max_screenshot_bytes
            ),
            (4, 20, 20_000, 10_000_000)
        );
        assert!(!c.allow_persistent_profiles);
        assert_eq!(c.license_key_env, "CLOAKBROWSER_LICENSE_KEY");
        c.validate().unwrap();

        let p = PlaywrightConfig::default();
        assert!(p.enabled && p.headless && p.allow_all_domains);
        assert_eq!(
            (p.transport.as_str(), p.browser_type.as_str()),
            ("local", "chromium")
        );
        assert!(p.browser_channel.is_empty() && p.executable_path.is_empty());
        assert_eq!(p.profile_root, ".xaft/browser-profiles/playwright");
        p.validate().unwrap();
    }

    #[test]
    fn backend_parsing_is_case_and_space_insensitive() {
        assert_eq!(
            BrowserBackend::parse(" Playwright "),
            Some(BrowserBackend::Playwright)
        );
        assert_eq!(
            BrowserBackend::parse("CLOAKBROWSER"),
            Some(BrowserBackend::CloakBrowser)
        );
        assert_eq!(BrowserBackend::parse("none"), Some(BrowserBackend::None));
        assert_eq!(BrowserBackend::parse("chrome"), None);
        let err = validate_browser_settings(
            "chrome",
            &CloakBrowserConfig::default(),
            &PlaywrightConfig::default(),
        )
        .unwrap_err();
        assert!(message(err).contains("must be 'cloakbrowser', 'playwright', or 'none'"));
    }

    #[test]
    fn transport_and_browser_type_follow_agenthicc() {
        let cloak = CloakBrowserConfig {
            transport: " CDP ".into(),
            ..Default::default()
        };
        cloak.validate().unwrap();
        let cloak = CloakBrowserConfig {
            transport: "remote".into(),
            ..Default::default()
        };
        assert!(message(cloak.validate().unwrap_err()).contains("must be 'local' or 'cdp'"));

        let pw = PlaywrightConfig {
            transport: "cdp".into(),
            ..Default::default()
        };
        assert!(message(pw.validate().unwrap_err()).contains("must be 'local'"));
        let pw = PlaywrightConfig {
            browser_type: "safari".into(),
            ..Default::default()
        };
        assert!(message(pw.validate().unwrap_err()).contains("browser_type"));
        let pw = PlaywrightConfig {
            browser_type: "Firefox".into(),
            ..Default::default()
        };
        pw.validate().unwrap();
        let pw = PlaywrightConfig {
            browser_type: "firefox".into(),
            browser_channel: "chrome".into(),
            ..Default::default()
        };
        assert!(message(pw.validate().unwrap_err()).contains("supported only for chromium"));
    }

    #[test]
    fn the_cdp_endpoint_is_loopback_only() {
        let cdp = |endpoint: &str| CloakBrowserConfig {
            transport: "cdp".into(),
            cdp_endpoint: endpoint.into(),
            ..Default::default()
        };
        for ok in [
            "http://127.0.0.1:9222",
            "http://localhost:9222/",
            "https://[::1]:9222",
            "http://127.0.0.1",
        ] {
            cdp(ok).validate().unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        let cases = [
            ("http://remote.example:9222", "must use a loopback host"),
            ("http://[invalid", "must be an HTTP(S) loopback URL"),
            ("ws://127.0.0.1:9222", "must be an HTTP(S) loopback URL"),
            ("127.0.0.1:9222", "must be an HTTP(S) loopback URL"),
            ("http://127.0.0.1:bad", "must be an HTTP(S) loopback URL"),
            ("http://127.0.0.1:70000", "must be an HTTP(S) loopback URL"),
            ("http://127.0.0.1:0", "has an invalid port"),
            (
                "http://user:pw@127.0.0.1:9222",
                "must not contain credentials",
            ),
            ("http://127.0.0.1:9222/json", "must not contain credentials"),
            ("http://127.0.0.1:9222?x=1", "must not contain credentials"),
        ];
        for (endpoint, expected) in cases {
            let err = message(cdp(endpoint).validate().unwrap_err());
            assert!(err.contains(expected), "{endpoint}: {err}");
        }
        // Only validated for the cdp transport, as in agenthicc.
        CloakBrowserConfig {
            cdp_endpoint: "http://remote.example:9222".into(),
            ..Default::default()
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn bounds_and_profile_root_are_enforced() {
        let base = CloakBrowserConfig::default;
        let cases: Vec<(CloakBrowserConfig, &str)> = vec![
            (
                CloakBrowserConfig {
                    navigation_timeout_s: 0.0,
                    ..base()
                },
                "CloakBrowser timeouts must be greater than zero",
            ),
            (
                CloakBrowserConfig {
                    action_timeout_s: -1.0,
                    ..base()
                },
                "CloakBrowser timeouts must be greater than zero",
            ),
            (
                CloakBrowserConfig {
                    action_timeout_s: f64::NAN,
                    ..base()
                },
                "CloakBrowser timeouts must be greater than zero",
            ),
            (
                CloakBrowserConfig {
                    max_pages: 0,
                    ..base()
                },
                "max_pages]: must be at least 1",
            ),
            (
                CloakBrowserConfig {
                    max_actions_per_turn: 0,
                    ..base()
                },
                "max_actions_per_turn]: must be at least 1",
            ),
            (
                CloakBrowserConfig {
                    max_snapshot_chars: 255,
                    ..base()
                },
                "must be at least 256",
            ),
            (
                CloakBrowserConfig {
                    max_screenshot_bytes: 1023,
                    ..base()
                },
                "must be at least 1024",
            ),
            (
                CloakBrowserConfig {
                    profile_root: "../profiles".into(),
                    ..base()
                },
                "relative path inside the workspace",
            ),
            (
                CloakBrowserConfig {
                    profile_root: "/abs".into(),
                    ..base()
                },
                "relative path inside the workspace",
            ),
            (
                CloakBrowserConfig {
                    profile_root: "  ".into(),
                    ..base()
                },
                "relative path inside the workspace",
            ),
            (
                CloakBrowserConfig {
                    license_key_env: " ".into(),
                    ..base()
                },
                "must not be empty",
            ),
        ];
        for (config, expected) in cases {
            let err = message(config.validate().unwrap_err());
            assert!(err.contains(expected), "{expected}: {err}");
        }
        let pw = PlaywrightConfig {
            max_pages: 0,
            ..Default::default()
        };
        assert!(message(pw.validate().unwrap_err()).contains("tools.playwright.max_pages"));
        let pw = PlaywrightConfig {
            navigation_timeout_s: 0.0,
            ..Default::default()
        };
        assert!(
            message(pw.validate().unwrap_err())
                .contains("Playwright timeouts must be greater than zero")
        );
    }

    /// A pasted key is refused without being echoed back.
    #[test]
    fn license_key_env_must_be_a_variable_name_and_is_never_echoed() {
        let secret = "cb-live-1234-abcd";
        let config = CloakBrowserConfig {
            license_key_env: secret.into(),
            ..Default::default()
        };
        let err = message(config.validate().unwrap_err());
        assert!(err.contains("must name an environment variable"));
        assert!(!err.contains(secret));
    }

    #[test]
    fn allowed_domains_normalise_like_agenthicc() {
        let domains = vec![
            "Example.com".to_string(),
            ".example.org".to_string(),
            " ".to_string(),
            "example.com".to_string(),
            "HTTPS://Example.com".to_string(),
        ];
        assert_eq!(
            normalize_allowed_domains(&domains),
            vec!["example.com", "example.org", "https://example.com"]
        );
    }
}
