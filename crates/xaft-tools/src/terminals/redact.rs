//! Redaction and bounding for everything a background terminal persists.
//!
//! # Why this is a module and not a helper inside the manager
//!
//! Because the ordering is the security property. A background terminal writes
//! its captured stdout/stderr to an append-only JSONL file on disk
//! ([`super::record::TerminalStore`]), so anything that reaches that file is
//! durable and greppable long after the process is gone. The single most
//! obvious way to leak a credential is `cat .env` or `printenv` run in the
//! background: the manager captures it, and if capture happens *before*
//! redaction the secret is already on disk.
//!
//! So every write path in the manager goes through [`redact_output`] and
//! [`bounded_append`] *before* the record is handed to the store. There is no
//! code path that persists a raw chunk, and `redaction_happens_before_a_record
//! _reaches_the_store` pins that by reading the JSONL back.
//!
//! # The two patterns, and why there are two
//!
//! agenthicc uses exactly two, and they are kept verbatim because they cover
//! disjoint shapes:
//!
//! * `_SECRET_FLAGS` — the command-line form: `--token abc`, `--token=abc`.
//! * `_SECRET_ASSIGNMENT` — the environment/`.env` form: `TOKEN=abc`.
//!
//! The second is what catches a `cat .env`, the first is what catches a
//! `curl -H "--authorization: Bearer ..."`-style argument captured in `argv`.
//! Whitespace is collapsed by [`redact_text`] but deliberately *not* by
//! [`redact_output`], because collapsing would destroy the line structure the
//! agent reads and a redacted transcript that no longer parses is worse than
//! a long one.
//!
//! # Known divergence, stated rather than hidden
//!
//! Neither pattern matches a bare `export TOKEN` (no `=`), nor a value that is
//! the entire line with no key (`sk-live-abc123`). agenthicc has the same
//! limits — this is a redaction net for *credential-shaped* text, not a
//! guarantee. Anything stronger is a secret-scanner, which is a different tool
//! with a different false-positive profile. The bounding below is the second
//! layer: even an unredacted secret cannot make the store unbounded.

use std::sync::OnceLock;

use regex::Regex;

/// The text substituted for any redacted value.
pub const REDACTION: &str = "<redacted>";

/// Default per-stream capture budget, in bytes. Matches agenthicc's
/// `_DEFAULT_MAX_OUTPUT`.
pub const DEFAULT_MAX_OUTPUT: usize = 64 * 1024;

/// Default bound for a single display string (a command, a label).
pub const DEFAULT_TEXT_LIMIT: usize = 240;

/// agenthicc `_SECRET_FLAGS`: the command-line form.
fn secret_flags() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(--?(?:password|passwd|token|secret|api[-_]?key|authorization))\s*(?:=|\s)\s*([^\s]+)",
        )
        .expect("secret-flag regex is a compile-time constant")
    })
}

/// agenthicc `_SECRET_ASSIGNMENT`: the env/`.env` form.
fn secret_assignment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b((?:api[_-]?key|token|secret|password|passwd|authorization))\s*=\s*([^\s;&]+)",
        )
        .expect("secret-assignment regex is a compile-time constant")
    })
}

/// Replace both credential shapes, preserving line structure.
///
/// This is the function every persisted output chunk goes through.
pub fn redact_output(value: &str) -> String {
    let stage = secret_flags().replace_all(value, format!("${{1}}={REDACTION}").as_str());
    secret_assignment()
        .replace_all(&stage, format!("${{1}}={REDACTION}").as_str())
        .into_owned()
}

/// Redact, then collapse whitespace and bound to `limit` characters.
///
/// Used for single-line display strings (a command, a label, a readiness
/// value), never for captured output.
pub fn redact_text(value: &str, limit: usize) -> String {
    let redacted = redact_output(value);
    let collapsed = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let keep = limit.saturating_sub(1);
    let head: String = collapsed.chars().take(keep).collect();
    format!("{head}\u{2026}")
}

/// Redact, then collapse and bound to [`DEFAULT_TEXT_LIMIT`].
pub fn redact_label(value: &str) -> String {
    redact_text(value, DEFAULT_TEXT_LIMIT)
}

/// Append `addition` to `current` under a byte budget, keeping both ends.
///
/// agenthicc's `_bounded_append`. Keeping head *and* tail is deliberate: a
/// build that fails usually names the error at the end and the context at the
/// start, and dropping either end loses the one line the agent needs.
pub fn bounded_append(current: &str, addition: &str, limit: usize) -> (String, bool) {
    if addition.is_empty() {
        return (current.to_string(), false);
    }
    let combined = format!("{current}{addition}");
    if combined.len() <= limit {
        return (combined, false);
    }
    const MARKER: &str = "\n[\u{2026} output truncated \u{2026}]\n";
    // A limit too small to hold the marker must still return *something*
    // bounded, so the floor is 1 byte on each side rather than an underflow.
    let keep = limit.saturating_sub(MARKER.len()).max(1) / 2;
    let head = floor_char_boundary(&combined, keep.min(combined.len()));
    let tail_start = ceil_char_boundary(&combined, combined.len().saturating_sub(keep))
        .max(head.len())
        .min(combined.len());
    let tail = &combined[tail_start..];
    (format!("{head}{MARKER}{tail}"), true)
}

/// Back off to the nearest char boundary at or below `end`.
///
/// Slicing a `&str` at a non-boundary index panics, and a byte budget sliced
/// through the middle of a multi-byte character is exactly what a byte-based
/// limit produces.
fn floor_char_boundary(s: &str, end: usize) -> &str {
    if end >= s.len() {
        return s;
    }
    let mut idx = end;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

/// Advance to the nearest char boundary at or above `start`.
///
/// The tail slice needs the same treatment as the head, for the same reason:
/// `len - keep` lands mid-character exactly when `keep` does. Flooring here
/// instead would duplicate bytes the head already kept.
fn ceil_char_boundary(s: &str, start: usize) -> usize {
    if start >= s.len() {
        return s.len();
    }
    let mut idx = start;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Whether a readiness probe target is the local machine.
///
/// agenthicc's `_is_loopback`: `localhost`, or an address `ipaddress` considers
/// loopback. Note this deliberately excludes `0.0.0.0` — that is a *bind*
/// address meaning "every interface", not a loopback destination, and treating
/// it as safe would make a probe that reaches the whole LAN look local.
pub fn is_loopback(host: &str) -> bool {
    let normalized = host.trim().to_ascii_lowercase();
    if normalized == "localhost" {
        return true;
    }
    // Bracketed IPv6, as it appears in a URL authority.
    let bare = normalized
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(&normalized);
    if bare == "::1" {
        return true;
    }
    match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_secret_is_redacted() {
        let out = redact_output("curl --token hunter2 https://example.invalid");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("--token=<redacted>"), "{out}");
    }

    #[test]
    fn an_equals_form_flag_is_redacted() {
        let out = redact_output("login --password=s3cr3t");
        assert!(!out.contains("s3cr3t"), "{out}");
    }

    /// The `cat .env` case the module doc is written about.
    #[test]
    fn an_env_assignment_is_redacted() {
        let out = redact_output("DATABASE_URL=postgres://x\nAPI_KEY=sk-live-abcdef123\nPORT=5432");
        assert!(!out.contains("sk-live-abcdef123"), "{out}");
        assert!(
            out.contains("PORT=5432"),
            "non-secret lines must survive: {out}"
        );
    }

    #[test]
    fn redaction_preserves_line_structure() {
        let out = redact_output("line one\nTOKEN=abc\nline three");
        assert_eq!(out.lines().count(), 3, "{out}");
    }

    #[test]
    fn a_non_secret_command_is_untouched() {
        let cmd = "cargo test --workspace --all-targets";
        assert_eq!(redact_output(cmd), cmd);
    }

    #[test]
    fn redact_text_collapses_whitespace_and_bounds() {
        let out = redact_text("a   b\n\tc", DEFAULT_TEXT_LIMIT);
        assert_eq!(out, "a b c");
        let long = "x".repeat(DEFAULT_TEXT_LIMIT + 50);
        let bounded = redact_text(&long, DEFAULT_TEXT_LIMIT);
        assert!(bounded.chars().count() <= DEFAULT_TEXT_LIMIT, "{bounded}");
        assert!(bounded.ends_with('\u{2026}'), "{bounded}");
    }

    #[test]
    fn bounded_append_keeps_head_and_tail() {
        let (out, truncated) = bounded_append("", &"a".repeat(100), 60);
        assert!(truncated);
        assert!(out.contains("output truncated"), "{out}");
        assert!(out.len() <= 120, "budget must hold: {}", out.len());
    }

    #[test]
    fn bounded_append_below_budget_is_a_no_op() {
        let (out, truncated) = bounded_append("head", "tail", 1024);
        assert_eq!(out, "headtail");
        assert!(!truncated);
    }

    /// A byte budget must not panic when it lands mid-character.
    #[test]
    fn bounded_append_survives_a_multibyte_split() {
        let addition = "\u{00e9}".repeat(40); // 2 bytes each
        for limit in 1..64 {
            let (out, _) = bounded_append("", &addition, limit);
            assert!(
                out.len() <= limit.max(2) + 32,
                "limit {limit} -> {}",
                out.len()
            );
        }
    }

    #[test]
    fn loopback_recognises_local_targets_only() {
        for host in ["localhost", "127.0.0.1", "127.5.5.5", "::1", "[::1]"] {
            assert!(is_loopback(host), "{host} should be loopback");
        }
        for host in ["0.0.0.0", "10.0.0.5", "example.com", "169.254.169.254", ""] {
            assert!(!is_loopback(host), "{host} should NOT be loopback");
        }
    }
}
