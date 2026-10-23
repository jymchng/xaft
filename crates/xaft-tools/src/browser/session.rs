//! Session-scoped browser lifecycle, quotas, idempotency and redaction.
//!
//! Port of agenthicc's `BrowserSessionManager`
//! (`tools/cloakbrowser/session.py`). The manager is the *only* object the
//! eighteen browser tools hold, which is what keeps a model from choosing a
//! transport, an endpoint, a profile path or a raw CDP command: every tool
//! call is one of nine methods here, and each method enforces —
//!
//! * the backend switch, the allow-list and the browser's health;
//! * the per-turn action budget (`max_actions_per_turn`, reset each turn);
//! * the open-page ceiling (`max_pages`);
//! * at-most-once execution per `operation_id` (a retried mutation replays
//!   its first result instead of clicking twice);
//! * bounded, redacted output (`max_snapshot_chars`, 50 links/controls of at
//!   most 500 characters, URLs without credentials, queries or fragments);
//! * agenthicc's error mapping, which never lets an exception's text reach
//!   the agent.
//!
//! agenthicc's `checkpoint_payload` / `restore_checkpoint` are not ported:
//! they serve agenthicc's workflow checkpoints, which have no xaft
//! counterpart. Browser state is process-local in both, and nothing a
//! checkpoint held is part of the tool contract.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;
use xaft_config::{BrowserBackend, ToolSettingsConfig};

use super::artifacts::{BrowserArtifactStore, is_safe_id};
use super::client::{BrowserClient, PageState, UnavailableBrowserClient, truncate_chars};
use super::engine::{BrowserLaunchOptions, CdpBrowserClient};
use super::errors::{BrowserErrorKind, BrowserFailure, BrowserToolError};
use super::policy::{BrowserPolicy, redact_link};
use super::settings::{BrowserFamily, BrowserSettings};

/// agenthicc `_OPERATION_ID_LIMIT`.
const OPERATION_ID_LIMIT: usize = 128;
/// agenthicc `_OPERATION_CACHE_LIMIT`.
const OPERATION_CACHE_LIMIT: usize = 256;

#[derive(Debug, Default)]
struct ManagerState {
    closed: bool,
    actions_used: usize,
    pages: HashMap<String, PageState>,
    page_order: Vec<String>,
    in_flight: HashSet<String>,
    results: HashMap<String, Value>,
    result_order: VecDeque<String>,
}

impl ManagerState {
    fn insert_page(&mut self, page: PageState) {
        if !self.pages.contains_key(&page.page_id) {
            self.page_order.push(page.page_id.clone());
        }
        self.pages.insert(page.page_id.clone(), page);
    }

    fn remove_page(&mut self, page_id: &str) {
        self.pages.remove(page_id);
        self.page_order.retain(|id| id != page_id);
    }

    fn clear_pages(&mut self) {
        self.pages.clear();
        self.page_order.clear();
    }

    fn cache_result(&mut self, operation_id: &str, result: &Value) {
        if self
            .results
            .insert(operation_id.to_string(), result.clone())
            .is_none()
        {
            self.result_order.push_back(operation_id.to_string());
        }
        while self.result_order.len() > OPERATION_CACHE_LIMIT {
            if let Some(oldest) = self.result_order.pop_front() {
                self.results.remove(&oldest);
            }
        }
    }

    fn clear_operations(&mut self) {
        self.in_flight.clear();
        self.results.clear();
        self.result_order.clear();
    }
}

/// Owns exactly one browser context for one xaft session.
pub struct BrowserSessionManager {
    settings: BrowserSettings,
    conversation_id: String,
    client: Arc<dyn BrowserClient>,
    policy: Arc<BrowserPolicy>,
    policy_error: Option<String>,
    artifacts: BrowserArtifactStore,
    browser_session_id: String,
    state: Mutex<ManagerState>,
    op_lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for BrowserSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSessionManager")
            .field("family", &self.settings.family)
            .field("transport", &self.settings.transport)
            .field("session_id", &self.browser_session_id)
            .finish_non_exhaustive()
    }
}

/// Map one failure to agenthicc's structured error result.
fn failure_result(failure: BrowserFailure, backend: &str) -> Value {
    match failure {
        BrowserFailure::Tool(e) => error_result(e.kind, &e.message),
        BrowserFailure::Timeout => {
            error_result(BrowserErrorKind::Timeout, "Browser operation timed out.")
        }
        BrowserFailure::Network => error_result(
            BrowserErrorKind::Network,
            "Browser network operation failed.",
        ),
        BrowserFailure::Execution(category) => {
            // Only the category: an error's text can carry page content or
            // credentials (agenthicc logs the exception class, never its value).
            tracing::debug!(backend, category, "browser operation failed");
            error_result(BrowserErrorKind::Execution, "Browser operation failed.")
        }
    }
}

fn error_result(kind: BrowserErrorKind, message: &str) -> Value {
    json!({ "ok": false, "error_kind": kind.as_str(), "error": message })
}

fn cancelled_result() -> Value {
    error_result(
        BrowserErrorKind::Cancelled,
        "Browser operation was cancelled; the affected session was closed.",
    )
}

fn set_default(result: &mut Value, key: &str, value: Value) {
    if let Some(object) = result.as_object_mut() {
        object.entry(key.to_string()).or_insert(value);
    }
}

/// agenthicc `_result_page`.
fn result_page(page: &PageState, page_id: &str) -> Value {
    json!({
        "ok": true,
        "page": PageState::new(page_id, page.url.clone(), page.title.clone()).to_json(),
    })
}

fn tool_error(kind: BrowserErrorKind, message: impl Into<String>) -> BrowserFailure {
    BrowserFailure::Tool(BrowserToolError::new(kind, message))
}

/// Disarmed on normal completion; otherwise (the tool future was dropped
/// mid-operation) it performs agenthicc's cancellation cleanup.
struct CancelGuard<'a> {
    manager: &'a BrowserSessionManager,
    operation_id: Option<String>,
    armed: bool,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.manager
            .record_cancellation(self.operation_id.as_deref());
        let client = Arc::clone(&self.manager.client);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = client.close_session().await;
            });
        }
    }
}

impl BrowserSessionManager {
    /// A manager for `settings`, launching the real engine on first use.
    ///
    /// `session_hint` keys persistent profiles and screenshot artifacts. A
    /// safe xaft session id (`[A-Za-z0-9_-]{1,128}`) is used as given, so a
    /// resumed session finds its profile again — agenthicc rebinds its opaque
    /// id on checkpoint restore to the same end. Anything else gets a fresh
    /// opaque id.
    pub fn new(
        settings: BrowserSettings,
        conversation_id: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        session_hint: Option<&str>,
        options: BrowserLaunchOptions,
    ) -> Self {
        let workspace_root = workspace_root.into();
        let browser_session_id = session_hint
            .filter(|id| is_safe_id(id))
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let (policy, policy_error) = Self::build_policy(&settings);
        let backend = settings.family.backend_name();
        let client: Arc<dyn BrowserClient> = if !settings.enabled {
            Arc::new(UnavailableBrowserClient::new(
                settings.transport_label(),
                BrowserErrorKind::Disabled,
                format!("{backend} is disabled; enable the selected browser backend."),
            ))
        } else if policy_error.is_some() && settings.family == BrowserFamily::Playwright {
            Arc::new(UnavailableBrowserClient::new(
                "local",
                BrowserErrorKind::PolicyDenied,
                "Playwright browser policy configuration is invalid.",
            ))
        } else {
            Arc::new(CdpBrowserClient::new(
                settings.clone(),
                Arc::clone(&policy),
                workspace_root.clone(),
                browser_session_id.clone(),
                options,
            ))
        };
        Self::assemble(
            settings,
            conversation_id.into(),
            workspace_root,
            client,
            policy,
            policy_error,
            browser_session_id,
        )
    }

    /// A manager over an explicit client and (optionally) policy — the
    /// `client=` / `policy=` seams of agenthicc's constructor, used by tests
    /// and embedders.
    pub fn with_client(
        settings: BrowserSettings,
        conversation_id: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        client: Arc<dyn BrowserClient>,
        policy: Option<BrowserPolicy>,
    ) -> Self {
        let (policy, policy_error) = match policy {
            Some(p) => (Arc::new(p), None),
            None => Self::build_policy(&settings),
        };
        Self::assemble(
            settings,
            conversation_id.into(),
            workspace_root.into(),
            client,
            policy,
            policy_error,
            uuid::Uuid::new_v4().simple().to_string(),
        )
    }

    /// The manager a session gets from `[tools]`, or `None` when
    /// `browser_backend = "none"` — agenthicc builds no browser manager then.
    pub fn from_tool_settings(
        tools: &ToolSettingsConfig,
        workspace_root: impl Into<PathBuf>,
        session_id: Option<&str>,
        options: BrowserLaunchOptions,
    ) -> Option<Arc<Self>> {
        let settings = match BrowserBackend::parse(&tools.browser_backend)? {
            BrowserBackend::CloakBrowser => BrowserSettings::from_cloakbrowser(&tools.cloakbrowser),
            BrowserBackend::Playwright => BrowserSettings::from_playwright(&tools.playwright),
            BrowserBackend::None => return None,
        };
        Some(Arc::new(Self::new(
            settings,
            session_id.unwrap_or("session"),
            workspace_root,
            session_id,
            options,
        )))
    }

    fn assemble(
        settings: BrowserSettings,
        conversation_id: String,
        workspace_root: PathBuf,
        client: Arc<dyn BrowserClient>,
        policy: Arc<BrowserPolicy>,
        policy_error: Option<String>,
        browser_session_id: String,
    ) -> Self {
        Self {
            settings,
            conversation_id: if conversation_id.is_empty() {
                "session".into()
            } else {
                conversation_id
            },
            client,
            policy,
            policy_error,
            artifacts: BrowserArtifactStore::new(workspace_root),
            browser_session_id,
            state: Mutex::new(ManagerState::default()),
            op_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The family policy, or agenthicc's deny-all stand-in plus the reason.
    fn build_policy(settings: &BrowserSettings) -> (Arc<BrowserPolicy>, Option<String>) {
        let built = match settings.family {
            BrowserFamily::CloakBrowser => {
                BrowserPolicy::cloakbrowser(&settings.allowed_domains, settings.allow_all_domains)
            }
            BrowserFamily::Playwright => {
                BrowserPolicy::playwright(&settings.allowed_domains, settings.allow_all_domains)
            }
        };
        match built {
            Ok(policy) => (Arc::new(policy), None),
            Err(error) => {
                tracing::debug!(%error, "browser allow-list is invalid; the backend is unavailable");
                (
                    Arc::new(BrowserPolicy::deny_all()),
                    settings.enabled.then(|| error.to_string()),
                )
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ManagerState> {
        self.state.lock().expect("browser session state poisoned")
    }

    /// The settings this session runs under.
    pub fn settings(&self) -> &BrowserSettings {
        &self.settings
    }

    /// The tool family.
    pub fn family(&self) -> BrowserFamily {
        self.settings.family
    }

    /// The opaque id keying artifacts and persistent profiles.
    pub fn session_id(&self) -> &str {
        &self.browser_session_id
    }

    /// The conversation this manager belongs to.
    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    /// The destination policy in force.
    pub fn policy(&self) -> &BrowserPolicy {
        &self.policy
    }

    /// agenthicc `enabled`: switched on, a valid allow-list, not closed.
    pub fn enabled(&self) -> bool {
        self.settings.enabled && self.policy_error.is_none() && !self.lock().closed
    }

    /// Actions consumed this turn.
    pub fn actions_used(&self) -> usize {
        self.lock().actions_used
    }

    /// Ids of the pages currently open, oldest first.
    pub fn open_page_ids(&self) -> Vec<String> {
        self.lock().page_order.clone()
    }

    /// Reset the action quota at a new agent turn boundary.
    pub fn reset_turn_budget(&self) {
        self.lock().actions_used = 0;
    }

    fn backend(&self) -> &'static str {
        self.settings.family.backend_name()
    }

    fn transport(&self) -> &'static str {
        self.settings.transport_label()
    }

    /// agenthicc `status`: readiness and bounded session counters.
    pub async fn status(&self) -> Value {
        let transport = self.transport();
        if self.lock().closed {
            return json!({
                "ok": false,
                "status": BrowserErrorKind::Closed.as_str(),
                "transport": transport,
                "session_id": self.browser_session_id,
            });
        }
        if self.policy_error.is_some() {
            return json!({
                "ok": false,
                "status": BrowserErrorKind::PolicyDenied.as_str(),
                "transport": transport,
                "session_id": self.browser_session_id,
                "message": "Browser allow-list configuration is invalid.",
            });
        }
        if self.policy.allowed_domains().is_empty() && !self.policy.allow_all_domains() {
            return json!({
                "ok": false,
                "status": BrowserErrorKind::Disabled.as_str(),
                "transport": transport,
                "session_id": self.browser_session_id,
                "message": format!("Configure at least one allowed {} origin.", self.backend()),
            });
        }
        let health = self.client.health().await;
        let mut result: Map<String, Value> = health.to_json();
        let (open_pages, actions_used) = {
            let state = self.lock();
            (state.pages.len(), state.actions_used)
        };
        result.insert("ok".into(), json!(health.status == "ready"));
        result.insert("session_id".into(), json!(self.browser_session_id));
        result.insert("open_pages".into(), json!(open_pages));
        result.insert("max_pages".into(), json!(self.settings.max_pages));
        result.insert("actions_used".into(), json!(actions_used));
        result.insert(
            "max_actions_per_turn".into(),
            json!(self.settings.max_actions_per_turn),
        );
        Value::Object(result)
    }

    fn ensure_usable(&self) -> Result<(), BrowserFailure> {
        if self.lock().closed {
            return Err(tool_error(
                BrowserErrorKind::Closed,
                "Browser session is closed.",
            ));
        }
        if !self.settings.enabled {
            return Err(tool_error(
                BrowserErrorKind::Disabled,
                format!("{} is disabled.", self.backend()),
            ));
        }
        if self.policy_error.is_some() {
            return Err(tool_error(
                BrowserErrorKind::PolicyDenied,
                "Browser allow-list configuration is invalid.",
            ));
        }
        if self.policy.allowed_domains().is_empty() && !self.policy.allow_all_domains() {
            return Err(tool_error(
                BrowserErrorKind::PolicyDenied,
                format!(
                    "No {} destinations are configured in the allow-list.",
                    self.backend()
                ),
            ));
        }
        Ok(())
    }

    /// Re-check dependency, binary or endpoint health before an operation.
    async fn ensure_ready(&self) -> Result<(), BrowserFailure> {
        let health = self.client.health().await;
        if health.status == "ready" {
            return Ok(());
        }
        let kind = match BrowserErrorKind::from_wire(&health.status) {
            Some(BrowserErrorKind::BinaryMissing | BrowserErrorKind::Unhealthy) | None => {
                BrowserErrorKind::BrowserUnavailable
            }
            Some(kind) => kind,
        };
        let message = if health.message.is_empty() {
            "Browser backend is unavailable.".to_string()
        } else {
            health.message
        };
        Err(tool_error(kind, message))
    }

    /// agenthicc `_validate_operation_id`.
    fn validate_operation_id(
        operation_id: Option<&str>,
    ) -> Result<Option<String>, BrowserToolError> {
        let Some(raw) = operation_id else {
            return Ok(None);
        };
        let candidate = raw.trim();
        if candidate.is_empty() {
            return Ok(None);
        }
        let valid_chars = candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'));
        if candidate.chars().count() > OPERATION_ID_LIMIT || !valid_chars {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "Operation id is invalid or too long.",
            ));
        }
        Ok(Some(candidate.to_string()))
    }

    fn page(&self, page_id: &str) -> Result<PageState, BrowserFailure> {
        self.lock().pages.get(page_id).cloned().ok_or_else(|| {
            tool_error(
                BrowserErrorKind::NOT_FOUND,
                "Browser page is no longer open.",
            )
        })
    }

    fn consume_action(&self) -> Result<(), BrowserFailure> {
        let mut state = self.lock();
        if state.actions_used >= self.settings.max_actions_per_turn {
            return Err(tool_error(
                BrowserErrorKind::LimitExceeded,
                "Browser action limit reached for this turn.",
            ));
        }
        state.actions_used += 1;
        Ok(())
    }

    /// Drop a page the browser reports gone (crashed, closed by its own
    /// script, or lost with a relaunched browser) so it stops holding a
    /// `max_pages` slot. agenthicc keeps such a page listed until the session
    /// ends, and its `close` cannot remove it either — the client raises
    /// before the manager forgets it.
    fn forget_if_stale<T>(
        &self,
        page_id: &str,
        outcome: Result<T, BrowserFailure>,
    ) -> Result<T, BrowserFailure> {
        if let Err(BrowserFailure::Tool(e)) = &outcome {
            if e.kind == BrowserErrorKind::StalePage {
                self.lock().remove_page(page_id);
            }
        }
        outcome
    }

    fn store_page(&self, page_id: &str, page: &PageState) {
        let mut state = self.lock();
        state.insert_page(PageState::new(
            page_id,
            page.url.clone(),
            page.title.clone(),
        ));
    }

    fn record_cancellation(&self, operation_id: Option<&str>) {
        let mut state = self.lock();
        state.clear_pages();
        if let Some(id) = operation_id {
            state.in_flight.remove(id);
            let mut result = cancelled_result();
            set_default(&mut result, "operation_id", json!(id));
            state.cache_result(id, &result);
        }
    }

    /// agenthicc `_safe`: operation-id idempotency, serialisation, and the
    /// mapping of every failure to a structured result.
    async fn safe<F, Fut>(
        &self,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
        operation: F,
    ) -> Value
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, BrowserFailure>>,
    {
        let normalized = match Self::validate_operation_id(operation_id) {
            Ok(id) => id,
            Err(e) => return error_result(e.kind, &e.message),
        };
        if let Some(id) = &normalized {
            let mut state = self.lock();
            if let Some(cached) = state.results.get(id) {
                return cached.clone();
            }
            if state.in_flight.contains(id) {
                return json!({
                    "ok": false,
                    "error_kind": BrowserErrorKind::Execution.as_str(),
                    "error": "Operation is already in progress; do not retry it yet.",
                    "operation_id": id,
                });
            }
            state.in_flight.insert(id.clone());
        }

        let mut guard = CancelGuard {
            manager: self,
            operation_id: normalized.clone(),
            armed: true,
        };
        let run = async {
            let _serial = self.op_lock.lock().await;
            operation().await
        };
        let outcome = match cancel {
            Some(token) => tokio::select! {
                biased;
                _ = token.cancelled() => None,
                outcome = run => Some(outcome),
            },
            None => Some(run.await),
        };
        guard.armed = false;
        drop(guard);

        let mut result = match outcome {
            Some(Ok(value)) => value,
            Some(Err(failure)) => failure_result(failure, self.backend()),
            None => {
                // A cancelled mutation must not be retried against an
                // uncertain page: close the owned context, as agenthicc does.
                {
                    let _serial = self.op_lock.lock().await;
                    let _ = self.client.close_session().await;
                }
                self.lock().clear_pages();
                cancelled_result()
            }
        };
        if let Some(id) = &normalized {
            set_default(&mut result, "operation_id", json!(id));
            let mut state = self.lock();
            state.in_flight.remove(id);
            state.cache_result(id, &result);
        }
        let fallback = normalized.unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        set_default(&mut result, "operation_id", json!(fallback));
        result
    }

    /// agenthicc `open`: also the re-entry point after `close_session`.
    pub async fn open(
        &self,
        url: &str,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Value {
        self.safe(operation_id, cancel, || async move {
            {
                let mut state = self.lock();
                if state.closed {
                    state.closed = false;
                    state.clear_pages();
                    state.clear_operations();
                }
            }
            self.ensure_usable()?;
            self.ensure_ready().await?;
            if self.lock().pages.len() >= self.settings.max_pages {
                return Err(tool_error(
                    BrowserErrorKind::LimitExceeded,
                    "Maximum open browser pages reached.",
                ));
            }
            self.policy.validate_url(url).await?;
            let page = self.client.open_page(url).await?;
            let page_id = page.page_id.clone();
            self.store_page(&page_id, &page);
            Ok(result_page(&page, &page_id))
        })
        .await
    }

    /// agenthicc `snapshot`: bounded page text, links and controls.
    pub async fn snapshot(
        &self,
        page_id: &str,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Value {
        self.safe(operation_id, cancel, || async move {
            self.ensure_usable()?;
            self.ensure_ready().await?;
            self.page(page_id)?;
            let snapshot = self.forget_if_stale(page_id, self.client.snapshot(page_id).await)?;
            self.store_page(page_id, &snapshot.page);
            let max = self.settings.max_snapshot_chars;
            let mut bounded = snapshot.to_json();
            let text = bounded
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let truncated = snapshot.truncated || text.chars().count() > max;
            bounded.insert("truncated".into(), json!(truncated));
            bounded.insert("text".into(), json!(truncate_chars(&text, max)));
            for key in ["links", "controls"] {
                let items = bounded
                    .get(key)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let safe: Vec<Value> = items
                    .into_iter()
                    .take(50)
                    .filter_map(|item| item.as_object().cloned())
                    .map(|item| {
                        let mut out = Map::new();
                        for (name, value) in item {
                            let text = match value {
                                Value::String(s) => s,
                                other => other.to_string(),
                            };
                            out.insert(name, Value::String(truncate_chars(&text, 500)));
                        }
                        if key == "links" {
                            if let Some(Value::String(href)) = out.get("href").cloned() {
                                out.insert("href".into(), Value::String(redact_link(&href)));
                            }
                        }
                        Value::Object(out)
                    })
                    .collect();
                bounded.insert(key.into(), Value::Array(safe));
            }
            Ok(json!({ "ok": true, "snapshot": Value::Object(bounded) }))
        })
        .await
    }

    /// agenthicc `click`.
    pub async fn click(
        &self,
        page_id: &str,
        selector: &str,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Value {
        self.safe(operation_id, cancel, || async move {
            self.ensure_usable()?;
            self.ensure_ready().await?;
            let selector = self.policy.validate_selector(selector)?;
            self.page(page_id)?;
            self.consume_action()?;
            let page =
                self.forget_if_stale(page_id, self.client.click(page_id, &selector).await)?;
            self.store_page(page_id, &page);
            Ok(result_page(&page, page_id))
        })
        .await
    }

    /// agenthicc `fill`: never echoes the value, not even redacted.
    pub async fn fill(
        &self,
        page_id: &str,
        selector: &str,
        value: &str,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Value {
        self.safe(operation_id, cancel, || async move {
            self.ensure_usable()?;
            self.ensure_ready().await?;
            let (selector, value) = self.policy.validate_value(selector, value)?;
            self.consume_action()?;
            let page =
                self.forget_if_stale(page_id, self.client.fill(page_id, &selector, &value).await)?;
            self.store_page(page_id, &page);
            Ok(result_page(&page, page_id))
        })
        .await
    }

    /// agenthicc `press`.
    pub async fn press(
        &self,
        page_id: &str,
        key: &str,
        selector: &str,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Value {
        self.safe(operation_id, cancel, || async move {
            self.ensure_usable()?;
            self.ensure_ready().await?;
            let key = self.policy.validate_key(key)?;
            let selector = self.policy.validate_selector(selector)?;
            self.consume_action()?;
            let page =
                self.forget_if_stale(page_id, self.client.press(page_id, &key, &selector).await)?;
            self.store_page(page_id, &page);
            Ok(result_page(&page, page_id))
        })
        .await
    }

    /// agenthicc `wait_for`.
    pub async fn wait_for(
        &self,
        page_id: &str,
        condition: &str,
        value: &str,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Value {
        self.safe(operation_id, cancel, || async move {
            self.ensure_usable()?;
            self.ensure_ready().await?;
            let (condition, value) = self.policy.validate_wait(condition, value)?;
            self.page(page_id)?;
            if condition == "url" {
                self.policy.validate_url(&value).await?;
            }
            let page = self.forget_if_stale(
                page_id,
                self.client.wait_for(page_id, &condition, &value).await,
            )?;
            self.store_page(page_id, &page);
            Ok(result_page(&page, page_id))
        })
        .await
    }

    /// agenthicc `screenshot`: stored below the workspace artifact root.
    pub async fn screenshot(
        &self,
        page_id: &str,
        image_type: &str,
        full_page: bool,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Value {
        self.safe(operation_id, cancel, || async move {
            self.ensure_usable()?;
            self.ensure_ready().await?;
            self.page(page_id)?;
            let image_type = image_type.trim().to_lowercase();
            if image_type != "png" && image_type != "jpeg" {
                return Err(tool_error(
                    BrowserErrorKind::InvalidInput,
                    "Screenshot format must be png or jpeg.",
                ));
            }
            let shot = self.forget_if_stale(
                page_id,
                self.client
                    .screenshot(page_id, &image_type, full_page)
                    .await,
            )?;
            if shot.content.len() > self.settings.max_screenshot_bytes {
                return Err(tool_error(
                    BrowserErrorKind::OutputLimit,
                    "Screenshot exceeds the configured size limit.",
                ));
            }
            let artifact = self
                .artifacts
                .write_screenshot(&self.browser_session_id, &shot.content, shot.mime_type)
                .map_err(|error| {
                    tracing::debug!(%error, "screenshot artifact could not be stored");
                    BrowserFailure::Execution("artifact write failed")
                })?;
            Ok(json!({ "ok": true, "artifact": artifact.to_json() }))
        })
        .await
    }

    /// agenthicc `close`: one page, or every page.
    pub async fn close(
        &self,
        page_id: &str,
        all_pages: bool,
        operation_id: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Value {
        self.safe(operation_id, cancel, || async move {
            self.ensure_usable()?;
            self.ensure_ready().await?;
            if all_pages {
                for current in self.open_page_ids() {
                    // A page that is already gone is as closed as it gets.
                    match self.client.close_page(&current).await {
                        Err(BrowserFailure::Tool(e)) if e.kind == BrowserErrorKind::StalePage => {}
                        other => other?,
                    }
                    self.lock().remove_page(&current);
                }
                return Ok(json!({ "ok": true, "closed_pages": "all" }));
            }
            let page = self.page(page_id)?;
            self.forget_if_stale(&page.page_id, self.client.close_page(&page.page_id).await)?;
            self.lock().remove_page(&page.page_id);
            Ok(json!({ "ok": true, "closed_page": page.page_id }))
        })
        .await
    }

    /// Release the live browser but keep the manager usable — agenthicc's
    /// recovery after a failed turn. A terminal `close_session` wins.
    pub async fn close_runtime(&self) {
        let _serial = self.op_lock.lock().await;
        if self.lock().closed {
            return;
        }
        let _ = self.client.close_session().await;
        let mut state = self.lock();
        state.clear_pages();
        state.actions_used = 0;
        state.clear_operations();
        state.closed = false;
    }

    /// Terminal cleanup when the owning session ends. `open` still reopens.
    pub async fn close_session(&self) {
        if self.lock().closed {
            return;
        }
        let _serial = self.op_lock.lock().await;
        let _ = self.client.close_session().await;
        let mut state = self.lock();
        state.clear_pages();
        state.actions_used = 0;
        state.clear_operations();
        state.closed = true;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::browser::client::{BrowserHealth, PageSnapshot, ScreenshotData};
    use crate::browser::policy::tests::{fixed_resolver, public_resolver};
    use async_trait::async_trait;
    use tempfile::TempDir;
    use xaft_config::{CloakBrowserConfig, PlaywrightConfig};

    /// agenthicc's `FakeBrowserClient`, recording every call.
    #[derive(Default)]
    pub(crate) struct FakeClient {
        pub calls: Mutex<Vec<String>>,
        pub status: Mutex<Option<(String, String)>>,
        pub snapshot_text: Mutex<String>,
        pub screenshot_bytes: Mutex<usize>,
        pub open_failure: Mutex<Option<BrowserFailure>>,
        pub gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
        pub gone: Mutex<Vec<String>>,
    }

    impl FakeClient {
        fn check_gone(&self, page_id: &str) -> Result<(), BrowserFailure> {
            if self.gone.lock().unwrap().iter().any(|p| p == page_id) {
                return Err(BrowserFailure::tool(
                    BrowserErrorKind::NOT_FOUND,
                    "Browser page is no longer open.",
                ));
            }
            Ok(())
        }
    }

    impl FakeClient {
        fn record(&self, call: &str) {
            self.calls.lock().unwrap().push(call.to_string());
        }
        pub(crate) fn count(&self, call: &str) -> usize {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| *c == call)
                .count()
        }
    }

    #[async_trait]
    impl BrowserClient for FakeClient {
        async fn health(&self) -> BrowserHealth {
            match self.status.lock().unwrap().clone() {
                Some((status, message)) => BrowserHealth {
                    status,
                    transport: "local".into(),
                    message,
                },
                None => BrowserHealth::ready("local"),
            }
        }
        async fn open_page(&self, url: &str) -> Result<PageState, BrowserFailure> {
            self.record("open");
            let gate = self.gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            if let Some(failure) = self.open_failure.lock().unwrap().clone() {
                return Err(failure);
            }
            Ok(PageState::new("page-1", url, "Example"))
        }
        async fn snapshot(&self, page_id: &str) -> Result<PageSnapshot, BrowserFailure> {
            self.check_gone(page_id)?;
            let mut link = Map::new();
            link.insert(
                "href".into(),
                json!("https://example.com/next?token=hidden"),
            );
            link.insert("text".into(), json!("Next"));
            Ok(PageSnapshot {
                page: PageState::new(page_id, "https://example.com/path?secret=1", "Example"),
                text: self.snapshot_text.lock().unwrap().clone(),
                links: vec![link; 100],
                controls: Vec::new(),
                truncated: false,
            })
        }
        async fn click(&self, page_id: &str, _s: &str) -> Result<PageState, BrowserFailure> {
            self.record("click");
            Ok(PageState::new(
                page_id,
                "https://example.com/after",
                "After",
            ))
        }
        async fn fill(
            &self,
            page_id: &str,
            _s: &str,
            _v: &str,
        ) -> Result<PageState, BrowserFailure> {
            self.record("fill");
            if page_id != "page-1" {
                return Err(BrowserFailure::tool(
                    BrowserErrorKind::NOT_FOUND,
                    "Browser page is no longer open.",
                ));
            }
            Ok(PageState::new(page_id, "https://example.com/", "Example"))
        }
        async fn press(
            &self,
            page_id: &str,
            _k: &str,
            _s: &str,
        ) -> Result<PageState, BrowserFailure> {
            self.record("press");
            Ok(PageState::new(page_id, "https://example.com/", "Example"))
        }
        async fn wait_for(
            &self,
            page_id: &str,
            _c: &str,
            _v: &str,
        ) -> Result<PageState, BrowserFailure> {
            Ok(PageState::new(page_id, "https://example.com/", "Example"))
        }
        async fn screenshot(
            &self,
            _p: &str,
            image_type: &str,
            _f: bool,
        ) -> Result<ScreenshotData, BrowserFailure> {
            let size = (*self.screenshot_bytes.lock().unwrap()).max(10);
            Ok(ScreenshotData {
                content: vec![b'x'; size],
                mime_type: if image_type == "jpeg" {
                    "image/jpeg"
                } else {
                    "image/png"
                },
            })
        }
        async fn close_page(&self, page_id: &str) -> Result<(), BrowserFailure> {
            self.record("close");
            self.check_gone(page_id)
        }
        async fn close_session(&self) -> Result<(), BrowserFailure> {
            self.record("close_session");
            Ok(())
        }
    }

    pub(crate) fn cloak_settings(f: impl FnOnce(&mut CloakBrowserConfig)) -> BrowserSettings {
        let mut config = CloakBrowserConfig {
            allowed_domains: vec!["example.com".into()],
            allow_all_domains: false,
            ..Default::default()
        };
        f(&mut config);
        BrowserSettings::from_cloakbrowser(&config)
    }

    pub(crate) fn manager_with(
        tmp: &TempDir,
        settings: BrowserSettings,
    ) -> (BrowserSessionManager, Arc<FakeClient>) {
        let client = Arc::new(FakeClient::default());
        let policy = BrowserPolicy::cloakbrowser(&["example.com".to_string()], false)
            .unwrap()
            .with_resolver(public_resolver());
        let manager = BrowserSessionManager::with_client(
            settings,
            "conversation-1",
            tmp.path(),
            client.clone(),
            Some(policy),
        );
        (manager, client)
    }

    fn manager(tmp: &TempDir) -> (BrowserSessionManager, Arc<FakeClient>) {
        manager_with(tmp, cloak_settings(|_| {}))
    }

    #[tokio::test]
    async fn the_default_is_enabled_with_the_liberal_policy() {
        let tmp = TempDir::new().unwrap();
        let manager = BrowserSessionManager::new(
            BrowserSettings::from_cloakbrowser(&CloakBrowserConfig::default()),
            "s",
            tmp.path(),
            None,
            BrowserLaunchOptions::default(),
        );
        assert!(manager.enabled());
        assert!(manager.policy().allow_all_domains());
        assert!(manager.policy().allow_loopback());
        assert!(manager.policy().allow_private_addresses());
        let pw = BrowserSessionManager::new(
            BrowserSettings::from_playwright(&PlaywrightConfig::default()),
            "s",
            tmp.path(),
            None,
            BrowserLaunchOptions::default(),
        );
        assert!(pw.policy().allow_loopback() && pw.policy().allow_all_domains());
    }

    #[tokio::test]
    async fn outputs_are_redacted_and_sensitive_fields_refused() {
        let tmp = TempDir::new().unwrap();
        let (manager, _client) = manager(&tmp);
        let opened = manager
            .open("https://example.com/path?token=secret", None, None)
            .await;
        assert_eq!(opened["ok"], true, "{opened}");
        assert_eq!(opened["page"]["url"], "https://example.com/path");
        let snapshot = manager.snapshot("page-1", None, None).await;
        assert!(!snapshot.to_string().contains("secret"));
        assert!(!snapshot.to_string().contains("hidden"));
        assert_eq!(snapshot["snapshot"]["untrusted"], true);
        let denied = manager
            .fill("page-1", "input[name=password]", "not-recorded", None, None)
            .await;
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["error_kind"], "policy_denied");
        assert!(!denied.to_string().contains("not-recorded"));
    }

    #[tokio::test]
    async fn snapshots_are_bounded_at_the_session_boundary() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager_with(&tmp, cloak_settings(|c| c.max_snapshot_chars = 256));
        *client.snapshot_text.lock().unwrap() = "x".repeat(300);
        manager.open("https://example.com/", None, None).await;
        let result = manager.snapshot("page-1", None, None).await;
        let snapshot = &result["snapshot"];
        assert_eq!(snapshot["text"].as_str().unwrap().chars().count(), 256);
        assert_eq!(snapshot["truncated"], true);
        assert_eq!(snapshot["links"].as_array().unwrap().len(), 50);
        assert_eq!(snapshot["links"][0]["href"], "https://example.com/next");
    }

    #[tokio::test]
    async fn the_action_budget_limits_and_resets_per_turn() {
        let tmp = TempDir::new().unwrap();
        let (manager, _client) = manager_with(&tmp, cloak_settings(|c| c.max_actions_per_turn = 1));
        manager.open("https://example.com/", None, None).await;
        let first = manager.click("page-1", "button.submit", None, None).await;
        assert_eq!(first["ok"], true);
        let second = manager.press("page-1", "Enter", "body", None, None).await;
        assert_eq!(second["error_kind"], "limit_exceeded");
        assert_eq!(
            second["error"],
            "Browser action limit reached for this turn."
        );
        // Snapshots, waits and screenshots do not consume actions.
        assert_eq!(manager.snapshot("page-1", None, None).await["ok"], true);
        manager.reset_turn_budget();
        assert_eq!(manager.actions_used(), 0);
        assert_eq!(
            manager.press("page-1", "Enter", "body", None, None).await["ok"],
            true
        );
    }

    #[tokio::test]
    async fn an_operation_id_replays_instead_of_repeating() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager(&tmp);
        let first = manager
            .open("https://example.com/", Some("open-once"), None)
            .await;
        let second = manager
            .open("https://example.com/", Some(" open-once "), None)
            .await;
        assert_eq!(first, second);
        assert_eq!(first["operation_id"], "open-once");
        assert_eq!(client.count("open"), 1);
        let invalid = manager
            .open("https://example.com/", Some("bad id"), None)
            .await;
        assert_eq!(invalid["error_kind"], "invalid_input");
        assert_eq!(invalid["error"], "Operation id is invalid or too long.");
        assert!(
            invalid.get("operation_id").is_none(),
            "agenthicc returns before tagging"
        );
        let generated = manager.status().await;
        assert!(
            generated.get("operation_id").is_none(),
            "status is not an operation"
        );
        let untagged = manager.snapshot("page-1", None, None).await;
        assert_eq!(untagged["operation_id"].as_str().unwrap().len(), 32);
    }

    #[tokio::test]
    async fn an_in_flight_operation_id_is_refused() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager(&tmp);
        let gate = Arc::new(tokio::sync::Notify::new());
        *client.gate.lock().unwrap() = Some(Arc::clone(&gate));
        let manager = Arc::new(manager);
        let m = Arc::clone(&manager);
        let first =
            tokio::spawn(async move { m.open("https://example.com/", Some("op-1"), None).await });
        while client.count("open") == 0 {
            tokio::task::yield_now().await;
        }
        let second = manager
            .open("https://example.com/", Some("op-1"), None)
            .await;
        assert_eq!(
            second["error"],
            "Operation is already in progress; do not retry it yet."
        );
        assert_eq!(second["operation_id"], "op-1");
        gate.notify_one();
        assert_eq!(first.await.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn status_reports_each_unavailable_state() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager(&tmp);
        let ready = manager.status().await;
        assert_eq!(ready["ok"], true);
        assert_eq!(ready["max_pages"], 4);
        assert_eq!(ready["max_actions_per_turn"], 20);
        *client.status.lock().unwrap() = Some(("binary_missing".into(), "no binary".into()));
        let missing = manager.status().await;
        assert_eq!(missing["status"], "binary_missing");
        // An operation maps binary_missing to browser_unavailable.
        let opened = manager.open("https://example.com/", None, None).await;
        assert_eq!(opened["error_kind"], "browser_unavailable");
        assert_eq!(opened["error"], "no binary");

        manager.close_session().await;
        assert_eq!(manager.status().await["status"], "closed");

        let invalid = BrowserSessionManager::with_client(
            cloak_settings(|c| c.allowed_domains = vec!["*".into()]),
            "c",
            tmp.path(),
            Arc::new(FakeClient::default()),
            None,
        );
        assert!(!invalid.enabled());
        let status = invalid.status().await;
        assert_eq!(status["status"], "policy_denied");
        assert_eq!(
            status["message"],
            "Browser allow-list configuration is invalid."
        );
        assert_eq!(
            invalid.open("https://example.com", None, None).await["error_kind"],
            "policy_denied"
        );

        let empty = BrowserSessionManager::with_client(
            cloak_settings(|c| c.allowed_domains.clear()),
            "c",
            tmp.path(),
            Arc::new(FakeClient::default()),
            None,
        );
        let status = empty.status().await;
        assert_eq!(status["status"], "not_configured");
        assert_eq!(
            status["message"],
            "Configure at least one allowed CloakBrowser origin."
        );
        let opened = empty.open("https://example.com/", None, None).await;
        assert_eq!(
            opened["error"],
            "No CloakBrowser destinations are configured in the allow-list."
        );
    }

    #[tokio::test]
    async fn health_kinds_map_to_browser_unavailable() {
        for status in ["binary_missing", "unhealthy", "unknown-status"] {
            let tmp = TempDir::new().unwrap();
            let (manager, client) = manager(&tmp);
            *client.status.lock().unwrap() = Some((status.into(), String::new()));
            let opened = manager.open("https://example.com/", None, None).await;
            assert_eq!(opened["error_kind"], "browser_unavailable", "{status}");
            assert_eq!(opened["error"], "Browser backend is unavailable.");
        }
    }

    #[tokio::test]
    async fn pages_close_one_at_a_time_or_all_together() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager(&tmp);
        manager.open("https://example.com/", None, None).await;
        let closed = manager.close("page-1", false, None, None).await;
        assert_eq!(closed["closed_page"], "page-1");
        let missing = manager.close("page-1", false, None, None).await;
        assert_eq!(missing["error_kind"], "stale_page");
        manager.open("https://example.com/", None, None).await;
        let all = manager.close("", true, None, None).await;
        assert_eq!(all["closed_pages"], "all");
        assert!(manager.open_page_ids().is_empty());
        assert_eq!(client.count("close"), 2);
    }

    #[tokio::test]
    async fn the_page_ceiling_and_screenshot_limits_hold() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager_with(
            &tmp,
            cloak_settings(|c| {
                c.max_pages = 1;
                c.max_screenshot_bytes = 1024;
            }),
        );
        assert_eq!(
            manager.open("https://example.com/", None, None).await["ok"],
            true
        );
        let second = manager.open("https://example.com/other", None, None).await;
        assert_eq!(second["error_kind"], "limit_exceeded");
        assert_eq!(second["error"], "Maximum open browser pages reached.");
        let shot = manager.screenshot("page-1", "PNG", false, None, None).await;
        assert_eq!(shot["ok"], true, "{shot}");
        let path = shot["artifact"]["path"].as_str().unwrap();
        assert!(path.starts_with(".xaft/browser-artifacts/"), "{path}");
        assert!(tmp.path().join(path).exists());
        let bad = manager.screenshot("page-1", "gif", false, None, None).await;
        assert_eq!(bad["error"], "Screenshot format must be png or jpeg.");
        *client.screenshot_bytes.lock().unwrap() = 1025;
        let big = manager.screenshot("page-1", "png", false, None, None).await;
        assert_eq!(big["error_kind"], "output_limit");
    }

    #[tokio::test]
    async fn failures_are_structured_without_leaking_detail() {
        let tmp = TempDir::new().unwrap();
        for (failure, kind, message) in [
            (
                BrowserFailure::Timeout,
                "timeout",
                "Browser operation timed out.",
            ),
            (
                BrowserFailure::Network,
                "network",
                "Browser network operation failed.",
            ),
            (
                BrowserFailure::Execution("secret page detail"),
                "execution",
                "Browser operation failed.",
            ),
        ] {
            let (manager, client) = manager(&tmp);
            *client.open_failure.lock().unwrap() = Some(failure);
            let result = manager.open("https://example.com/", None, None).await;
            assert_eq!(result["ok"], false);
            assert_eq!(result["error_kind"], kind);
            assert_eq!(result["error"], message);
            assert!(!result.to_string().contains("secret"));
            assert_eq!(result.as_object().unwrap().len(), 4, "{result}");
        }
    }

    #[tokio::test]
    async fn closing_the_session_is_terminal_until_open_reopens_it() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager(&tmp);
        manager
            .open("https://example.com/", Some("before-close"), None)
            .await;
        assert_eq!(
            manager.click("page-1", "button", None, None).await["ok"],
            true
        );
        manager.close_session().await;
        manager.close_session().await;
        assert_eq!(client.count("close_session"), 1);
        assert_eq!(manager.actions_used(), 0);
        assert_eq!(manager.status().await["status"], "closed");
        assert_eq!(
            manager.click("page-1", "button", None, None).await["error"],
            "Browser session is closed."
        );
        // The same operation id runs again: reopening clears the cache.
        let reopened = manager
            .open("https://example.com/", Some("before-close"), None)
            .await;
        assert_eq!(reopened["ok"], true);
        assert_eq!(manager.status().await["ok"], true);
        assert_eq!(client.count("open"), 2);
    }

    #[tokio::test]
    async fn runtime_cleanup_keeps_the_session_usable() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager(&tmp);
        manager.open("https://example.com/", None, None).await;
        manager.close_runtime().await;
        assert_eq!(manager.status().await["status"], "ready");
        assert!(manager.open_page_ids().is_empty());
        assert_eq!(
            manager.open("https://example.com/", None, None).await["ok"],
            true
        );
        assert_eq!(client.count("close_session"), 1);
    }

    /// A page the browser lost stops holding a `max_pages` slot.
    #[tokio::test]
    async fn a_page_the_browser_lost_is_forgotten() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager_with(&tmp, cloak_settings(|c| c.max_pages = 1));
        manager.open("https://example.com/", None, None).await;
        client.gone.lock().unwrap().push("page-1".into());
        let stale = manager.snapshot("page-1", None, None).await;
        assert_eq!(stale["error_kind"], "stale_page");
        assert!(manager.open_page_ids().is_empty());
        assert_eq!(
            manager.open("https://example.com/", None, None).await["ok"],
            true
        );
        // Closing everything succeeds even when a page is already gone.
        assert_eq!(
            manager.close("", true, None, None).await["closed_pages"],
            "all"
        );
        assert!(manager.open_page_ids().is_empty());
    }

    /// agenthicc's quirk, kept: `fill` consumes an action before the client
    /// discovers the page is not open.
    #[tokio::test]
    async fn fill_consumes_an_action_before_the_page_check() {
        let tmp = TempDir::new().unwrap();
        let (manager, _client) = manager(&tmp);
        let result = manager.fill("no-such-page", "#q", "x", None, None).await;
        assert_eq!(result["error_kind"], "stale_page");
        assert_eq!(manager.actions_used(), 1);
    }

    #[tokio::test]
    async fn cancellation_closes_the_session_and_caches_the_outcome() {
        let tmp = TempDir::new().unwrap();
        let (manager, client) = manager(&tmp);
        manager.open("https://example.com/", None, None).await;
        let gate = Arc::new(tokio::sync::Notify::new());
        *client.gate.lock().unwrap() = Some(gate);
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let result = manager
            .open("https://example.com/again", Some("op-c"), Some(&token))
            .await;
        assert_eq!(result["error_kind"], "cancelled");
        assert_eq!(
            result["error"],
            "Browser operation was cancelled; the affected session was closed."
        );
        assert_eq!(client.count("close_session"), 1);
        assert!(manager.open_page_ids().is_empty());
        let replay = manager
            .open("https://example.com/", Some("op-c"), None)
            .await;
        assert_eq!(
            replay["error_kind"], "cancelled",
            "a retried id replays the outcome"
        );
    }

    #[tokio::test]
    async fn the_operation_cache_is_bounded() {
        let tmp = TempDir::new().unwrap();
        let (manager, _client) = manager(&tmp);
        manager.open("https://example.com/", None, None).await;
        for i in 0..(OPERATION_CACHE_LIMIT + 10) {
            manager
                .snapshot("page-1", Some(&format!("op-{i}")), None)
                .await;
        }
        let state = manager.lock();
        assert_eq!(state.results.len(), OPERATION_CACHE_LIMIT);
        assert!(!state.results.contains_key("op-0"));
    }

    #[tokio::test]
    async fn from_tool_settings_selects_one_family_or_none() {
        let tmp = TempDir::new().unwrap();
        let mut tools = ToolSettingsConfig::default();
        let cloak = BrowserSessionManager::from_tool_settings(
            &tools,
            tmp.path(),
            Some("sess-1"),
            BrowserLaunchOptions::default(),
        )
        .unwrap();
        assert_eq!(cloak.family(), BrowserFamily::CloakBrowser);
        assert_eq!(cloak.session_id(), "sess-1");
        tools.browser_backend = " Playwright ".into();
        let pw = BrowserSessionManager::from_tool_settings(
            &tools,
            tmp.path(),
            Some("../evil"),
            BrowserLaunchOptions::default(),
        )
        .unwrap();
        assert_eq!(pw.family(), BrowserFamily::Playwright);
        assert_ne!(pw.session_id(), "../evil", "an unsafe hint is replaced");
        tools.browser_backend = "none".into();
        assert!(
            BrowserSessionManager::from_tool_settings(
                &tools,
                tmp.path(),
                None,
                BrowserLaunchOptions::default()
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn a_disabled_backend_reports_agenthiccs_sentences() {
        let tmp = TempDir::new().unwrap();
        let settings = BrowserSettings::from_playwright(&PlaywrightConfig {
            enabled: false,
            ..Default::default()
        });
        let manager = BrowserSessionManager::new(
            settings,
            "c",
            tmp.path(),
            None,
            BrowserLaunchOptions::default(),
        );
        let status = manager.status().await;
        assert_eq!(status["status"], "not_configured");
        assert_eq!(
            status["message"],
            "Playwright is disabled; enable the selected browser backend."
        );
        let opened = manager.open("https://example.com/", None, None).await;
        assert_eq!(opened["error"], "Playwright is disabled.");
    }

    #[tokio::test]
    async fn rebinding_resolution_is_checked_before_opening() {
        let tmp = TempDir::new().unwrap();
        let client = Arc::new(FakeClient::default());
        let policy = BrowserPolicy::cloakbrowser(&["example.com".to_string()], false)
            .unwrap()
            .with_resolver(fixed_resolver(&[("example.com", "10.0.0.7")]));
        let manager = BrowserSessionManager::with_client(
            cloak_settings(|_| {}),
            "c",
            tmp.path(),
            client.clone(),
            Some(policy),
        );
        let opened = manager.open("https://example.com/", None, None).await;
        assert_eq!(opened["error"], "Private destinations are blocked.");
        assert_eq!(client.count("open"), 0, "the browser never saw the request");
    }
}
