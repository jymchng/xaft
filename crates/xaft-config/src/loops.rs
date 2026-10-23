//! `[loops]` — settings for the session-scoped `/loop` scheduler.
//!
//! The xaft counterpart of agenthicc's `LoopSettings` (`config.py`), key for
//! key and default for default (parity rows 143-152; the table itself is row
//! 185, `XaftConfig.loops`). The scheduler that reads these lives in
//! `xaft-tui` (`loops::LoopManager`), because a loop is local, explicit user
//! automation owned by one interactive session — agenthicc's headless runner
//! refuses `/loop` for the same reason.
//!
//! Limits are intentionally finite. A loop is not an unbounded background
//! daemon: it expires after `max_age_s`, stops after
//! `max_consecutive_failures`, and every iteration still goes through the
//! session's normal approval and security path.

use serde::{Deserialize, Serialize};

/// `[loops]` settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoopsConfig {
    /// Allow `/loop` to create, resume and run loops. With `false`, existing
    /// loops can still be inspected, paused and stopped.
    pub enabled: bool,
    /// Interval used when `/loop` is given no explicit interval, seconds.
    pub default_interval_s: u64,
    /// Smallest interval `/loop` accepts, seconds.
    pub min_interval_s: u64,
    /// Largest interval `/loop` accepts, seconds.
    pub max_interval_s: u64,
    /// A loop expires this many seconds after it was created.
    pub max_age_s: u64,
    /// Largest prompt or command payload, in UTF-8 bytes.
    pub max_prompt_bytes: usize,
    /// Consecutive failed iterations after which the loop stops as `failed`.
    pub max_consecutive_failures: u32,
    /// While an iteration is due but the session is busy, how often to
    /// re-check for idle, seconds.
    pub busy_poll_s: f64,
    /// Persist the loop with its session so an explicit resume rehydrates it.
    pub persist: bool,
    /// Accept a registered slash command (e.g. `/status`) as the payload.
    pub allow_slash_commands: bool,
}

impl Default for LoopsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_interval_s: 600,
            min_interval_s: 60,
            max_interval_s: 86_400,
            max_age_s: 259_200,
            max_prompt_bytes: 16_384,
            max_consecutive_failures: 3,
            busy_poll_s: 1.0,
            persist: true,
            allow_slash_commands: true,
        }
    }
}

impl LoopsConfig {
    /// Validate scheduler bounds before a session can use them.
    ///
    /// agenthicc's `LoopSettings.validate`, message for message (each names
    /// the key). Returns the first problem found.
    pub fn validate(&self) -> Result<(), String> {
        if self.default_interval_s == 0 {
            return Err("loops.default_interval_s must be positive".into());
        }
        if self.min_interval_s == 0 {
            return Err("loops.min_interval_s must be positive".into());
        }
        if self.max_interval_s < self.min_interval_s {
            return Err("loops.max_interval_s must be >= loops.min_interval_s".into());
        }
        if !(self.min_interval_s..=self.max_interval_s).contains(&self.default_interval_s) {
            return Err(
                "loops.default_interval_s must be within the configured interval bounds".into(),
            );
        }
        if self.max_age_s == 0 {
            return Err("loops.max_age_s must be positive".into());
        }
        if self.max_prompt_bytes == 0 {
            return Err("loops.max_prompt_bytes must be positive".into());
        }
        if self.max_consecutive_failures == 0 {
            return Err("loops.max_consecutive_failures must be positive".into());
        }
        if !(self.busy_poll_s.is_finite() && self.busy_poll_s > 0.0) {
            return Err("loops.busy_poll_s must be positive".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_agenthicc_loop_settings() {
        let c = LoopsConfig::default();
        assert!(c.enabled);
        assert_eq!(c.default_interval_s, 600);
        assert_eq!(c.min_interval_s, 60);
        assert_eq!(c.max_interval_s, 86_400);
        assert_eq!(c.max_age_s, 259_200);
        assert_eq!(c.max_prompt_bytes, 16_384);
        assert_eq!(c.max_consecutive_failures, 3);
        assert_eq!(c.busy_poll_s, 1.0);
        assert!(c.persist);
        assert!(c.allow_slash_commands);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validation_names_the_offending_key() {
        let bad_bounds = LoopsConfig {
            min_interval_s: 100,
            max_interval_s: 10,
            ..Default::default()
        };
        assert!(
            bad_bounds
                .validate()
                .unwrap_err()
                .contains("max_interval_s")
        );

        let default_outside = LoopsConfig {
            default_interval_s: 1,
            min_interval_s: 60,
            ..Default::default()
        };
        assert!(
            default_outside
                .validate()
                .unwrap_err()
                .contains("default_interval_s")
        );

        let zero_failures = LoopsConfig {
            max_consecutive_failures: 0,
            ..Default::default()
        };
        assert!(
            zero_failures
                .validate()
                .unwrap_err()
                .contains("max_consecutive_failures")
        );

        let bad_poll = LoopsConfig {
            busy_poll_s: 0.0,
            ..Default::default()
        };
        assert!(bad_poll.validate().unwrap_err().contains("busy_poll_s"));
    }

    #[test]
    fn a_loops_table_round_trips_through_toml() {
        let parsed: LoopsConfig = toml::from_str(
            "default_interval_s = 120\nmin_interval_s = 30\nmax_interval_s = 900\n\
             max_age_s = 1800\nallow_slash_commands = false\n",
        )
        .unwrap();
        assert_eq!(parsed.default_interval_s, 120);
        assert_eq!(parsed.min_interval_s, 30);
        assert_eq!(parsed.max_interval_s, 900);
        assert_eq!(parsed.max_age_s, 1800);
        assert!(!parsed.allow_slash_commands);
        // Unspecified keys keep their defaults.
        assert_eq!(parsed.max_prompt_bytes, 16_384);
        assert!(parsed.persist);
    }
}
