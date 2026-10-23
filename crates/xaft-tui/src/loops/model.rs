//! The durable loop record and the small text helpers `/loop` shares.
//!
//! Field names and state strings are agenthicc's `LoopRecord.to_mapping()`
//! (`agenthicc/runners/loop_scheduler.py`) verbatim, so a `loop.json` written by
//! either implementation describes the same thing.

use serde::{Deserialize, Serialize};

/// Durable lifecycle states for one session loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopLifecycle {
    /// Waiting for its next due time.
    Scheduled,
    /// Due, but the session was busy; one coalesced iteration is pending.
    WaitingForIdle,
    /// An iteration has been dispatched and has not finished.
    Running,
    /// Paused by the user; resumable.
    Paused,
    /// Stopped by the user. Terminal.
    Stopped,
    /// Reached `max_age_s`. Terminal.
    Expired,
    /// Hit `max_consecutive_failures`. Terminal.
    Failed,
}

impl LoopLifecycle {
    /// The wire string, e.g. `"waiting_for_idle"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::WaitingForIdle => "waiting_for_idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }

    /// Whether the loop can still schedule work (agenthicc's `active`).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Scheduled | Self::WaitingForIdle | Self::Running | Self::Paused
        )
    }

    /// Stopped, expired or failed — nothing will ever run again.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Expired | Self::Failed)
    }
}

/// The two payloads `/loop` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopPayloadKind {
    /// A prompt submitted as a normal user turn.
    Prompt,
    /// A registered slash command, e.g. `/status`.
    Command,
}

impl LoopPayloadKind {
    /// The wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Command => "command",
        }
    }

    /// `/`-prefixed payloads are commands, everything else is a prompt.
    pub fn of(payload: &str) -> Self {
        if payload.starts_with('/') {
            Self::Command
        } else {
            Self::Prompt
        }
    }
}

/// The only schema version this build reads or writes.
pub const LOOP_SCHEMA_VERSION: u32 = 1;

fn default_pending() -> bool {
    false
}

/// Versioned, serialisable state for one session-scoped loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopRecord {
    /// Always [`LOOP_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Random hex id, new for every `/loop <prompt>`.
    pub loop_id: String,
    /// The session that owns the loop.
    pub session_id: String,
    /// The conversation iterations run in — always the session itself.
    pub conversation_id: String,
    /// Prompt or slash command.
    pub payload_kind: LoopPayloadKind,
    /// The prompt text or the command line.
    pub payload: String,
    /// Seconds between the end of one iteration and the start of the next.
    pub interval_s: u64,
    /// Epoch seconds.
    pub created_at: f64,
    /// Epoch seconds.
    pub updated_at: f64,
    /// Epoch seconds at which the next iteration becomes due.
    pub next_due_at: f64,
    /// Epoch seconds after which the loop expires.
    pub expires_at: f64,
    /// Current lifecycle state.
    pub state: LoopLifecycle,
    /// Bumped on every persisted change.
    pub revision: u64,
    /// Iterations dispatched.
    pub runs: u64,
    /// Due times that found the session busy and were folded into one run.
    pub coalesced_runs: u64,
    /// Failed iterations, ever.
    pub failures: u64,
    /// Failed iterations since the last success.
    pub consecutive_failures: u32,
    /// Epoch seconds of the last dispatch.
    #[serde(default)]
    pub last_dispatch_at: Option<f64>,
    /// Epoch seconds of the last completion (success or failure).
    #[serde(default)]
    pub last_completed_at: Option<f64>,
    /// Redacted summary of the last failure.
    pub last_error: String,
    /// Whether an iteration is owed (due now, or waiting for idle).
    #[serde(default = "default_pending")]
    pub pending: bool,
}

impl LoopRecord {
    /// agenthicc's `__post_init__` checks, so a hand-edited or foreign file
    /// that breaks an invariant is refused rather than scheduled.
    pub fn validate(&self) -> Result<(), String> {
        if self.loop_id.is_empty() || self.session_id.is_empty() || self.conversation_id.is_empty()
        {
            return Err("loop identity fields must be non-empty".into());
        }
        if self.payload.trim().is_empty() {
            return Err("loop payload must be non-empty".into());
        }
        if self.interval_s == 0 {
            return Err("loop interval must be positive".into());
        }
        if self.expires_at < self.created_at {
            return Err("loop expiry cannot precede creation".into());
        }
        if self.schema_version != LOOP_SCHEMA_VERSION {
            return Err(format!(
                "unsupported loop schema version {}",
                self.schema_version
            ));
        }
        Ok(())
    }

    /// The display form of the job id (agenthicc's `loop_id[:12]`).
    pub fn short_id(&self) -> String {
        self.loop_id.chars().take(12).collect()
    }
}

/// Largest duration `/loop` accepts, seconds (agenthicc's `2**31 - 1`).
pub const MAX_DURATION_SECONDS: u64 = (1 << 31) - 1;

/// The single error message for a malformed interval.
pub const INTERVAL_SHAPE_ERROR: &str = "interval must look like 30s, 5m, 1h, or 2d";

/// Whether `token` has the strict `integer + s/m/h/d` shape.
pub fn looks_like_duration(token: &str) -> bool {
    let Some(unit) = token.chars().last() else {
        return false;
    };
    let digits = &token[..token.len() - unit.len_utf8()];
    matches!(unit.to_ascii_lowercase(), 's' | 'm' | 'h' | 'd')
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Parse a strict `integer + s/m/h/d` duration into seconds.
///
/// agenthicc's `parse_loop_duration`: case-insensitive unit, no sign, no
/// fraction, no bare number, and the result must be in `1..=2^31-1`.
pub fn parse_loop_duration(value: &str) -> Result<u64, String> {
    let token = value.trim();
    if !looks_like_duration(token) {
        return Err(INTERVAL_SHAPE_ERROR.into());
    }
    let unit = token.chars().last().expect("checked above");
    let digits = &token[..token.len() - unit.len_utf8()];
    let multiplier: u64 = match unit.to_ascii_lowercase() {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        _ => 86_400,
    };
    let range_error = || "interval is outside the supported duration range".to_string();
    let count: u64 = digits.parse().map_err(|_| range_error())?;
    let seconds = count.checked_mul(multiplier).ok_or_else(range_error)?;
    if seconds == 0 || seconds > MAX_DURATION_SECONDS {
        return Err(range_error());
    }
    Ok(seconds)
}

/// A compact duration for status lines: `2h`, `5m`, `61s`.
pub fn format_loop_duration(seconds: f64) -> String {
    let value = if seconds.is_finite() && seconds > 0.0 {
        seconds as u64
    } else {
        0
    };
    if value != 0 && value % 86_400 == 0 {
        return format!("{}d", value / 86_400);
    }
    if value != 0 && value % 3_600 == 0 {
        return format!("{}h", value / 3_600);
    }
    if value != 0 && value % 60 == 0 {
        return format!("{}m", value / 60);
    }
    format!("{value}s")
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Mask credential-shaped tokens and bound the text.
///
/// agenthicc's `_redacted_preview`: the regex
/// `(?i)\b(sk|pk|api|token|bearer)[-_ ][A-Za-z0-9._-]+` becomes
/// `<prefix> [redacted]`, whitespace is collapsed, and the result is cut to
/// `limit` characters with an ellipsis. Implemented as a scanner rather than a
/// regex so this crate does not grow a dependency for one pattern.
pub fn redacted_preview(payload: &str, limit: usize) -> String {
    const PREFIXES: &[&str] = &["sk", "pk", "api", "token", "bearer"];
    let chars: Vec<char> = payload.chars().collect();
    let mut out = String::with_capacity(payload.len());
    let mut i = 0;
    while i < chars.len() {
        let at_boundary = i == 0 || !is_word_char(chars[i - 1]);
        let mut matched = None;
        if at_boundary {
            for prefix in PREFIXES {
                let n = prefix.chars().count();
                // Room for the prefix, one separator and at least one secret char.
                if i + n + 1 >= chars.len() {
                    continue;
                }
                let candidate: String = chars[i..i + n].iter().collect();
                if !candidate.eq_ignore_ascii_case(prefix) {
                    continue;
                }
                let sep = chars.get(i + n).copied();
                if !matches!(sep, Some('-') | Some('_') | Some(' ')) {
                    continue;
                }
                let mut end = i + n + 1;
                while end < chars.len() && is_secret_char(chars[end]) {
                    end += 1;
                }
                if end > i + n + 1 {
                    matched = Some((candidate, end));
                    break;
                }
            }
        }
        match matched {
            Some((prefix, end)) => {
                out.push_str(&prefix);
                out.push_str(" [redacted]");
                i = end;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > limit {
        let mut cut: String = collapsed.chars().take(limit).collect();
        cut.push('…');
        cut
    } else {
        collapsed
    }
}

/// A failure message as `last_error` stores it (agenthicc's
/// `_redacted_error`: the same masking, bounded to 240 characters).
pub fn redacted_error(message: &str) -> String {
    redacted_preview(message, 240)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_duration_parser_is_strict() {
        assert_eq!(parse_loop_duration("5m"), Ok(300));
        assert_eq!(parse_loop_duration("2H"), Ok(7_200));
        assert_eq!(parse_loop_duration("1d"), Ok(86_400));
        assert_eq!(parse_loop_duration(" 30s "), Ok(30));
        for bad in ["", "5", "-1m", "1w", "1.5h", "0s", "m", "5mm"] {
            assert!(
                parse_loop_duration(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        assert!(
            parse_loop_duration("99999999999999999999d")
                .unwrap_err()
                .contains("outside")
        );
        assert!(
            parse_loop_duration("24856d")
                .unwrap_err()
                .contains("outside")
        );
    }

    #[test]
    fn durations_format_compactly() {
        assert_eq!(format_loop_duration(7_200.0), "2h");
        assert_eq!(format_loop_duration(61.0), "61s");
        assert_eq!(format_loop_duration(600.0), "10m");
        assert_eq!(format_loop_duration(259_200.0), "3d");
        assert_eq!(format_loop_duration(0.0), "0s");
        assert_eq!(format_loop_duration(-5.0), "0s");
        assert_eq!(format_loop_duration(59.9), "59s");
    }

    #[test]
    fn previews_mask_credentials_and_collapse_whitespace() {
        assert_eq!(
            redacted_preview("deploy with token-abc123 now", 96),
            "deploy with token [redacted] now"
        );
        assert_eq!(
            redacted_preview("use sk_live.KEY please", 96),
            "use sk [redacted] please"
        );
        assert_eq!(redacted_preview("Bearer xyz", 96), "Bearer [redacted]");
        // `\b` — a prefix inside a word is not a secret.
        assert_eq!(redacted_preview("task-list", 96), "task-list");
        assert_eq!(redacted_preview("a\n\n  b", 96), "a b");
        assert_eq!(redacted_preview("abcdef", 3), "abc…");
    }

    #[test]
    fn records_round_trip_with_agenthiccs_field_names() {
        let record = LoopRecord {
            schema_version: 1,
            loop_id: "loop-1".into(),
            session_id: "s".into(),
            conversation_id: "s".into(),
            payload_kind: LoopPayloadKind::Prompt,
            payload: "inspect".into(),
            interval_s: 60,
            created_at: 1.0,
            updated_at: 1.0,
            next_due_at: 1.0,
            expires_at: 100.0,
            state: LoopLifecycle::WaitingForIdle,
            revision: 0,
            runs: 0,
            coalesced_runs: 0,
            failures: 0,
            consecutive_failures: 0,
            last_dispatch_at: None,
            last_completed_at: None,
            last_error: String::new(),
            pending: true,
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["state"], serde_json::json!("waiting_for_idle"));
        assert_eq!(json["payload_kind"], serde_json::json!("prompt"));
        let back: LoopRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, record);
        assert!(back.validate().is_ok());

        let mut broken = record.clone();
        broken.schema_version = 2;
        assert!(broken.validate().unwrap_err().contains("schema version"));
        broken = record;
        broken.expires_at = 0.0;
        assert!(broken.validate().unwrap_err().contains("expiry"));
    }
}
