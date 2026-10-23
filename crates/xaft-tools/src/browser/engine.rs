//! The Chromium engine behind both browser tool families.
//!
//! [`CdpBrowserClient`] is the real [`BrowserClient`]: it launches (or, for
//! `cloakbrowser` with `transport = "cdp"`, attaches to) a Chromium-family
//! browser and drives it over the Chrome DevTools Protocol. It replaces
//! agenthicc's two Playwright-based transports — `LocalCloakBrowserClient` /
//! `PlaywrightBrowserClient` and `CdpCloakBrowserClient` — with one engine,
//! because both families run the same contract on the same kind of browser.
//!
//! # What it guarantees
//!
//! * **Isolation.** A launched browser gets a throw-away profile (or, with
//!   `allow_persistent_profiles`, `<workspace>/<profile_root>/<session>`). An
//!   attached browser is never driven in its default context: the session
//!   gets a fresh browser context that Chromium disposes of when xaft
//!   disconnects, exactly as agenthicc's CDP transport does.
//! * **Every new target is paused until xaft has decided about it.** The
//!   browser is told to auto-attach with `waitForDebuggerOnStart`, so a popup
//!   is closed before it can load anything (agenthicc closes popups after the
//!   fact), and a frame or worker has request interception installed before
//!   its first request.
//! * **The destination policy covers sub-resources** when an allow-list is in
//!   force (`allow_all_domains = false`): every request the page, its frames
//!   or its workers make is paused and checked by [`BrowserPolicy`], and
//!   refused requests fail with `BlockedByClient` — agenthicc's
//!   `page.route("**/*", guard)`. Service-worker registration is disabled in
//!   that mode, as Playwright's `serviceWorkers: "block"` does.
//! * **Dialogs never wedge the page**: `alert`/`confirm`/`prompt` are
//!   dismissed and `beforeunload` accepted, as Playwright does when nothing
//!   listens. Downloads are denied — no tool can reach a downloaded file.
//! * **Page scripts cannot interfere with the tools**: element lookup,
//!   snapshotting and actionability checks run in a CDP isolated world, and
//!   clicks and key presses are real input events, not synthetic DOM events.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{Map, Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::artifacts::is_safe_id;
use super::cdp::{CdpConnection, CdpError, CdpEvent, websocket_config, websocket_pumps};
use super::client::{
    BrowserClient, BrowserHealth, PageSnapshot, PageState, ScreenshotData, truncate_chars,
};
use super::errors::{BrowserErrorKind, BrowserFailure, BrowserToolError};
use super::launch::{
    BrowserDiscovery, LaunchSpec, http_get_json, is_loopback_ws_url, kill_process_tree, launch,
};
use super::policy::{BrowserPolicy, LOAD_STATES, redact_link, sensitive_field_refusal};
use super::settings::{BrowserFamily, BrowserSettings, BrowserTransport};

/// The injected page script (see `page_script.js`).
const PAGE_SCRIPT: &str = include_str!("page_script.js");

/// Installed in every document when an allow-list is in force: a service
/// worker or shared worker would make requests outside the page's
/// interception (Playwright's `serviceWorkers: "block"` does the same).
const WORKER_BLOCK_SCRIPT: &str = "(() => { try { \
    if (navigator.serviceWorker) { navigator.serviceWorker.register = () => \
        Promise.reject(new DOMException('Service workers are disabled by the browser policy.', 'SecurityError')); } \
    if (window.SharedWorker) { window.SharedWorker = undefined; } \
} catch (e) {} })();";

/// How long to wait for an action to *start* a navigation before reading the
/// page state back.
const SETTLE_WINDOW: Duration = Duration::from_millis(250);

/// How often an actionability probe is retried.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// Hooks for embedders and tests. Nothing here is user configuration.
#[derive(Debug, Clone, Default)]
pub struct BrowserLaunchOptions {
    /// Where to look for browsers; `None` reads the process environment.
    pub discovery: Option<BrowserDiscovery>,
    /// Extra Chromium switches appended to every launch.
    pub extra_args: Vec<String>,
    /// Parent directory for throw-away profiles; `None` is the system temp dir.
    pub temp_root: Option<PathBuf>,
    /// Use the loopback-port transport even where pipes are available.
    pub force_port_transport: bool,
    /// Deadline for a launched browser to answer its first command.
    pub launch_timeout: Option<Duration>,
}

fn stale_page() -> BrowserFailure {
    BrowserFailure::tool(
        BrowserErrorKind::StalePage,
        "Browser page is no longer open.",
    )
}

fn map_cdp(error: CdpError) -> BrowserFailure {
    match error {
        CdpError::Closed => BrowserFailure::Execution("browser connection closed"),
        CdpError::Timeout => BrowserFailure::Timeout,
        CdpError::Protocol { .. } => BrowserFailure::Execution("cdp protocol error"),
    }
}

/// A protocol error that a fresh isolated world (after a navigation) fixes.
fn context_lost(error: &CdpError) -> bool {
    match error {
        CdpError::Protocol { message, .. } => {
            let m = message.to_ascii_lowercase();
            m.contains("context") || m.contains("navigat") || m.contains("cannot find")
        }
        _ => false,
    }
}

/// Map a `Page.navigate` `errorText` to a failure bucket.
fn navigation_error(text: &str) -> BrowserFailure {
    if text.contains("ERR_BLOCKED_BY_CLIENT") {
        BrowserFailure::tool(
            BrowserErrorKind::PolicyDenied,
            "Destination is not allow-listed.",
        )
    } else if text.contains("ERR_ABORTED") {
        BrowserFailure::Execution("navigation aborted")
    } else if text.starts_with("net::") {
        BrowserFailure::Network
    } else {
        BrowserFailure::Execution("navigation failed")
    }
}

/// Per-page navigation state, maintained by the event loop.
#[derive(Debug, Default)]
struct PageRecord {
    target_id: String,
    session_id: String,
    main_frame_id: Option<String>,
    url: String,
    loader_id: Option<String>,
    lifecycle: HashSet<String>,
    loader_seq: u64,
    loader_history: VecDeque<(String, u64)>,
    nav_requested: u64,
    nav_committed: u64,
    same_document: u64,
    stopped_loading: u64,
    detached: bool,
}

impl PageRecord {
    fn is_main_frame(&self, frame_id: Option<&str>) -> bool {
        match (&self.main_frame_id, frame_id) {
            (Some(main), Some(frame)) => main == frame,
            (None, _) => true,
            (Some(_), None) => false,
        }
    }

    /// Whether the document `loader` (or one committed after it) reached
    /// `DOMContentLoaded`.
    fn dom_content_loaded_since(&self, loader: &str) -> bool {
        if !self.lifecycle.contains("DOMContentLoaded") {
            return false;
        }
        if self.loader_id.as_deref() == Some(loader) {
            return true;
        }
        self.loader_history
            .iter()
            .any(|(id, seq)| id == loader && *seq < self.loader_seq)
    }

    fn counters(&self) -> NavCounters {
        NavCounters {
            requested: self.nav_requested,
            committed: self.nav_committed,
            same_document: self.same_document,
            stopped: self.stopped_loading,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct NavCounters {
    requested: u64,
    committed: u64,
    same_document: u64,
    stopped: u64,
}

#[derive(Debug, Default)]
struct RuntimeState {
    /// Page targets that attached but have not been claimed by `open_page`.
    unclaimed: HashMap<String, String>,
    pages: HashMap<String, PageRecord>,
    session_index: HashMap<String, String>,
    /// Popup sessions being torn down: every request they make is refused.
    doomed_sessions: HashSet<String>,
    closed: bool,
}

impl RuntimeState {
    fn page_for_session(&mut self, session: Option<&str>) -> Option<&mut PageRecord> {
        let page_id = self.session_index.get(session?)?.clone();
        self.pages.get_mut(&page_id)
    }
}

#[derive(Debug, Default)]
struct RuntimeShared {
    state: Mutex<RuntimeState>,
    notify: Notify,
}

impl RuntimeShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, RuntimeState> {
        self.state.lock().expect("browser runtime state poisoned")
    }

    /// Wait until `check` yields a value, the runtime closes, or `timeout`.
    async fn wait_for<T>(
        &self,
        timeout: Duration,
        mut check: impl FnMut(&RuntimeState) -> Option<T>,
    ) -> Result<T, BrowserFailure> {
        let deadline = Instant::now() + timeout;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.lock();
                if let Some(value) = check(&state) {
                    return Ok(value);
                }
                if state.closed {
                    return Err(BrowserFailure::Execution("browser connection closed"));
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                let state = self.lock();
                return check(&state).ok_or(BrowserFailure::Timeout);
            }
        }
    }
}

/// What the event loop needs to decide about targets and requests.
#[derive(Clone)]
struct LoopConfig {
    /// The isolated context xaft owns on an attached browser.
    context_id: Option<String>,
    /// Every target belongs to xaft (a browser xaft launched).
    own_all: bool,
    /// Check every sub-resource request against the policy.
    intercept: bool,
    policy: Arc<BrowserPolicy>,
}

const INTERCEPT_PATTERNS: &str = r#"{"patterns":[{"urlPattern":"*","requestStage":"Request"}]}"#;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

fn spawn_call(
    conn: &Arc<CdpConnection>,
    session: Option<String>,
    method: &'static str,
    params: Value,
) {
    let conn = Arc::clone(conn);
    tokio::spawn(async move {
        let _ = conn
            .call(session.as_deref(), method, params, COMMAND_TIMEOUT)
            .await;
    });
}

fn handle_attach(
    conn: &Arc<CdpConnection>,
    shared: &RuntimeShared,
    cfg: &LoopConfig,
    event: &CdpEvent,
) {
    let params = &event.params;
    let Some(session) = params["sessionId"].as_str().map(str::to_string) else {
        return;
    };
    let info = &params["targetInfo"];
    let target_type = info["type"].as_str().unwrap_or("");
    let target_id = info["targetId"].as_str().unwrap_or("").to_string();
    let waiting = params["waitingForDebugger"].as_bool().unwrap_or(false);
    let context = info["browserContextId"].as_str();
    let ours = cfg.own_all || (cfg.context_id.is_some() && context == cfg.context_id.as_deref());

    if !ours {
        // Someone else's target on an attached browser: let it run and leave.
        let conn = Arc::clone(conn);
        tokio::spawn(async move {
            if waiting {
                let _ = conn
                    .call(
                        Some(&session),
                        "Runtime.runIfWaitingForDebugger",
                        json!({}),
                        COMMAND_TIMEOUT,
                    )
                    .await;
            }
            let _ = conn
                .call(
                    None,
                    "Target.detachFromTarget",
                    json!({ "sessionId": session }),
                    COMMAND_TIMEOUT,
                )
                .await;
        });
        return;
    }

    match target_type {
        "page" => {
            let opener = info["openerId"].as_str().is_some_and(|o| !o.is_empty());
            if opener || event.session_id.is_some() {
                // A popup. It cannot simply be closed while paused: a same-site
                // popup shares its opener's renderer, a paused target never
                // finishes closing, and the opener's click would then never be
                // acknowledged. So it is made inert first — every request it
                // makes is refused (Chromium holds a paused target's first
                // navigation until it resumes, so that includes the page it
                // was opened for) — then resumed, then closed.
                shared.lock().doomed_sessions.insert(session.clone());
                let conn = Arc::clone(conn);
                tokio::spawn(async move {
                    let s = Some(session.as_str());
                    let patterns: Value =
                        serde_json::from_str(INTERCEPT_PATTERNS).expect("static json");
                    let _ = conn
                        .call(s, "Fetch.enable", patterns, COMMAND_TIMEOUT)
                        .await;
                    if waiting {
                        let _ = conn
                            .call(
                                s,
                                "Runtime.runIfWaitingForDebugger",
                                json!({}),
                                COMMAND_TIMEOUT,
                            )
                            .await;
                    }
                    let _ = conn
                        .call(
                            None,
                            "Target.closeTarget",
                            json!({ "targetId": target_id }),
                            COMMAND_TIMEOUT,
                        )
                        .await;
                });
            } else {
                shared.lock().unclaimed.insert(target_id, session);
            }
        }
        "iframe" | "worker" | "shared_worker" | "service_worker" => {
            let conn = Arc::clone(conn);
            let intercept = cfg.intercept;
            let is_frame = target_type == "iframe";
            tokio::spawn(async move {
                let s = Some(session.as_str());
                if intercept {
                    let patterns: Value =
                        serde_json::from_str(INTERCEPT_PATTERNS).expect("static json");
                    let _ = conn
                        .call(s, "Fetch.enable", patterns, COMMAND_TIMEOUT)
                        .await;
                }
                if is_frame {
                    let _ = conn
                        .call(
                            s,
                            "Target.setAutoAttach",
                            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
                            COMMAND_TIMEOUT,
                        )
                        .await;
                    if intercept {
                        let _ = conn
                            .call(
                                s,
                                "Page.addScriptToEvaluateOnNewDocument",
                                json!({ "source": WORKER_BLOCK_SCRIPT }),
                                COMMAND_TIMEOUT,
                            )
                            .await;
                    }
                }
                if waiting {
                    let _ = conn
                        .call(
                            s,
                            "Runtime.runIfWaitingForDebugger",
                            json!({}),
                            COMMAND_TIMEOUT,
                        )
                        .await;
                }
            });
        }
        _ => {
            if waiting {
                spawn_call(
                    conn,
                    Some(session),
                    "Runtime.runIfWaitingForDebugger",
                    json!({}),
                );
            }
        }
    }
}

fn handle_event(
    conn: &Arc<CdpConnection>,
    shared: &RuntimeShared,
    cfg: &LoopConfig,
    event: CdpEvent,
) {
    let params = &event.params;
    let session = event.session_id.as_deref();
    match event.method.as_str() {
        "Target.attachedToTarget" => handle_attach(conn, shared, cfg, &event),
        "Target.detachedFromTarget" => {
            let mut state = shared.lock();
            if let Some(target) = params["targetId"].as_str() {
                state.unclaimed.remove(target);
            }
            let detached = params["sessionId"].as_str();
            if let Some(session) = detached {
                state.doomed_sessions.remove(session);
            }
            if let Some(page) = state.page_for_session(detached) {
                page.detached = true;
            }
        }
        "Inspector.detached" | "Inspector.targetCrashed" => {
            if let Some(page) = shared.lock().page_for_session(session) {
                page.detached = true;
            }
        }
        "Page.frameNavigated" => {
            let frame = &params["frame"];
            if frame.get("parentId").and_then(Value::as_str).is_some() {
                return;
            }
            if let Some(page) = shared.lock().page_for_session(session) {
                page.main_frame_id = frame["id"].as_str().map(str::to_string);
                page.url = format!(
                    "{}{}",
                    frame["url"].as_str().unwrap_or(""),
                    frame["urlFragment"].as_str().unwrap_or("")
                );
                page.nav_committed += 1;
            }
        }
        "Page.navigatedWithinDocument" => {
            if let Some(page) = shared.lock().page_for_session(session) {
                if page.is_main_frame(params["frameId"].as_str()) {
                    page.url = params["url"].as_str().unwrap_or("").to_string();
                    page.same_document += 1;
                }
            }
        }
        "Page.lifecycleEvent" => {
            if let Some(page) = shared.lock().page_for_session(session) {
                if !page.is_main_frame(params["frameId"].as_str()) {
                    return;
                }
                let name = params["name"].as_str().unwrap_or("");
                let loader = params["loaderId"].as_str().unwrap_or("").to_string();
                if name == "init" {
                    page.loader_seq += 1;
                    page.loader_id = Some(loader.clone());
                    page.lifecycle.clear();
                    page.loader_history.push_back((loader, page.loader_seq));
                    if page.loader_history.len() > 16 {
                        page.loader_history.pop_front();
                    }
                } else if page.loader_id.as_deref() == Some(loader.as_str()) {
                    page.lifecycle.insert(name.to_string());
                }
            }
        }
        "Page.frameRequestedNavigation"
        | "Page.frameStartedNavigating"
        | "Page.frameStartedLoading" => {
            if let Some(page) = shared.lock().page_for_session(session) {
                if page.is_main_frame(params["frameId"].as_str()) {
                    page.nav_requested += 1;
                }
            }
        }
        "Page.frameStoppedLoading" | "Page.frameClearedScheduledNavigation" => {
            if let Some(page) = shared.lock().page_for_session(session) {
                if page.is_main_frame(params["frameId"].as_str()) {
                    page.stopped_loading += 1;
                }
            }
        }
        "Page.javascriptDialogOpening" => {
            let accept = params["type"].as_str() == Some("beforeunload");
            spawn_call(
                conn,
                event.session_id.clone(),
                "Page.handleJavaScriptDialog",
                json!({ "accept": accept }),
            );
        }
        "Fetch.requestPaused" => {
            let conn = Arc::clone(conn);
            let policy = Arc::clone(&cfg.policy);
            let request_id = params["requestId"].clone();
            let url = params["request"]["url"].as_str().unwrap_or("").to_string();
            let session = event.session_id.clone();
            let doomed = session
                .as_deref()
                .is_some_and(|s| shared.lock().doomed_sessions.contains(s));
            tokio::spawn(async move {
                let verdict = if doomed {
                    Err(())
                } else {
                    policy.validate_url(&url).await.map_err(|_| ())
                };
                let (method, payload) = match verdict {
                    Ok(_) => ("Fetch.continueRequest", json!({ "requestId": request_id })),
                    Err(_) => (
                        "Fetch.failRequest",
                        json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                    ),
                };
                let _ = conn
                    .call(session.as_deref(), method, payload, COMMAND_TIMEOUT)
                    .await;
            });
        }
        _ => {}
    }
}

async fn event_loop(
    conn: Arc<CdpConnection>,
    shared: Arc<RuntimeShared>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<CdpEvent>,
    cfg: LoopConfig,
) {
    while let Some(event) = events.recv().await {
        handle_event(&conn, &shared, &cfg, event);
        shared.notify.notify_waiters();
    }
    shared.lock().closed = true;
    shared.notify.notify_waiters();
}

/// A browser process xaft launched, with its throw-away profile.
struct BrowserProcess {
    child: tokio::process::Child,
    temp_dir: Option<tempfile::TempDir>,
    stderr_task: JoinHandle<()>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

/// One live browser connection.
struct BrowserRuntime {
    conn: Arc<CdpConnection>,
    shared: Arc<RuntimeShared>,
    context_id: Option<String>,
    process: Mutex<Option<BrowserProcess>>,
    event_task: JoinHandle<()>,
}

impl BrowserRuntime {
    fn is_alive(&self) -> bool {
        !self.conn.is_closed() && !self.shared.lock().closed
    }

    /// Orderly shutdown: close pages, dispose of the context, close (and reap)
    /// a launched browser, delete a throw-away profile.
    async fn shutdown(&self) {
        let targets: Vec<String> = {
            let state = self.shared.lock();
            state.pages.values().map(|p| p.target_id.clone()).collect()
        };
        for target in targets {
            let _ = self
                .conn
                .call(
                    None,
                    "Target.closeTarget",
                    json!({ "targetId": target }),
                    Duration::from_secs(5),
                )
                .await;
        }
        if let Some(context) = &self.context_id {
            let _ = self
                .conn
                .call(
                    None,
                    "Target.disposeBrowserContext",
                    json!({ "browserContextId": context }),
                    Duration::from_secs(5),
                )
                .await;
        }
        let process = self
            .process
            .lock()
            .expect("browser process poisoned")
            .take();
        if let Some(mut process) = process {
            let _ = self
                .conn
                .call(None, "Browser.close", json!({}), Duration::from_secs(5))
                .await;
            if tokio::time::timeout(Duration::from_secs(5), process.child.wait())
                .await
                .is_err()
            {
                kill_process_tree(&mut process.child);
                let _ = tokio::time::timeout(Duration::from_secs(5), process.child.wait()).await;
            }
            process.stderr_task.abort();
            drop(process.temp_dir);
        }
        self.event_task.abort();
        let mut state = self.shared.lock();
        state.closed = true;
        drop(state);
        self.shared.notify.notify_waiters();
    }
}

impl Drop for BrowserRuntime {
    fn drop(&mut self) {
        self.event_task.abort();
        let process = self.process.get_mut().map(Option::take).unwrap_or(None);
        if let Some(mut process) = process {
            kill_process_tree(&mut process.child);
            process.stderr_task.abort();
            // Reap the browser before deleting its profile, off the dropping
            // thread; without a runtime, the profile is removed best-effort now.
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_secs(5), process.child.wait())
                            .await;
                        drop(process.temp_dir);
                    });
                }
                Err(_) => drop(process.temp_dir),
            }
        }
    }
}

/// A page's addressing, copied out of the shared state.
#[derive(Debug, Clone)]
struct PageHandle {
    target_id: String,
    session_id: String,
}

/// The real browser client: Chromium over CDP.
pub struct CdpBrowserClient {
    engine: Arc<Engine>,
}

impl std::fmt::Debug for CdpBrowserClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdpBrowserClient")
            .field("family", &self.engine.settings.family)
            .field("transport", &self.engine.settings.transport)
            .finish_non_exhaustive()
    }
}

struct Engine {
    settings: BrowserSettings,
    policy: Arc<BrowserPolicy>,
    workspace_root: PathBuf,
    session_id: String,
    options: BrowserLaunchOptions,
    runtime: tokio::sync::Mutex<Option<Arc<BrowserRuntime>>>,
}

impl CdpBrowserClient {
    /// A client for `settings`, enforcing `policy`, with persistent profiles
    /// and artifacts keyed by `session_id`.
    pub fn new(
        settings: BrowserSettings,
        policy: Arc<BrowserPolicy>,
        workspace_root: impl Into<PathBuf>,
        session_id: impl Into<String>,
        options: BrowserLaunchOptions,
    ) -> Self {
        Self {
            engine: Arc::new(Engine {
                settings,
                policy,
                workspace_root: workspace_root.into(),
                session_id: session_id.into(),
                options,
                runtime: tokio::sync::Mutex::new(None),
            }),
        }
    }
}

impl Engine {
    fn discovery(&self) -> BrowserDiscovery {
        self.options
            .discovery
            .clone()
            .unwrap_or_else(BrowserDiscovery::from_env)
    }

    /// The CloakBrowser licence key, read from the configured variable. Never
    /// logged and never returned; it is only handed to the browser process.
    fn license_key(&self) -> Option<String> {
        let name = self.settings.license_key_env.as_deref()?;
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }

    fn launch_timeout(&self) -> Duration {
        self.options
            .launch_timeout
            .unwrap_or(Duration::from_secs(60))
    }

    fn viewport(&self) -> Option<(u32, u32)> {
        match (self.settings.family, self.settings.transport) {
            (BrowserFamily::Playwright, _)
            | (BrowserFamily::CloakBrowser, BrowserTransport::Cdp) => Some((1280, 720)),
            // The cloakbrowser package's coherent headless viewport; headed
            // windows keep the real window size.
            (BrowserFamily::CloakBrowser, BrowserTransport::Local) => {
                self.settings.headless.then_some((1920, 947))
            }
        }
    }

    fn launch_unavailable(&self) -> BrowserFailure {
        match self.settings.family {
            BrowserFamily::Playwright => BrowserFailure::tool(
                BrowserErrorKind::BrowserUnavailable,
                "Playwright could not start its browser runtime.",
            ),
            BrowserFamily::CloakBrowser => BrowserFailure::tool(
                BrowserErrorKind::BrowserUnavailable,
                "CloakBrowser could not initialize its local browser runtime.",
            ),
        }
    }

    fn cdp_unavailable() -> BrowserFailure {
        BrowserFailure::tool(
            BrowserErrorKind::BrowserUnavailable,
            "CDP browser endpoint is unavailable.",
        )
    }

    /// The live runtime, launching or attaching one if needed.
    async fn runtime(&self) -> Result<Arc<BrowserRuntime>, BrowserFailure> {
        let mut guard = self.runtime.lock().await;
        if let Some(runtime) = guard.as_ref() {
            if runtime.is_alive() {
                return Ok(Arc::clone(runtime));
            }
        }
        if let Some(dead) = guard.take() {
            dead.shutdown().await;
        }
        let runtime = Arc::new(match self.settings.transport {
            BrowserTransport::Local => self.start_local().await?,
            BrowserTransport::Cdp => self.start_cdp().await?,
        });
        *guard = Some(Arc::clone(&runtime));
        Ok(runtime)
    }

    /// The live runtime, without starting one: page operations on a browser
    /// that is not running address pages that are no longer open.
    async fn existing_runtime(&self) -> Result<Arc<BrowserRuntime>, BrowserFailure> {
        let guard = self.runtime.lock().await;
        match guard.as_ref() {
            Some(runtime) if runtime.is_alive() => Ok(Arc::clone(runtime)),
            _ => Err(stale_page()),
        }
    }

    fn executable(&self) -> Result<PathBuf, BrowserToolError> {
        let discovery = self.discovery();
        match self.settings.family {
            BrowserFamily::Playwright => discovery.find_playwright_executable(&self.settings),
            BrowserFamily::CloakBrowser => {
                discovery.find_cloakbrowser_executable(self.license_key().is_some())
            }
        }
    }

    async fn start_local(&self) -> Result<BrowserRuntime, BrowserFailure> {
        let executable = self.executable()?;
        let (profile_dir, temp_dir) = if self.settings.allow_persistent_profiles {
            let relative = std::path::Path::new(&self.settings.profile_root);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                || !is_safe_id(&self.session_id)
            {
                return Err(BrowserFailure::tool(
                    BrowserErrorKind::PolicyDenied,
                    "Persistent browser profiles must stay inside the workspace.",
                ));
            }
            let dir = self.workspace_root.join(relative).join(&self.session_id);
            std::fs::create_dir_all(&dir).map_err(|_| self.launch_unavailable())?;
            (dir, None)
        } else {
            let mut builder = tempfile::Builder::new();
            builder.prefix("xaft-browser-");
            let temp = match &self.options.temp_root {
                Some(root) => builder.tempdir_in(root),
                None => builder.tempdir(),
            }
            .map_err(|_| self.launch_unavailable())?;
            (temp.path().to_path_buf(), Some(temp))
        };

        let mut spec = LaunchSpec::new(
            executable,
            &self.settings,
            &profile_dir,
            &self.options.extra_args,
        );
        if self.options.force_port_transport {
            spec.use_pipe = false;
        }
        spec.timeout = self.launch_timeout();
        if let Some(key) = self.license_key() {
            // The CloakBrowser Pro binary reads its key from this variable,
            // whatever variable the operator keeps it in.
            spec.env.push(("CLOAKBROWSER_LICENSE_KEY".into(), key));
        }

        let launched = launch(&spec).await.map_err(|failure| {
            tracing::debug!(family = ?self.settings.family, %failure, "browser launch failed");
            self.launch_unavailable()
        })?;
        let process = BrowserProcess {
            child: launched.child,
            temp_dir,
            stderr_task: launched.stderr_task,
            stderr_tail: launched.stderr_tail,
        };
        let (conn, events) = CdpConnection::start(launched.pumps);
        let mut process = Some(process);
        if conn
            .call(None, "Browser.getVersion", json!({}), self.launch_timeout())
            .await
            .is_err()
        {
            if let Some(mut p) = process.take() {
                let tail: Vec<String> = p
                    .stderr_tail
                    .lock()
                    .map(|t| t.iter().cloned().collect())
                    .unwrap_or_default();
                tracing::debug!(
                    lines = tail.len(),
                    "browser did not answer its first command"
                );
                kill_process_tree(&mut p.child);
            }
            return Err(self.launch_unavailable());
        }
        self.finish_start(conn, events, None, true, process)
            .await
            .map_err(|_| self.launch_unavailable())
    }

    async fn start_cdp(&self) -> Result<BrowserRuntime, BrowserFailure> {
        let endpoint =
            url::Url::parse(&self.settings.cdp_endpoint).map_err(|_| Self::cdp_unavailable())?;
        if endpoint.scheme() != "http" {
            // Chromium's DevTools server speaks plain HTTP only; an https
            // endpoint would be a TLS proxy, which xaft does not support.
            return Err(BrowserFailure::tool(
                BrowserErrorKind::BrowserUnavailable,
                "CDP browser endpoint is unavailable. xaft supports http:// DevTools endpoints \
                 (what Chromium serves); https:// endpoints are not supported.",
            ));
        }
        let host = endpoint
            .host_str()
            .ok_or_else(Self::cdp_unavailable)?
            .to_string();
        let port = endpoint.port_or_known_default().unwrap_or(80);
        let version =
            http_get_json(&host, port, "/json/version", self.settings.action_timeout).await?;
        let ws_url = version["webSocketDebuggerUrl"]
            .as_str()
            .ok_or_else(Self::cdp_unavailable)?
            .to_string();
        // The endpoint is loopback by validation; the URL it hands back must
        // be too, or an endpoint could redirect the session elsewhere.
        if !is_loopback_ws_url(&ws_url) {
            return Err(Self::cdp_unavailable());
        }
        let (socket, _) = tokio::time::timeout(
            self.settings.action_timeout,
            tokio_tungstenite::connect_async_with_config(
                ws_url.as_str(),
                Some(websocket_config()),
                false,
            ),
        )
        .await
        .map_err(|_| Self::cdp_unavailable())?
        .map_err(|_| Self::cdp_unavailable())?;
        let (conn, events) = CdpConnection::start(websocket_pumps(socket));
        let context = conn
            .call(
                None,
                "Target.createBrowserContext",
                json!({ "disposeOnDetach": true }),
                self.settings.action_timeout,
            )
            .await
            .map_err(|_| Self::cdp_unavailable())?;
        let context_id = context["browserContextId"]
            .as_str()
            .ok_or_else(Self::cdp_unavailable)?
            .to_string();
        self.finish_start(conn, events, Some(context_id), false, None)
            .await
            .map_err(|_| Self::cdp_unavailable())
    }

    async fn finish_start(
        &self,
        conn: Arc<CdpConnection>,
        events: tokio::sync::mpsc::UnboundedReceiver<CdpEvent>,
        context_id: Option<String>,
        own_all: bool,
        process: Option<BrowserProcess>,
    ) -> Result<BrowserRuntime, BrowserFailure> {
        let shared = Arc::new(RuntimeShared::default());
        let cfg = LoopConfig {
            context_id: context_id.clone(),
            own_all,
            intercept: self.policy.needs_request_interception(),
            policy: Arc::clone(&self.policy),
        };
        // The event loop must run before auto-attach is switched on, because
        // switching it on reports every existing target at once.
        let event_task = tokio::spawn(event_loop(
            Arc::clone(&conn),
            Arc::clone(&shared),
            events,
            cfg,
        ));
        let runtime = BrowserRuntime {
            conn: Arc::clone(&conn),
            shared,
            context_id: context_id.clone(),
            process: Mutex::new(process),
            event_task,
        };
        let timeout = self.settings.action_timeout;
        conn.call(
            None,
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
            timeout,
        )
        .await
        .map_err(map_cdp)?;
        let mut downloads = json!({ "behavior": "deny" });
        if let Some(context) = &context_id {
            downloads["browserContextId"] = json!(context);
        }
        let _ = conn
            .call(None, "Browser.setDownloadBehavior", downloads, timeout)
            .await;
        Ok(runtime)
    }

    fn page_handle(
        &self,
        runtime: &BrowserRuntime,
        page_id: &str,
    ) -> Result<PageHandle, BrowserFailure> {
        let state = runtime.shared.lock();
        match state.pages.get(page_id) {
            Some(page) if !page.detached => Ok(PageHandle {
                target_id: page.target_id.clone(),
                session_id: page.session_id.clone(),
            }),
            _ => Err(stale_page()),
        }
    }

    fn main_frame(
        &self,
        runtime: &BrowserRuntime,
        page_id: &str,
    ) -> Result<String, BrowserFailure> {
        let state = runtime.shared.lock();
        state
            .pages
            .get(page_id)
            .filter(|p| !p.detached)
            .and_then(|p| p.main_frame_id.clone())
            .ok_or_else(stale_page)
    }

    async fn call(
        &self,
        runtime: &BrowserRuntime,
        session: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BrowserFailure> {
        runtime
            .conn
            .call(Some(session), method, params, timeout)
            .await
            .map_err(map_cdp)
    }

    /// Run one page-script operation in a fresh isolated world.
    async fn evaluate(
        &self,
        runtime: &BrowserRuntime,
        page_id: &str,
        op: &str,
        arg: Value,
        timeout: Duration,
    ) -> Result<Value, BrowserFailure> {
        let page = self.page_handle(runtime, page_id)?;
        let expression = format!(
            "({PAGE_SCRIPT})({}, {})",
            Value::String(op.to_string()),
            arg
        );
        let deadline = Instant::now() + timeout;
        let mut attempts = 0;
        loop {
            attempts += 1;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(BrowserFailure::Timeout);
            }
            let frame_id = self.main_frame(runtime, page_id)?;
            let world = runtime
                .conn
                .call(
                    Some(&page.session_id),
                    "Page.createIsolatedWorld",
                    json!({"frameId": frame_id, "worldName": "__xaft__", "grantUniveralAccess": false}),
                    remaining,
                )
                .await;
            let context_id = match world {
                Ok(v) => v["executionContextId"].clone(),
                Err(e) if context_lost(&e) && attempts < 5 => {
                    tokio::time::sleep(PROBE_INTERVAL).await;
                    continue;
                }
                Err(e) => return Err(map_cdp(e)),
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            let outcome = runtime
                .conn
                .call(
                    Some(&page.session_id),
                    "Runtime.evaluate",
                    json!({
                        "expression": expression,
                        "contextId": context_id,
                        "returnByValue": true,
                        "awaitPromise": true,
                    }),
                    remaining.max(Duration::from_millis(1)),
                )
                .await;
            match outcome {
                Ok(v) if v.get("exceptionDetails").is_some() => {
                    if attempts < 5 {
                        // A navigation can tear the world down mid-script.
                        tokio::time::sleep(PROBE_INTERVAL).await;
                        continue;
                    }
                    return Err(BrowserFailure::Execution("page script exception"));
                }
                Ok(v) => return Ok(v["result"]["value"].clone()),
                Err(e) if context_lost(&e) && attempts < 5 => {
                    tokio::time::sleep(PROBE_INTERVAL).await;
                }
                Err(e) => return Err(map_cdp(e)),
            }
        }
    }

    async fn page_state(
        &self,
        runtime: &BrowserRuntime,
        page_id: &str,
    ) -> Result<PageState, BrowserFailure> {
        let value = self
            .evaluate(
                runtime,
                page_id,
                "state",
                json!({}),
                self.settings.action_timeout,
            )
            .await?;
        Ok(PageState::new(
            page_id,
            value["url"].as_str().unwrap_or(""),
            value["title"].as_str().unwrap_or(""),
        ))
    }

    /// Poll an actionability probe until it is ready or terminally refused.
    async fn wait_actionable(
        &self,
        runtime: &BrowserRuntime,
        page_id: &str,
        probe: Value,
    ) -> Result<Value, BrowserFailure> {
        let deadline = Instant::now() + self.settings.action_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(BrowserFailure::Timeout);
            }
            let value = match self
                .evaluate(runtime, page_id, "probe", probe.clone(), remaining)
                .await
            {
                Ok(v) => v,
                Err(BrowserFailure::Timeout) => return Err(BrowserFailure::Timeout),
                Err(e) => return Err(e),
            };
            match value["state"].as_str().unwrap_or("") {
                "ready" | "filled" => return Ok(value),
                "invalid_selector" => {
                    return Err(BrowserFailure::tool(
                        BrowserErrorKind::InvalidInput,
                        "Selector is not a valid CSS, XPath or text= selector.",
                    ));
                }
                "not_fillable" => {
                    return Err(BrowserFailure::tool(
                        BrowserErrorKind::InvalidInput,
                        "The matched element cannot be filled; target an input, textarea or \
                         contenteditable element.",
                    ));
                }
                "sensitive" => return Err(sensitive_field_refusal().into()),
                "missing" | "hidden" | "disabled" | "readonly" | "obscured" | "unstable" => {}
                _ => return Err(BrowserFailure::Execution("unexpected probe state")),
            }
            let pause = PROBE_INTERVAL.min(deadline.saturating_duration_since(Instant::now()));
            tokio::time::sleep(pause).await;
        }
    }

    fn counters(&self, runtime: &BrowserRuntime, page_id: &str) -> NavCounters {
        runtime
            .shared
            .lock()
            .pages
            .get(page_id)
            .map(PageRecord::counters)
            .unwrap_or_default()
    }

    /// After an input action, give a navigation it started the chance to
    /// commit, so the state reported back describes the page the agent is now
    /// on. Best-effort: never fails the action.
    async fn settle(&self, runtime: &BrowserRuntime, page_id: &str, before: NavCounters) {
        let started = runtime
            .shared
            .wait_for(SETTLE_WINDOW, |state| {
                let page = state.pages.get(page_id)?;
                (page.detached
                    || page.nav_requested > before.requested
                    || page.nav_committed > before.committed
                    || page.same_document > before.same_document)
                    .then_some(())
            })
            .await
            .is_ok();
        if !started {
            return;
        }
        let _ = runtime
            .shared
            .wait_for(self.settings.navigation_timeout, |state| {
                let page = state.pages.get(page_id)?;
                (page.detached
                    || page.same_document > before.same_document
                    || page.stopped_loading > before.stopped
                    || (page.nav_committed > before.committed
                        && page.lifecycle.contains("DOMContentLoaded")))
                .then_some(())
            })
            .await;
    }

    /// The post-action state, re-validated against the policy (agenthicc
    /// validates the page URL after every action).
    ///
    /// The fragment is dropped before the check. agenthicc refuses fragments
    /// in URLs an agent *supplies*, and so does this engine, but it also runs
    /// the landed page's URL through the same check — so a click on an
    /// in-page `#section` link, or a redirect to one, comes back as
    /// `policy_denied` although nothing left the browser. The check exists to
    /// catch a navigation that reached a disallowed *destination*; a fragment
    /// is never sent to one.
    async fn checked_state(
        &self,
        runtime: &BrowserRuntime,
        page_id: &str,
    ) -> Result<PageState, BrowserFailure> {
        let state = self.page_state(runtime, page_id).await?;
        self.policy
            .validate_url(&without_fragment(&state.url))
            .await?;
        Ok(state)
    }

    async fn navigate(
        &self,
        runtime: &BrowserRuntime,
        page_id: &str,
        url: &str,
    ) -> Result<(), BrowserFailure> {
        let page = self.page_handle(runtime, page_id)?;
        let deadline = Instant::now() + self.settings.navigation_timeout;
        let response = self
            .call(
                runtime,
                &page.session_id,
                "Page.navigate",
                json!({ "url": url }),
                self.settings.navigation_timeout,
            )
            .await?;
        if let Some(error) = response["errorText"].as_str().filter(|e| !e.is_empty()) {
            return Err(navigation_error(error));
        }
        if let Some(loader) = response["loaderId"].as_str().map(str::to_string) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let outcome = runtime
                .shared
                .wait_for(remaining, |state| {
                    let page = state.pages.get(page_id)?;
                    if page.detached {
                        return Some(Err(stale_page()));
                    }
                    page.dom_content_loaded_since(&loader).then_some(Ok(()))
                })
                .await?;
            outcome?;
        }
        Ok(())
    }

    async fn open(&self, url: &str) -> Result<PageState, BrowserFailure> {
        let runtime = self.runtime().await?;
        let timeout = self.settings.action_timeout;
        let mut params = json!({ "url": "about:blank" });
        if let Some(context) = &runtime.context_id {
            params["browserContextId"] = json!(context);
        }
        let created = runtime
            .conn
            .call(None, "Target.createTarget", params, timeout)
            .await
            .map_err(map_cdp)?;
        let target_id = created["targetId"]
            .as_str()
            .ok_or(BrowserFailure::Execution("createTarget returned no target"))?
            .to_string();
        let mut page_id = None;
        let outcome = self
            .setup_and_navigate(&runtime, &target_id, url, &mut page_id)
            .await;
        if outcome.is_err() {
            if let Some(id) = &page_id {
                let mut state = runtime.shared.lock();
                if let Some(record) = state.pages.remove(id) {
                    state.session_index.remove(&record.session_id);
                }
            }
            let _ = runtime
                .conn
                .call(
                    None,
                    "Target.closeTarget",
                    json!({ "targetId": target_id }),
                    Duration::from_secs(5),
                )
                .await;
        }
        outcome
    }

    async fn setup_and_navigate(
        &self,
        runtime: &BrowserRuntime,
        target_id: &str,
        url: &str,
        page_id_out: &mut Option<String>,
    ) -> Result<PageState, BrowserFailure> {
        let timeout = self.settings.action_timeout;
        let session = runtime
            .shared
            .wait_for(timeout, |state| state.unclaimed.get(target_id).cloned())
            .await?;
        runtime.shared.lock().unclaimed.remove(target_id);

        self.call(runtime, &session, "Page.enable", json!({}), timeout)
            .await?;
        self.call(
            runtime,
            &session,
            "Page.setLifecycleEventsEnabled",
            json!({ "enabled": true }),
            timeout,
        )
        .await?;
        let _ = self
            .call(runtime, &session, "Inspector.enable", json!({}), timeout)
            .await;
        let tree = self
            .call(runtime, &session, "Page.getFrameTree", json!({}), timeout)
            .await?;
        let frame = &tree["frameTree"]["frame"];
        let main_frame = frame["id"].as_str().map(str::to_string);
        self.call(
            runtime,
            &session,
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
            timeout,
        )
        .await?;
        let _ = self
            .call(
                runtime,
                &session,
                "Page.setInterceptFileChooserDialog",
                json!({ "enabled": true }),
                timeout,
            )
            .await;
        if let Some((width, height)) = self.viewport() {
            self.call(
                runtime,
                &session,
                "Emulation.setDeviceMetricsOverride",
                json!({"width": width, "height": height, "deviceScaleFactor": 1, "mobile": false}),
                timeout,
            )
            .await?;
        }
        if self.policy.needs_request_interception() {
            let patterns: Value = serde_json::from_str(INTERCEPT_PATTERNS).expect("static json");
            self.call(runtime, &session, "Fetch.enable", patterns, timeout)
                .await?;
            self.call(
                runtime,
                &session,
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": WORKER_BLOCK_SCRIPT }),
                timeout,
            )
            .await?;
        }

        let page_id = uuid::Uuid::new_v4().simple().to_string();
        {
            let mut state = runtime.shared.lock();
            state.session_index.insert(session.clone(), page_id.clone());
            state.pages.insert(
                page_id.clone(),
                PageRecord {
                    target_id: target_id.to_string(),
                    session_id: session.clone(),
                    main_frame_id: main_frame,
                    url: frame["url"].as_str().unwrap_or("about:blank").to_string(),
                    loader_id: frame["loaderId"].as_str().map(str::to_string),
                    ..Default::default()
                },
            );
        }
        *page_id_out = Some(page_id.clone());
        self.call(
            runtime,
            &session,
            "Runtime.runIfWaitingForDebugger",
            json!({}),
            timeout,
        )
        .await?;

        self.policy.validate_url(url).await?;
        self.navigate(runtime, &page_id, url).await?;
        self.checked_state(runtime, &page_id).await
    }

    async fn click_at(
        &self,
        runtime: &BrowserRuntime,
        session: &str,
        x: f64,
        y: f64,
    ) -> Result<(), BrowserFailure> {
        let timeout = self.settings.action_timeout;
        self.call(
            runtime,
            session,
            "Input.dispatchMouseEvent",
            json!({"type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0}),
            timeout,
        )
        .await?;
        self.call(
            runtime,
            session,
            "Input.dispatchMouseEvent",
            json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1}),
            timeout,
        )
        .await?;
        self.call(
            runtime,
            session,
            "Input.dispatchMouseEvent",
            json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "buttons": 0, "clickCount": 1}),
            timeout,
        )
        .await?;
        Ok(())
    }

    async fn press_key(
        &self,
        runtime: &BrowserRuntime,
        session: &str,
        key: &str,
    ) -> Result<(), BrowserFailure> {
        let def = key_definition(key);
        let timeout = self.settings.action_timeout;
        let mut down = json!({
            "type": if def.text.is_some() { "keyDown" } else { "rawKeyDown" },
            "key": def.key,
            "code": def.code,
            "windowsVirtualKeyCode": def.key_code,
            "nativeVirtualKeyCode": def.key_code,
        });
        if let Some(text) = &def.text {
            down["text"] = json!(text);
            down["unmodifiedText"] = json!(text);
        }
        self.call(runtime, session, "Input.dispatchKeyEvent", down, timeout)
            .await?;
        self.call(
            runtime,
            session,
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": def.key,
                "code": def.code,
                "windowsVirtualKeyCode": def.key_code,
                "nativeVirtualKeyCode": def.key_code,
            }),
            timeout,
        )
        .await?;
        Ok(())
    }
}

/// One key as CDP wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyDefinition {
    key: String,
    code: String,
    key_code: u32,
    text: Option<String>,
}

/// Map an allow-listed key (named, any case, or one character) to its CDP
/// description. agenthicc accepts `enter` but hands Playwright the string
/// verbatim, and Playwright's key names are case-sensitive, so `enter`
/// fails there; here every allow-listed spelling presses the key.
fn key_definition(key: &str) -> KeyDefinition {
    let named = |key: &str, code: &str, key_code: u32, text: Option<&str>| KeyDefinition {
        key: key.into(),
        code: code.into(),
        key_code,
        text: text.map(str::to_string),
    };
    match key.to_lowercase().as_str() {
        "enter" => return named("Enter", "Enter", 13, Some("\r")),
        "tab" => return named("Tab", "Tab", 9, None),
        "escape" => return named("Escape", "Escape", 27, None),
        "space" | " " => return named(" ", "Space", 32, Some(" ")),
        "backspace" => return named("Backspace", "Backspace", 8, None),
        "delete" => return named("Delete", "Delete", 46, None),
        "arrowleft" => return named("ArrowLeft", "ArrowLeft", 37, None),
        "arrowup" => return named("ArrowUp", "ArrowUp", 38, None),
        "arrowright" => return named("ArrowRight", "ArrowRight", 39, None),
        "arrowdown" => return named("ArrowDown", "ArrowDown", 40, None),
        "home" => return named("Home", "Home", 36, None),
        "end" => return named("End", "End", 35, None),
        "pageup" => return named("PageUp", "PageUp", 33, None),
        "pagedown" => return named("PageDown", "PageDown", 34, None),
        _ => {}
    }
    let c = key.chars().next().unwrap_or(' ');
    let (code, key_code) = if c.is_ascii_alphabetic() {
        (
            format!("Key{}", c.to_ascii_uppercase()),
            c.to_ascii_uppercase() as u32,
        )
    } else if c.is_ascii_digit() {
        (format!("Digit{c}"), c as u32)
    } else {
        (String::new(), 0)
    };
    KeyDefinition {
        key: c.to_string(),
        code,
        key_code,
        text: Some(c.to_string()),
    }
}

/// Playwright's URL glob: `*` matches within a path segment, `**` across
/// segments, `{a,b}` alternates, `\` escapes. `?` is literal (Playwright
/// 1.52+). A pattern without wildcards compares as a normalised URL.
pub fn url_matches(pattern: &str, url: &str) -> bool {
    if !pattern.contains(['*', '{']) {
        return match (url::Url::parse(pattern), url::Url::parse(url)) {
            (Ok(a), Ok(b)) => a.as_str() == b.as_str(),
            _ => pattern == url,
        };
    }
    regex::Regex::new(&glob_to_regex(pattern))
        .map(|re| re.is_match(url))
        .unwrap_or(false)
}

fn glob_to_regex(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = String::from("^");
    let mut in_group = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            out.push_str(&regex::escape(&chars[i + 1].to_string()));
            i += 2;
            continue;
        }
        if c == '*' {
            let before = if i == 0 { None } else { Some(chars[i - 1]) };
            let mut last = i;
            while last + 1 < chars.len() && chars[last + 1] == '*' {
                last += 1;
            }
            let stars = last - i + 1;
            let after = chars.get(last + 1).copied();
            let deep = stars > 1
                && matches!(before, None | Some('/'))
                && matches!(after, None | Some('/'));
            if deep {
                out.push_str("((?:[^/]*(?:/|$))*)");
                // The `/` after `**` is part of the deep match.
                i = last + 2;
            } else {
                out.push_str("([^/]*)");
                i = last + 1;
            }
            continue;
        }
        match c {
            '{' => {
                in_group = true;
                out.push('(');
            }
            '}' => {
                in_group = false;
                out.push(')');
            }
            ',' if in_group => out.push('|'),
            _ => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    out.push('$');
    out
}

/// `url` without its fragment; unparseable input is returned unchanged (and
/// then fails the policy check on its own merits).
fn without_fragment(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

fn lifecycle_name(state: &str) -> &'static str {
    match state {
        "domcontentloaded" => "DOMContentLoaded",
        "load" => "load",
        _ => "networkIdle",
    }
}

fn string_map(value: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(object) = value.as_object() {
        for (key, v) in object {
            let text = match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            out.insert(key.clone(), Value::String(text));
        }
    }
    out
}

#[async_trait]
impl BrowserClient for CdpBrowserClient {
    async fn health(&self) -> BrowserHealth {
        let engine = &self.engine;
        let transport = engine.settings.transport_label();
        match (engine.settings.family, engine.settings.transport) {
            // agenthicc's local CloakBrowser health checks the installation
            // and does not start a browser.
            (BrowserFamily::CloakBrowser, BrowserTransport::Local) => {
                {
                    let guard = engine.runtime.lock().await;
                    if guard.as_ref().is_some_and(|r| r.is_alive()) {
                        return BrowserHealth::ready(transport);
                    }
                }
                match engine.executable() {
                    Ok(_) => BrowserHealth::ready(transport),
                    Err(e) => BrowserHealth::failed(e.kind, transport, e.message),
                }
            }
            // Playwright's and the CDP transport's health start (or attach
            // to) the browser, as agenthicc's `_get_context` does.
            _ => match engine.runtime().await {
                Ok(_) => BrowserHealth::ready(transport),
                Err(BrowserFailure::Tool(e)) => BrowserHealth::failed(e.kind, transport, e.message),
                Err(_) => {
                    let message = if engine.settings.transport == BrowserTransport::Cdp {
                        "CDP browser endpoint is unavailable."
                    } else {
                        "Playwright browser runtime is unavailable."
                    };
                    BrowserHealth::failed(BrowserErrorKind::BrowserUnavailable, transport, message)
                }
            },
        }
    }

    async fn open_page(&self, url: &str) -> Result<PageState, BrowserFailure> {
        self.engine.open(url).await
    }

    async fn snapshot(&self, page_id: &str) -> Result<PageSnapshot, BrowserFailure> {
        let engine = &self.engine;
        let runtime = engine.existing_runtime().await?;
        let max = engine.settings.max_snapshot_chars;
        let units = max.saturating_mul(2).saturating_add(16);
        let value = engine
            .evaluate(
                &runtime,
                page_id,
                "snapshot",
                json!({ "max_text_units": units }),
                engine.settings.action_timeout,
            )
            .await?;
        let text = value["text"].as_str().unwrap_or("");
        let truncated = value["more"].as_bool().unwrap_or(false) || text.chars().count() > max;
        let links = value["links"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .take(50)
                    .map(|item| {
                        let mut map = string_map(item);
                        if let Some(Value::String(href)) = map.get("href") {
                            let redacted = truncate_chars(&redact_link(href), 500);
                            map.insert("href".into(), Value::String(redacted));
                        }
                        map
                    })
                    .collect()
            })
            .unwrap_or_default();
        let controls = value["controls"]
            .as_array()
            .map(|items| items.iter().take(50).map(string_map).collect())
            .unwrap_or_default();
        Ok(PageSnapshot {
            page: PageState::new(
                page_id,
                value["url"].as_str().unwrap_or(""),
                value["title"].as_str().unwrap_or(""),
            ),
            text: truncate_chars(text, max),
            links,
            controls,
            truncated,
        })
    }

    async fn click(&self, page_id: &str, selector: &str) -> Result<PageState, BrowserFailure> {
        let engine = &self.engine;
        let runtime = engine.existing_runtime().await?;
        let page = engine.page_handle(&runtime, page_id)?;
        let ready = engine
            .wait_actionable(
                &runtime,
                page_id,
                json!({ "action": "click", "selector": selector }),
            )
            .await?;
        let x = ready["x"]
            .as_f64()
            .ok_or(BrowserFailure::Execution("probe returned no point"))?;
        let y = ready["y"]
            .as_f64()
            .ok_or(BrowserFailure::Execution("probe returned no point"))?;
        let before = engine.counters(&runtime, page_id);
        engine.click_at(&runtime, &page.session_id, x, y).await?;
        engine.settle(&runtime, page_id, before).await;
        engine.checked_state(&runtime, page_id).await
    }

    async fn fill(
        &self,
        page_id: &str,
        selector: &str,
        value: &str,
    ) -> Result<PageState, BrowserFailure> {
        let engine = &self.engine;
        let runtime = engine.existing_runtime().await?;
        let page = engine.page_handle(&runtime, page_id)?;
        let ready = engine
            .wait_actionable(
                &runtime,
                page_id,
                json!({ "action": "fill", "selector": selector, "value": value }),
            )
            .await?;
        let before = engine.counters(&runtime, page_id);
        if ready["state"].as_str() != Some("filled") {
            if value.is_empty() {
                engine
                    .press_key(&runtime, &page.session_id, "Delete")
                    .await?;
            } else {
                engine
                    .call(
                        &runtime,
                        &page.session_id,
                        "Input.insertText",
                        json!({ "text": value }),
                        engine.settings.action_timeout,
                    )
                    .await?;
            }
        }
        engine.settle(&runtime, page_id, before).await;
        engine.checked_state(&runtime, page_id).await
    }

    async fn press(
        &self,
        page_id: &str,
        key: &str,
        selector: &str,
    ) -> Result<PageState, BrowserFailure> {
        let engine = &self.engine;
        let runtime = engine.existing_runtime().await?;
        let page = engine.page_handle(&runtime, page_id)?;
        engine
            .wait_actionable(
                &runtime,
                page_id,
                json!({ "action": "press", "selector": selector }),
            )
            .await?;
        let before = engine.counters(&runtime, page_id);
        engine.press_key(&runtime, &page.session_id, key).await?;
        engine.settle(&runtime, page_id, before).await;
        engine.checked_state(&runtime, page_id).await
    }

    async fn wait_for(
        &self,
        page_id: &str,
        condition: &str,
        value: &str,
    ) -> Result<PageState, BrowserFailure> {
        let engine = &self.engine;
        let runtime = engine.existing_runtime().await?;
        engine.page_handle(&runtime, page_id)?;
        let timeout = engine.settings.action_timeout;
        match condition {
            "selector" => {
                engine
                    .wait_actionable(
                        &runtime,
                        page_id,
                        json!({ "action": "visible", "selector": value }),
                    )
                    .await?;
            }
            "text" => {
                engine
                    .wait_actionable(
                        &runtime,
                        page_id,
                        json!({ "action": "text", "value": value }),
                    )
                    .await?;
            }
            "url" => {
                runtime
                    .shared
                    .wait_for(timeout, |state| {
                        let page = state.pages.get(page_id)?;
                        if page.detached {
                            return Some(Err(stale_page()));
                        }
                        url_matches(value, &page.url).then_some(Ok(()))
                    })
                    .await??;
                return engine.checked_state(&runtime, page_id).await;
            }
            _ => {
                if !LOAD_STATES.contains(&value) {
                    return Err(BrowserFailure::tool(
                        BrowserErrorKind::InvalidInput,
                        "Load state is invalid.",
                    ));
                }
                let name = lifecycle_name(value);
                runtime
                    .shared
                    .wait_for(timeout, |state| {
                        let page = state.pages.get(page_id)?;
                        if page.detached {
                            return Some(Err(stale_page()));
                        }
                        page.lifecycle.contains(name).then_some(Ok(()))
                    })
                    .await??;
            }
        }
        engine.page_state(&runtime, page_id).await
    }

    async fn screenshot(
        &self,
        page_id: &str,
        image_type: &str,
        full_page: bool,
    ) -> Result<ScreenshotData, BrowserFailure> {
        let engine = &self.engine;
        let runtime = engine.existing_runtime().await?;
        let page = engine.page_handle(&runtime, page_id)?;
        let image_type = image_type.to_lowercase();
        if image_type != "png" && image_type != "jpeg" {
            return Err(BrowserFailure::tool(
                BrowserErrorKind::InvalidInput,
                "Screenshot format must be png or jpeg.",
            ));
        }
        let timeout = engine.settings.action_timeout;
        let mut params = json!({ "format": image_type, "fromSurface": true });
        if full_page {
            let metrics = engine
                .call(
                    &runtime,
                    &page.session_id,
                    "Page.getLayoutMetrics",
                    json!({}),
                    timeout,
                )
                .await?;
            let size = if metrics["cssContentSize"].is_object() {
                &metrics["cssContentSize"]
            } else {
                &metrics["contentSize"]
            };
            let width = size["width"].as_f64().unwrap_or(1280.0).ceil().max(1.0);
            let height = size["height"].as_f64().unwrap_or(720.0).ceil().max(1.0);
            params["captureBeyondViewport"] = json!(true);
            params["clip"] = json!({"x": 0, "y": 0, "width": width, "height": height, "scale": 1});
        }
        let shot = engine
            .call(
                &runtime,
                &page.session_id,
                "Page.captureScreenshot",
                params,
                timeout,
            )
            .await?;
        let content = base64::engine::general_purpose::STANDARD
            .decode(shot["data"].as_str().unwrap_or(""))
            .map_err(|_| BrowserFailure::Execution("screenshot payload was not base64"))?;
        if content.len() > engine.settings.max_screenshot_bytes {
            return Err(BrowserFailure::tool(
                BrowserErrorKind::OutputLimit,
                "Screenshot exceeds the configured size limit.",
            ));
        }
        Ok(ScreenshotData {
            content,
            mime_type: if image_type == "jpeg" {
                "image/jpeg"
            } else {
                "image/png"
            },
        })
    }

    async fn close_page(&self, page_id: &str) -> Result<(), BrowserFailure> {
        let engine = &self.engine;
        let runtime = engine.existing_runtime().await?;
        let page = engine.page_handle(&runtime, page_id)?;
        {
            let mut state = runtime.shared.lock();
            state.pages.remove(page_id);
            state.session_index.remove(&page.session_id);
        }
        let _ = runtime
            .conn
            .call(
                None,
                "Target.closeTarget",
                json!({ "targetId": page.target_id }),
                engine.settings.action_timeout,
            )
            .await;
        Ok(())
    }

    async fn close_session(&self) -> Result<(), BrowserFailure> {
        let runtime = self.engine.runtime.lock().await.take();
        if let Some(runtime) = runtime {
            runtime.shutdown().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_map_case_insensitively() {
        let enter = key_definition("enter");
        assert_eq!(
            (enter.key.as_str(), enter.key_code, enter.text.as_deref()),
            ("Enter", 13, Some("\r"))
        );
        assert_eq!(key_definition("ArrowDown").code, "ArrowDown");
        assert_eq!(key_definition("Space").key, " ");
        let a = key_definition("a");
        assert_eq!(
            (a.code.as_str(), a.key_code, a.text.as_deref()),
            ("KeyA", 65, Some("a"))
        );
        assert_eq!(key_definition("7").code, "Digit7");
        assert_eq!(key_definition("é").key_code, 0);
    }

    #[test]
    fn url_globs_follow_playwright() {
        assert!(url_matches("https://example.com/", "https://example.com/"));
        assert!(url_matches("https://example.com", "https://example.com/"));
        assert!(!url_matches(
            "https://example.com/a",
            "https://example.com/b"
        ));
        assert!(url_matches("**/login", "https://example.com/app/login"));
        assert!(url_matches(
            "https://example.com/*",
            "https://example.com/next"
        ));
        assert!(!url_matches(
            "https://example.com/*",
            "https://example.com/a/b"
        ));
        assert!(url_matches(
            "https://example.com/**",
            "https://example.com/a/b"
        ));
        assert!(url_matches(
            "https://example.com/{a,b}",
            "https://example.com/b"
        ));
        assert!(!url_matches(
            "https://example.com/{a,b}",
            "https://example.com/c"
        ));
        assert!(url_matches(
            "https://example.com/page?*",
            "https://example.com/page?x=1"
        ));
        assert!(!url_matches(
            "https://example.com/pa?e",
            "https://example.com/page"
        ));
    }

    #[test]
    fn navigation_errors_map_to_buckets() {
        assert_eq!(
            navigation_error("net::ERR_BLOCKED_BY_CLIENT"),
            BrowserFailure::tool(
                BrowserErrorKind::PolicyDenied,
                "Destination is not allow-listed."
            )
        );
        assert_eq!(
            navigation_error("net::ERR_NAME_NOT_RESOLVED"),
            BrowserFailure::Network
        );
        assert!(matches!(
            navigation_error("net::ERR_ABORTED"),
            BrowserFailure::Execution(_)
        ));
        assert!(matches!(
            navigation_error("Cannot navigate to invalid URL"),
            BrowserFailure::Execution(_)
        ));
    }

    #[test]
    fn lifecycle_tracking_follows_the_document() {
        let mut page = PageRecord {
            main_frame_id: Some("F".into()),
            ..Default::default()
        };
        page.loader_seq = 1;
        page.loader_id = Some("L1".into());
        page.loader_history.push_back(("L1".into(), 1));
        page.lifecycle.insert("DOMContentLoaded".into());
        assert!(page.dom_content_loaded_since("L1"));
        // A client-side redirect committed a newer document that has loaded.
        page.loader_seq = 2;
        page.loader_id = Some("L2".into());
        page.loader_history.push_back(("L2".into(), 2));
        assert!(page.dom_content_loaded_since("L1"));
        page.lifecycle.clear();
        assert!(!page.dom_content_loaded_since("L1"));
        assert!(!page.dom_content_loaded_since("unknown"));
        assert!(page.is_main_frame(Some("F")));
        assert!(!page.is_main_frame(Some("child")));
    }

    /// Path traversal: a persistent profile can never be created outside the
    /// workspace, even from settings that skipped config validation.
    #[tokio::test]
    async fn a_traversing_profile_root_is_refused_before_launch() {
        let workspace = tempfile::TempDir::new().unwrap();
        let exe = workspace.path().join("fake-chrome");
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let settings = BrowserSettings::from_playwright(&xaft_config::PlaywrightConfig {
            executable_path: exe.display().to_string(),
            allow_persistent_profiles: true,
            profile_root: "../escape".into(),
            ..Default::default()
        });
        let policy = Arc::new(BrowserPolicy::playwright(&[], true).unwrap());
        let client = CdpBrowserClient::new(
            settings,
            policy,
            workspace.path().join("ws"),
            "session-1",
            BrowserLaunchOptions::default(),
        );
        let health = client.health().await;
        assert_eq!(health.status, "policy_denied", "{health:?}");
        assert_eq!(
            health.message,
            "Persistent browser profiles must stay inside the workspace."
        );
        assert!(!workspace.path().join("escape").exists());
    }

    #[test]
    fn landed_urls_lose_only_their_fragment() {
        assert_eq!(
            without_fragment("http://127.0.0.1:8080/next?token=x#frag"),
            "http://127.0.0.1:8080/next?token=x"
        );
        assert_eq!(
            without_fragment("chrome-error://chromewebdata/"),
            "chrome-error://chromewebdata/"
        );
        assert_eq!(without_fragment("not a url"), "not a url");
    }

    #[test]
    fn string_maps_flatten_values() {
        let map = string_map(&json!({"a": "x", "b": 3, "c": null, "d": true}));
        assert_eq!(map["a"], "x");
        assert_eq!(map["b"], "3");
        assert_eq!(map["c"], "");
        assert_eq!(map["d"], "true");
    }
}
