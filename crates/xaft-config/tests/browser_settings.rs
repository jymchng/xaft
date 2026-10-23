//! `[tools.cloakbrowser]`, `[tools.playwright]` and `tools.browser_backend`
//! load, default and validate the way agenthicc's `CloakBrowserSettings`,
//! `PlaywrightSettings` and `ToolSettings.browser_backend` do (parity rows
//! 78-108 and 117-119).
//!
//! These pin the config half. The readers — the session manager, the CDP
//! engine and the runtime's per-session selection — are covered in
//! `crates/xaft-tools` (`browser::*` unit tests, `tests/browser_chromium.rs`)
//! and `crates/xaft-runtime`.

use tempfile::TempDir;

use xaft_config::{
    BrowserBackend, CloakBrowserConfig, ConfigLoader, PlaywrightConfig, ToolSettingsConfig,
    XaftConfig,
};

fn load(toml_text: &str) -> Result<XaftConfig, String> {
    let dir = TempDir::new().map_err(|e| e.to_string())?;
    let path = dir.path().join("xaft.toml");
    std::fs::write(&path, toml_text).map_err(|e| e.to_string())?;
    ConfigLoader::load_file(&path).map_err(|e| e.to_string())
}

/// Load and validate, as `ConfigLoader::load` does at startup.
fn load_validated(toml_text: &str) -> Result<XaftConfig, String> {
    let with_provider = format!(
        r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
{toml_text}"#
    );
    let config = load(&with_provider)?;
    xaft_config::validate(&config).map_err(|e| e.to_string())?;
    Ok(config)
}

#[test]
fn an_absent_browser_section_gives_agenthiccs_defaults() {
    let config = load_validated("").expect("load");
    assert_eq!(config.tools.browser_backend, "cloakbrowser");
    assert_eq!(
        BrowserBackend::parse(&config.tools.browser_backend),
        Some(BrowserBackend::CloakBrowser)
    );
    assert_eq!(config.tools.cloakbrowser, CloakBrowserConfig::default());
    assert_eq!(config.tools.playwright, PlaywrightConfig::default());
    // agenthicc: enabled and unrestricted by default.
    assert!(config.tools.cloakbrowser.enabled);
    assert!(config.tools.cloakbrowser.allow_all_domains);
    assert!(config.tools.cloakbrowser.allowed_domains.is_empty());
}

#[test]
fn every_cloakbrowser_key_loads_from_a_file() {
    let config = load_validated(
        r#"
[tools.cloakbrowser]
enabled = true
transport = "cdp"
cdp_endpoint = "http://localhost:9333"
allowed_domains = ["example.com", ".example.org"]
headless = false
navigation_timeout_s = 20.5
action_timeout_s = 4.0
max_pages = 2
max_actions_per_turn = 7
max_snapshot_chars = 4096
max_screenshot_bytes = 65536
allow_persistent_profiles = true
profile_root = "profiles/cloak"
license_key_env = "MY_CLOAK_KEY"
allow_all_domains = false
"#,
    )
    .expect("load");
    let c = &config.tools.cloakbrowser;
    assert_eq!(c.transport, "cdp");
    assert_eq!(c.cdp_endpoint, "http://localhost:9333");
    assert_eq!(c.allowed_domains, vec!["example.com", ".example.org"]);
    assert!(!c.headless);
    assert_eq!((c.navigation_timeout_s, c.action_timeout_s), (20.5, 4.0));
    assert_eq!((c.max_pages, c.max_actions_per_turn), (2, 7));
    assert_eq!(
        (c.max_snapshot_chars, c.max_screenshot_bytes),
        (4096, 65536)
    );
    assert!(c.allow_persistent_profiles);
    assert_eq!(c.profile_root, "profiles/cloak");
    assert_eq!(c.license_key_env, "MY_CLOAK_KEY");
    assert!(!c.allow_all_domains);
}

#[test]
fn every_playwright_key_loads_and_the_backend_is_selectable() {
    let config = load_validated(
        r#"
[tools]
browser_backend = "playwright"

[tools.playwright]
enabled = false
transport = "local"
browser_type = "chromium"
browser_channel = "chrome"
executable_path = "/opt/chromium/chrome"
allowed_domains = ["https://example.com", "https://*.example.org:8443"]
headless = true
navigation_timeout_s = 30.0
action_timeout_s = 5.0
max_pages = 3
max_actions_per_turn = 9
max_snapshot_chars = 1000
max_screenshot_bytes = 2048
allow_persistent_profiles = true
profile_root = ".xaft/pw"
allow_all_domains = false
"#,
    )
    .expect("load");
    assert_eq!(
        BrowserBackend::parse(&config.tools.browser_backend),
        Some(BrowserBackend::Playwright)
    );
    let p = &config.tools.playwright;
    assert!(!p.enabled);
    assert_eq!(p.browser_channel, "chrome");
    assert_eq!(p.executable_path, "/opt/chromium/chrome");
    assert_eq!(p.allowed_domains.len(), 2);
    assert_eq!((p.max_pages, p.max_actions_per_turn), (3, 9));
    assert_eq!(p.profile_root, ".xaft/pw");
    assert!(!p.allow_all_domains);
}

#[test]
fn an_invalid_backend_fails_loading() {
    let err = load_validated("[tools]\nbrowser_backend = \"chrome\"\n").unwrap_err();
    assert!(
        err.contains("tools.browser_backend")
            && err.contains("'cloakbrowser', 'playwright', or 'none'"),
        "{err}"
    );
}

/// agenthicc validates both sections whatever the backend: a bad Playwright
/// value fails loading even with CloakBrowser selected.
#[test]
fn both_sections_are_validated_whatever_the_backend() {
    let err = load_validated("[tools.playwright]\nmax_pages = 0\n").unwrap_err();
    assert!(err.contains("tools.playwright.max_pages"), "{err}");
    let err = load_validated("[tools.cloakbrowser]\nprofile_root = \"../out\"\n").unwrap_err();
    assert!(err.contains("relative path inside the workspace"), "{err}");
}

#[test]
fn the_cdp_endpoint_must_be_loopback_when_used() {
    let err = load_validated(
        "[tools.cloakbrowser]\ntransport = \"cdp\"\ncdp_endpoint = \"http://remote.example:9222\"\n",
    )
    .unwrap_err();
    assert!(err.contains("must use a loopback host"), "{err}");
    load_validated("[tools.cloakbrowser]\ncdp_endpoint = \"http://remote.example:9222\"\n")
        .expect("only validated for the cdp transport");
}

/// A malformed allow-list entry does not stop xaft from starting: agenthicc
/// makes the browser tools report `policy_denied` instead.
#[test]
fn a_malformed_allow_list_still_loads() {
    let config = load_validated("[tools.cloakbrowser]\nallowed_domains = [\"*\"]\n").expect("load");
    assert_eq!(config.tools.cloakbrowser.allowed_domains, vec!["*"]);
}

#[test]
fn a_pasted_licence_key_is_refused_without_being_echoed() {
    let err = load_validated("[tools.cloakbrowser]\nlicense_key_env = \"cb-live-9f8e7d6c\"\n")
        .unwrap_err();
    assert!(err.contains("license_key_env"), "{err}");
    assert!(!err.contains("cb-live-9f8e7d6c"), "{err}");
}

#[test]
fn defaults_are_not_written_back_and_set_values_round_trip() {
    let config = load_validated("").expect("load");
    let rendered = toml::to_string(&config.tools).expect("render");
    for absent in ["cloakbrowser", "playwright", "browser_backend"] {
        assert!(
            !rendered.contains(absent),
            "{absent} must not be written: {rendered}"
        );
    }

    let config =
        load_validated("[tools]\nbrowser_backend = \"none\"\n[tools.playwright]\nmax_pages = 2\n")
            .expect("load");
    let rendered = toml::to_string(&config.tools).expect("render");
    let back: ToolSettingsConfig = toml::from_str(&rendered).expect("re-parse");
    assert_eq!(back.browser_backend, "none");
    assert_eq!(back.playwright.max_pages, 2);
    assert_eq!(back.cloakbrowser, CloakBrowserConfig::default());
}
