//! Transition hooks: guards that read a call's arguments, and effects that may
//! choose the transition's edge.
//!
//! [`check_guard`] and [`run_effect`] see only the run's key/value state.
//! agenthicc's custom runners do more inside their transition tools: they
//! validate arguments against the run (a criterion list, a baseline id, a
//! re-entry target), keep typed bookkeeping (a component cursor, review
//! attempts) and pick the next state from it. This module is that layer for
//! declarative workflows:
//!
//! * [`check_transition_guard`] — a precondition, given the call's arguments;
//! * [`apply_transition_effect`] — validation and bookkeeping, whose
//!   [`EffectOutcome`] may carry a [`Route`] that replaces the transition's
//!   declared edge.
//!
//! A refused guard or effect is returned to the agent as a tool error and the
//! phase continues, exactly like a failed agenthicc transition tool. Every
//! name not handled here falls through to `check_guard` / `run_effect`, so the
//! original guards and effects behave as before.
//!
//! A routing effect can only pick an existing phase, the end of the run, or an
//! early exit; the TOML still declares every edge the route can take (as a
//! phase `next`, `on_reject` or transition `next`), so the validator's graph
//! checks stay meaningful.

use serde_json::{Map, Value, json};

use super::groups::{RunData, check_guard, run_effect};
use super::tools::SharedControl;
use super::{copy_website, reconstruct_site, site_imitate, ui_ux_pro_max};

/// Everything a hook may read about the call being made.
pub struct HookContext<'a> {
    /// The run's key/value state.
    pub data: &'a RunData,
    /// The phase run's decision slot.
    pub control: &'a SharedControl,
    /// The call's validated arguments.
    pub args: &'a Value,
    /// The phase the call is made in.
    pub phase: &'a str,
    /// The transition tool being called.
    pub tool: &'a str,
}

/// An edge chosen by an effect, replacing the transition's declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Enter this phase.
    Phase(String),
    /// End the run normally.
    End,
    /// End the run early (status `exited`) — agenthicc's terminal `BLOCKED`.
    Exit,
}

/// What an accepted effect reports back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectOutcome {
    /// A note for the agent, prepended to the transition's stop note.
    pub note: String,
    /// The edge to take instead of the declared one.
    pub route: Option<Route>,
}

impl EffectOutcome {
    /// An outcome that only carries a note.
    pub fn note(note: impl Into<String>) -> Self {
        Self {
            note: note.into(),
            route: None,
        }
    }

    /// An outcome that routes to `phase`.
    pub fn to_phase(phase: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            note: note.into(),
            route: Some(Route::Phase(phase.into())),
        }
    }
}

/// Check a transition guard, with the call's arguments available.
pub fn check_transition_guard(guard: &str, ctx: &HookContext<'_>) -> Result<(), String> {
    match guard {
        "mobile_verified" => site_imitate::check_mobile_verified(ctx),
        "implementation_plan_approved" => ui_ux_pro_max::check_implementation_approved(ctx),
        "infra_artifacts_present" => reconstruct_site::check_infra_artifacts(ctx),
        other => check_guard(other, ctx.data, ctx.control),
    }
}

/// Perform a transition effect. `Err` refuses the call.
pub fn apply_transition_effect(
    effect: &str,
    ctx: &HookContext<'_>,
) -> Result<EffectOutcome, String> {
    match effect {
        "copy_website" => copy_website::apply(ctx),
        "site_imitate" => site_imitate::apply(ctx),
        "ui_ux_pro_max" => ui_ux_pro_max::apply(ctx),
        "reconstruct_site" => reconstruct_site::apply(ctx),
        other => run_effect(other, ctx.data, ctx.args).map(EffectOutcome::note),
    }
}

// ── Argument helpers ───────────────────────────────────────────────────────

/// A string argument, trimmed; empty when absent.
pub fn arg_str(args: &Value, key: &str) -> String {
    match args.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// A list argument as trimmed, non-empty strings.
pub fn arg_list(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.trim().to_string(),
                other => other.to_string(),
            })
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => s
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// A list-of-objects argument.
pub fn arg_objects(args: &Value, key: &str) -> Vec<Map<String, Value>> {
    match args.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_object().cloned())
            .collect(),
        _ => Vec::new(),
    }
}

/// A boolean argument, or `default` when absent.
pub fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => true,
            "false" | "no" | "0" => false,
            _ => default,
        },
        _ => default,
    }
}

// ── Run-data helpers ───────────────────────────────────────────────────────

/// Merge string values into the `state` map that prompts read as
/// `{state.<key>}`.
pub fn set_state(data: &RunData, pairs: &[(&str, String)]) {
    data.set_state_values(pairs.iter().map(|(k, v)| (*k, v.clone())));
}

/// A workflow's private record under `key`, as an object.
pub fn record(data: &RunData, key: &str) -> Map<String, Value> {
    match data.get(key) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Replace a workflow's private record.
pub fn save_record(data: &RunData, key: &str, map: Map<String, Value>) {
    data.set(key, Value::Object(map));
}

/// Read a string field of a record.
pub fn field_str(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Read a string-list field of a record.
pub fn field_list(map: &Map<String, Value>, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Read an integer field of a record.
pub fn field_u64(map: &Map<String, Value>, key: &str) -> u64 {
    map.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Set a string-list field of a record.
pub fn put_list(map: &mut Map<String, Value>, key: &str, items: &[String]) {
    map.insert(key.to_string(), json!(items));
}

/// A bounded integer workflow param — agenthicc's `_bounded_int`: a missing or
/// non-integer value yields `default`; others are clamped to `lower..=upper`.
pub fn bounded_param(data: &RunData, key: &str, default: i64, lower: i64, upper: i64) -> i64 {
    match data.param(key) {
        Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap_or(default).clamp(lower, upper),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phased::tools::PhaseControl;
    use std::sync::{Arc, Mutex};
    use xaft_config::workflows::validate::{KNOWN_EFFECTS, KNOWN_GUARDS};

    #[test]
    fn every_known_guard_and_effect_has_an_implementation() {
        let data = RunData::default();
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        let args = json!({});
        let ctx = HookContext {
            data: &data,
            control: &control,
            args: &args,
            phase: "p",
            tool: "t",
        };
        for guard in KNOWN_GUARDS {
            if let Err(e) = check_transition_guard(guard, &ctx) {
                assert!(!e.contains("unknown guard"), "{guard}: {e}");
            }
        }
        for effect in KNOWN_EFFECTS {
            if let Err(e) = apply_transition_effect(effect, &ctx) {
                assert!(!e.contains("unknown effect"), "{effect}: {e}");
            }
        }
        assert!(
            check_transition_guard("moon_is_full", &ctx)
                .unwrap_err()
                .contains("unknown guard")
        );
        assert!(
            apply_transition_effect("teleport", &ctx)
                .unwrap_err()
                .contains("unknown effect")
        );
    }

    #[test]
    fn argument_helpers_accept_the_shapes_models_send() {
        let args = json!({
            "s": "  x  ",
            "l": [" a ", "", "b"],
            "lines": "one\n\n two ",
            "b": "true",
            "n": 3,
            "objs": [{"page": "/"}, "nope"]
        });
        assert_eq!(arg_str(&args, "s"), "x");
        assert_eq!(arg_str(&args, "missing"), "");
        assert_eq!(arg_list(&args, "l"), ["a", "b"]);
        assert_eq!(arg_list(&args, "lines"), ["one", "two"]);
        assert!(arg_bool(&args, "b", false));
        assert!(arg_bool(&args, "missing", true));
        assert_eq!(arg_objects(&args, "objs").len(), 1);
    }

    #[test]
    fn state_values_merge_into_the_prompt_state_map() {
        let data = RunData::default();
        set_state(&data, &[("a", "1".into())]);
        set_state(&data, &[("b", "2".into())]);
        assert_eq!(data.get("state"), Some(json!({"a": "1", "b": "2"})));
    }
}
