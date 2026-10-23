//! Startup phases and readiness reports — parity slash row 19 (`/startup`).
//!
//! A port of agenthicc's `StartupCoordinator` (`runners/startup.py`). Startup
//! is split into observable phases; each has a [`ReadinessState`], a
//! monotonic elapsed time, and a bounded, redacted error summary. The TUI
//! records the work it really does — configuration load, runtime bootstrap,
//! the session store, transcript restore on `--resume`, the terminal manager,
//! the loop scheduler, and (after the first frame, like agenthicc's deferred
//! extension discovery) the workspace index and skills — and `/startup`
//! prints the report.
//!
//! # What is deliberately not ported
//!
//! agenthicc's coordinator also owns cancellable asyncio tasks and a
//! `wait_for(...)` dependency barrier, because its provider, MCP catalogue
//! and browser runtime load *behind* phases that later operations block on.
//! xaft has no operation that waits on a startup phase: the provider is built
//! per run, and the deferred phases here only feed pickers. The coordinator is
//! therefore a thread-safe report store (a `Clone` handle over one shared
//! state), and the TUI owns the deferred tasks themselves. [`close`] still
//! marks anything left loading as cancelled at shutdown.
//!
//! Diagnostics never carry secrets: an error passes through
//! [`redact_error`] (agenthicc's `_redact_error`) and is cut to 500
//! characters before it is stored.
//!
//! [`close`]: StartupCoordinator::close

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Lifecycle state of one startup phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadinessState {
    /// Never begun.
    NotStarted,
    /// In progress.
    Loading,
    /// Finished successfully.
    Ready,
    /// Finished with a fallback; the session works, with less.
    Degraded,
    /// Finished with an error.
    Failed,
    /// Abandoned, e.g. because the session closed first.
    Cancelled,
}

impl ReadinessState {
    /// agenthicc's wire string, e.g. `"not_started"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Redacted timing and status for one phase.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupPhaseReport {
    /// Phase name, e.g. `"configuration"`.
    pub name: String,
    /// Current state.
    pub state: ReadinessState,
    /// Whether the phase runs after the first frame.
    pub deferred: bool,
    /// Elapsed time; for a loading phase, time so far.
    pub elapsed: Option<Duration>,
    /// Redacted, bounded error summary.
    pub error: Option<String>,
}

impl StartupPhaseReport {
    /// Elapsed seconds, as agenthicc's `elapsed_s` reports them.
    pub fn elapsed_s(&self) -> Option<f64> {
        self.elapsed.map(|d| d.as_secs_f64())
    }

    /// agenthicc's `to_dict()`: a JSON-safe report.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "state": self.state.as_str(),
            "deferred": self.deferred,
            "elapsed_s": self.elapsed_s(),
            "error": self.error,
        })
    }

    /// The `/startup` line: `name: state (0.012s)`, plus agenthicc's
    /// documented `deferred` marker and any error.
    pub fn render(&self) -> String {
        let elapsed = self
            .elapsed_s()
            .map(|s| format!("{s:.3}s"))
            .unwrap_or_else(|| "—".to_string());
        let mut line = format!("{}: {} ({elapsed})", self.name, self.state.as_str());
        if self.deferred {
            line.push_str(" · deferred");
        }
        if let Some(error) = &self.error {
            line.push_str(" · ");
            line.push_str(error);
        }
        line
    }
}

#[derive(Debug, Clone)]
struct PhaseRecord {
    state: ReadinessState,
    deferred: bool,
    started_at: Instant,
    finished_at: Option<Instant>,
    error: Option<String>,
}

#[derive(Debug, Default)]
struct Inner {
    phases: BTreeMap<String, PhaseRecord>,
    closed: bool,
}

/// Owns the session's phase reports.
///
/// `Clone` hands out another handle to the same reports, so the composition
/// root, deferred tasks and `/startup` all see one timeline.
#[derive(Debug, Clone, Default)]
pub struct StartupCoordinator {
    inner: Arc<Mutex<Inner>>,
}

impl StartupCoordinator {
    /// An empty coordinator.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Mark `name` as loading, replacing any previous report.
    pub fn begin(&self, name: &str) {
        self.begin_inner(name, false);
    }

    /// Mark `name` as loading after the first frame.
    pub fn begin_deferred(&self, name: &str) {
        self.begin_inner(name, true);
    }

    fn begin_inner(&self, name: &str, deferred: bool) {
        assert!(!name.is_empty(), "startup phase name must not be empty");
        self.lock().phases.insert(
            name.to_string(),
            PhaseRecord {
                state: ReadinessState::Loading,
                deferred,
                started_at: Instant::now(),
                finished_at: None,
                error: None,
            },
        );
    }

    /// Finish `name` as ready.
    pub fn finish(&self, name: &str) {
        self.finish_with(name, ReadinessState::Ready, None);
    }

    /// Finish `name` with an explicit state and an optional error, which is
    /// redacted and bounded before it is stored. Finishing a phase that never
    /// began records a zero-length phase, as agenthicc does.
    pub fn finish_with(&self, name: &str, state: ReadinessState, error: Option<&str>) {
        let mut inner = self.lock();
        let now = Instant::now();
        let record = inner
            .phases
            .entry(name.to_string())
            .or_insert_with(|| PhaseRecord {
                state: ReadinessState::Loading,
                deferred: false,
                started_at: now,
                finished_at: None,
                error: None,
            });
        record.state = state;
        record.finished_at = Some(now);
        record.error = error.filter(|e| !e.is_empty()).map(redact_error);
    }

    /// One phase's report; a never-begun phase reports `not_started`.
    pub fn report(&self, name: &str) -> StartupPhaseReport {
        let inner = self.lock();
        match inner.phases.get(name) {
            Some(record) => to_report(name, record),
            None => StartupPhaseReport {
                name: name.to_string(),
                state: ReadinessState::NotStarted,
                deferred: false,
                elapsed: None,
                error: None,
            },
        }
    }

    /// Every recorded phase, in phase-name order (agenthicc's `snapshot`).
    pub fn snapshot(&self) -> Vec<StartupPhaseReport> {
        let inner = self.lock();
        inner
            .phases
            .iter()
            .map(|(name, record)| to_report(name, record))
            .collect()
    }

    /// agenthicc's `to_dict()` over the whole snapshot.
    pub fn to_json(&self) -> Vec<serde_json::Value> {
        self.snapshot().iter().map(|r| r.to_json()).collect()
    }

    /// Names of the phases still loading.
    pub fn loading(&self) -> Vec<String> {
        let inner = self.lock();
        inner
            .phases
            .iter()
            .filter(|(_, r)| r.state == ReadinessState::Loading)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Whether [`StartupCoordinator::close`] ran.
    pub fn closed(&self) -> bool {
        self.lock().closed
    }

    /// Session shutdown: anything still loading is cancelled. Idempotent.
    pub fn close(&self) {
        let pending: Vec<String> = {
            let mut inner = self.lock();
            if inner.closed {
                return;
            }
            inner.closed = true;
            inner
                .phases
                .iter()
                .filter(|(_, r)| r.state == ReadinessState::Loading)
                .map(|(name, _)| name.clone())
                .collect()
        };
        for name in pending {
            self.finish_with(&name, ReadinessState::Cancelled, Some("session closed"));
        }
    }
}

fn to_report(name: &str, record: &PhaseRecord) -> StartupPhaseReport {
    let end = record.finished_at.unwrap_or_else(Instant::now);
    StartupPhaseReport {
        name: name.to_string(),
        state: record.state,
        deferred: record.deferred,
        elapsed: Some(end.saturating_duration_since(record.started_at)),
        error: record.error.clone(),
    }
}

/// Longest error summary a report keeps, in characters.
pub const MAX_ERROR_CHARS: usize = 500;

/// Bound and redact error text before it enters diagnostics.
///
/// agenthicc's `_redact_error`: `Bearer <token>` becomes `Bearer <redacted>`,
/// and any of `authorization`, `api key`, `token`, `secret`, `password`,
/// `passwd`, `cookie` or `private key` followed by `:`/`=`/whitespace and a
/// value becomes `<word>=<redacted>`. Case-insensitive, then cut to 500
/// characters. A scanner rather than a regex, so the runtime takes no new
/// dependency for two patterns.
pub fn redact_error(error: &str) -> String {
    let bearer = redact_bearer(error);
    let secrets = redact_secret_assignments(&bearer);
    secrets.chars().take(MAX_ERROR_CHARS).collect()
}

fn is_value_char(c: char) -> bool {
    !c.is_whitespace() && c != ',' && c != ';'
}

fn redact_bearer(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
        if boundary && i + 6 <= chars.len() {
            let word: String = chars[i..i + 6].iter().collect();
            if word.eq_ignore_ascii_case("bearer") {
                let mut j = i + 6;
                let ws_start = j;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                let value_start = j;
                while j < chars.len() && is_value_char(chars[j]) {
                    j += 1;
                }
                if j > ws_start && value_start > ws_start && j > value_start {
                    out.push_str("Bearer <redacted>");
                    i = j;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The sensitive words, as alternatives: each `[_ -]?` in agenthicc's regex
/// is expanded explicitly.
const SECRET_WORDS: &[&str] = &[
    "authorization",
    "api_key",
    "api-key",
    "api key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "cookie",
    "private_key",
    "private-key",
    "private key",
    "privatekey",
];

fn redact_secret_assignments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    if lower.len() != chars.len() {
        // A lowercase mapping that changes length (rare, non-ASCII) would
        // misalign the two views; fall back to the original text.
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    'scan: while i < chars.len() {
        for word in SECRET_WORDS {
            let w: Vec<char> = word.chars().collect();
            if i + w.len() > lower.len() || lower[i..i + w.len()] != w[..] {
                continue;
            }
            // Separator: `\s*[:=]\s*` or `\s+`.
            let mut j = i + w.len();
            let sep_start = j;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == ':' || chars[j] == '=') {
                j += 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
            } else if j == sep_start {
                continue;
            }
            let value_start = j;
            while j < chars.len() && is_value_char(chars[j]) {
                j += 1;
            }
            if j == value_start {
                continue;
            }
            let original: String = chars[i..i + w.len()].iter().collect();
            out.push_str(&original);
            out.push_str("=<redacted>");
            i = j;
            continue 'scan;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_move_from_loading_to_a_finished_state() {
        let startup = StartupCoordinator::new();
        startup.begin("configuration");
        assert_eq!(
            startup.report("configuration").state,
            ReadinessState::Loading
        );
        startup.finish("configuration");
        let report = startup.report("configuration");
        assert_eq!(report.state, ReadinessState::Ready);
        assert!(report.elapsed_s().is_some());
        assert!(!report.deferred);

        startup.begin_deferred("skills");
        assert_eq!(startup.loading(), vec!["skills".to_string()]);
        startup.finish_with("skills", ReadinessState::Degraded, Some("one skill failed"));
        let skills = startup.report("skills");
        assert!(skills.deferred);
        assert_eq!(skills.state, ReadinessState::Degraded);
        assert_eq!(skills.error.as_deref(), Some("one skill failed"));
        assert!(startup.loading().is_empty());
    }

    #[test]
    fn an_unknown_phase_reports_not_started_and_snapshots_are_name_ordered() {
        let startup = StartupCoordinator::new();
        assert_eq!(startup.report("mcp").state, ReadinessState::NotStarted);
        assert!(startup.snapshot().is_empty());
        startup.begin("zeta");
        startup.finish("zeta");
        startup.begin("alpha");
        let names: Vec<String> = startup.snapshot().into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["alpha".to_string(), "zeta".to_string()]);
    }

    #[test]
    fn close_cancels_whatever_is_still_loading() {
        let startup = StartupCoordinator::new();
        startup.begin("done");
        startup.finish("done");
        startup.begin_deferred("pending");
        startup.close();
        assert!(startup.closed());
        assert_eq!(startup.report("done").state, ReadinessState::Ready);
        let pending = startup.report("pending");
        assert_eq!(pending.state, ReadinessState::Cancelled);
        assert_eq!(pending.error.as_deref(), Some("session closed"));
        startup.close();
    }

    #[test]
    fn clones_share_one_timeline() {
        let a = StartupCoordinator::new();
        let b = a.clone();
        a.begin("runtime");
        b.finish("runtime");
        assert_eq!(a.report("runtime").state, ReadinessState::Ready);
    }

    #[test]
    fn reports_render_like_agenthiccs_startup_lines() {
        let report = StartupPhaseReport {
            name: "configuration".into(),
            state: ReadinessState::Ready,
            deferred: false,
            elapsed: Some(Duration::from_millis(12)),
            error: None,
        };
        assert_eq!(report.render(), "configuration: ready (0.012s)");
        let json = report.to_json();
        assert_eq!(json["state"], serde_json::json!("ready"));
        assert_eq!(json["deferred"], serde_json::json!(false));

        let never = StartupPhaseReport {
            name: "mcp".into(),
            state: ReadinessState::NotStarted,
            deferred: true,
            elapsed: None,
            error: Some("boom".into()),
        };
        assert_eq!(never.render(), "mcp: not_started (—) · deferred · boom");
    }

    #[test]
    fn errors_are_redacted_and_bounded() {
        // Both passes fire here, exactly as agenthicc's two `re.sub`s do: the
        // bearer pass first, then `Authorization: <value>`.
        assert_eq!(
            redact_error("401 from provider: Authorization: Bearer abc.def"),
            "401 from provider: Authorization=<redacted> <redacted>"
        );
        assert_eq!(
            redact_error("got bearer abc.def back"),
            "got Bearer <redacted> back"
        );
        assert_eq!(
            redact_error("bad api_key=sk-123, retry"),
            "bad api_key=<redacted>, retry"
        );
        assert_eq!(redact_error("password hunter2"), "password=<redacted>");
        assert_eq!(redact_error("TOKEN : xyz;"), "TOKEN=<redacted>;");
        assert_eq!(redact_error("no secrets here"), "no secrets here");
        let long = "x".repeat(2_000);
        assert_eq!(redact_error(&long).chars().count(), MAX_ERROR_CHARS);
    }
}
