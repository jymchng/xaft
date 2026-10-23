//! The transport contract between the session manager and a browser.
//!
//! Port of the data types and `BrowserClient` protocol in agenthicc's
//! `tools/cloakbrowser/client.py`. The manager (`session.rs`) owns every
//! policy decision — quotas, idempotency, bounding, redaction — and talks to
//! the browser only through this trait, so its contract is unit-tested
//! against a fake client while the real implementation
//! ([`super::engine::CdpBrowserClient`]) is exercised against a real Chromium.

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::errors::{BrowserErrorKind, BrowserFailure};
use super::policy::redact_url;

/// Truncate to at most `max` characters (Python `text[:max]`).
pub fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((byte, _)) => text[..byte].to_string(),
        None => text.to_string(),
    }
}

/// The backend's readiness, as `*_status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserHealth {
    /// `ready` or an error-kind wire value.
    pub status: String,
    /// `local` or `cdp`.
    pub transport: String,
    /// A safe explanation; empty when ready.
    pub message: String,
}

impl BrowserHealth {
    /// A ready backend.
    pub fn ready(transport: &str) -> Self {
        Self {
            status: "ready".into(),
            transport: transport.into(),
            message: String::new(),
        }
    }

    /// An unavailable backend.
    pub fn failed(kind: BrowserErrorKind, transport: &str, message: impl Into<String>) -> Self {
        Self {
            status: kind.as_str().into(),
            transport: transport.into(),
            message: message.into(),
        }
    }

    /// agenthicc `BrowserHealth.to_dict`.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("status".into(), json!(self.status));
        map.insert("transport".into(), json!(self.transport));
        map.insert("message".into(), json!(self.message));
        map
    }
}

/// An open page: its id, URL and title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageState {
    /// The opaque page id the agent passes back.
    pub page_id: String,
    /// The page's URL (redacted on output).
    pub url: String,
    /// The page's title (bounded on output).
    pub title: String,
}

impl PageState {
    /// Build a page state.
    pub fn new(
        page_id: impl Into<String>,
        url: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            page_id: page_id.into(),
            url: url.into(),
            title: title.into(),
        }
    }

    /// agenthicc `PageState.to_dict`: redacted URL, title bounded to 500
    /// characters.
    pub fn to_json(&self) -> Value {
        json!({
            "page_id": self.page_id,
            "url": redact_url(&self.url),
            "title": truncate_chars(&self.title, 500),
        })
    }
}

/// A bounded page representation (agenthicc `PageSnapshot`).
#[derive(Debug, Clone, PartialEq)]
pub struct PageSnapshot {
    /// The page the snapshot was taken from.
    pub page: PageState,
    /// The page's visible text.
    pub text: String,
    /// Links: `href` (redacted), `text`, and xaft's `selector`.
    pub links: Vec<Map<String, Value>>,
    /// Form controls: `aria-label`, `text`, and xaft's `selector`, `tag`, ...
    pub controls: Vec<Map<String, Value>>,
    /// Whether the client already cut the text short.
    pub truncated: bool,
}

impl PageSnapshot {
    /// agenthicc `PageSnapshot.to_dict`. `untrusted: true` marks page content
    /// the agent must not treat as instructions.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("page".into(), self.page.to_json());
        map.insert("untrusted".into(), json!(true));
        map.insert("text".into(), json!(self.text));
        map.insert(
            "links".into(),
            Value::Array(self.links.iter().cloned().map(Value::Object).collect()),
        );
        map.insert(
            "controls".into(),
            Value::Array(self.controls.iter().cloned().map(Value::Object).collect()),
        );
        map.insert("truncated".into(), json!(self.truncated));
        map
    }
}

/// Raw screenshot bytes and their MIME type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotData {
    /// The encoded image.
    pub content: Vec<u8>,
    /// `image/png` or `image/jpeg`.
    pub mime_type: &'static str,
}

/// What a browser transport must provide (agenthicc `BrowserClient`).
///
/// agenthicc passes an opaque `session_id` to every call because one client
/// object can serve several sessions; in xaft a client belongs to exactly one
/// manager, so the parameter is dropped.
#[async_trait]
pub trait BrowserClient: Send + Sync {
    /// Readiness. Must never fail and never leak detail.
    async fn health(&self) -> BrowserHealth;
    /// Open a page and navigate it to `url` (already policy-checked).
    async fn open_page(&self, url: &str) -> Result<PageState, BrowserFailure>;
    /// A bounded snapshot of an open page.
    async fn snapshot(&self, page_id: &str) -> Result<PageSnapshot, BrowserFailure>;
    /// Click the first element matching `selector`.
    async fn click(&self, page_id: &str, selector: &str) -> Result<PageState, BrowserFailure>;
    /// Fill the first element matching `selector` with `value`.
    async fn fill(
        &self,
        page_id: &str,
        selector: &str,
        value: &str,
    ) -> Result<PageState, BrowserFailure>;
    /// Focus `selector` and press `key`.
    async fn press(
        &self,
        page_id: &str,
        key: &str,
        selector: &str,
    ) -> Result<PageState, BrowserFailure>;
    /// Wait for a condition (`selector`, `text`, `url`, `load_state`).
    async fn wait_for(
        &self,
        page_id: &str,
        condition: &str,
        value: &str,
    ) -> Result<PageState, BrowserFailure>;
    /// Capture a screenshot (`png` or `jpeg`).
    async fn screenshot(
        &self,
        page_id: &str,
        image_type: &str,
        full_page: bool,
    ) -> Result<ScreenshotData, BrowserFailure>;
    /// Close one page.
    async fn close_page(&self, page_id: &str) -> Result<(), BrowserFailure>;
    /// Close every page and release the browser.
    async fn close_session(&self) -> Result<(), BrowserFailure>;
}

/// The client used when a backend is disabled or cannot be configured
/// (agenthicc `UnavailableBrowserClient`).
#[derive(Debug, Clone)]
pub struct UnavailableBrowserClient {
    health: BrowserHealth,
    kind: BrowserErrorKind,
}

impl UnavailableBrowserClient {
    /// A client that reports `kind` with `message` for every call.
    pub fn new(transport: &str, kind: BrowserErrorKind, message: impl Into<String>) -> Self {
        Self {
            health: BrowserHealth::failed(kind, transport, message),
            kind,
        }
    }

    fn fail<T>(&self) -> Result<T, BrowserFailure> {
        Err(BrowserFailure::tool(self.kind, self.health.message.clone()))
    }
}

#[async_trait]
impl BrowserClient for UnavailableBrowserClient {
    async fn health(&self) -> BrowserHealth {
        self.health.clone()
    }
    async fn open_page(&self, _url: &str) -> Result<PageState, BrowserFailure> {
        self.fail()
    }
    async fn snapshot(&self, _page_id: &str) -> Result<PageSnapshot, BrowserFailure> {
        self.fail()
    }
    async fn click(&self, _p: &str, _s: &str) -> Result<PageState, BrowserFailure> {
        self.fail()
    }
    async fn fill(&self, _p: &str, _s: &str, _v: &str) -> Result<PageState, BrowserFailure> {
        self.fail()
    }
    async fn press(&self, _p: &str, _k: &str, _s: &str) -> Result<PageState, BrowserFailure> {
        self.fail()
    }
    async fn wait_for(&self, _p: &str, _c: &str, _v: &str) -> Result<PageState, BrowserFailure> {
        self.fail()
    }
    async fn screenshot(
        &self,
        _p: &str,
        _t: &str,
        _f: bool,
    ) -> Result<ScreenshotData, BrowserFailure> {
        self.fail()
    }
    async fn close_page(&self, _page_id: &str) -> Result<(), BrowserFailure> {
        self.fail()
    }
    async fn close_session(&self) -> Result<(), BrowserFailure> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("", 3), "");
    }

    #[test]
    fn page_state_output_is_redacted_and_bounded() {
        let state = PageState::new("p1", "https://u:p@example.com/a?token=x#f", "t".repeat(600));
        let json = state.to_json();
        assert_eq!(json["url"], "https://example.com/a");
        assert_eq!(json["title"].as_str().unwrap().chars().count(), 500);
    }

    #[tokio::test]
    async fn an_unavailable_client_refuses_everything_but_closing() {
        let client = UnavailableBrowserClient::new(
            "local",
            BrowserErrorKind::Disabled,
            "Playwright is disabled; enable the selected browser backend.",
        );
        let health = client.health().await;
        assert_eq!(health.status, "not_configured");
        let err = client.open_page("https://example.com/").await.unwrap_err();
        assert_eq!(
            err,
            BrowserFailure::tool(
                BrowserErrorKind::Disabled,
                "Playwright is disabled; enable the selected browser backend."
            )
        );
        client.close_session().await.unwrap();
    }
}
