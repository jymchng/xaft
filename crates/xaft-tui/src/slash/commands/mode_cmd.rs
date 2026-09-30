//! /mode command — show or switch the active operational mode.

use std::sync::Arc;

use xaft_config::XaftConfig;

use crate::mode::builtins::builtin_modes;
use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct ModeHandler {
    pub config: Arc<XaftConfig>,
}

impl ModeHandler {
    pub fn new(config: Arc<XaftConfig>) -> Self {
        Self { config }
    }
}

impl SlashHandler for ModeHandler {
    fn description(&self) -> &'static str {
        "Show or switch the active mode  (/mode [safe|plan|auto|ask|review])"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[mode-name]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        use crate::mode::manager::{CYCLE_NAMES, ModeManager, canonical_mode_name};
        let args = ctx.args.trim().to_lowercase();
        if args.is_empty() {
            // List the interactive cycle (agenthicc parity) then the full set.
            let mut lines = vec![
                "Available modes (Shift+Tab to cycle):".to_string(),
                format!(
                    "  cycle: {}",
                    CYCLE_NAMES
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                        .join(" → ")
                ),
                String::new(),
            ];
            for m in builtin_modes() {
                lines.push(format!(
                    "  {} {} — {}",
                    m.ansi_badge(),
                    m.name,
                    m.description
                ));
            }
            lines.push(String::new());
            lines.push("  Aliases: yolo≡auto, ask/guard≡safe, review≡plan".to_string());
            lines.push("  /mode <name>   or   Shift+Tab to switch".to_string());
            CommandResult::Lines(lines)
        } else {
            let canonical = canonical_mode_name(&args);
            // agenthicc has NO debug mode, so `/mode debug` answers "Unknown
            // mode" there. xaft DOES ship a debug mode (a per-response
            // token/cost footer), so the second clause below deliberately lets
            // it through: it is an xaft extension, not agenthicc parity.
            //
            // This guard previously carried the comment "Debug is not an alias
            // and is rejected via /mode" while the code beneath it accepted
            // debug on every call -- `builtin_modes()` contains a mode named
            // `debug`, so `!any(...)` evaluates to false and the rejection
            // never fires. The comment was the false part, not the code.
            // Closing the divergence means removing the debug mode, which is a
            // user-facing feature removal; until that is authorized the
            // divergence is recorded in docs/parity-matrix.md row 7 as a known
            // divergence rather than claimed as parity.
            if !ModeManager::is_cyclable(&args)
                && !builtin_modes().iter().any(|m| m.name == canonical)
            {
                return CommandResult::Lines(vec![format!(
                    "  Unknown mode '{args}'. Available: safe, plan, auto (aliases: yolo, ask, guard, review)."
                )]);
            }
            CommandResult::Lines(vec![format!(
                "  Switching to '{}' mode — use Shift+Tab to cycle interactively",
                canonical
            )])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::{AgentStatsMap, CommandContext, SlashCommand};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicBool;

    fn make_ctx(args: &str) -> CommandContext {
        CommandContext {
            args: args.to_string(),
            command: SlashCommand::Mode {
                name: if args.is_empty() {
                    None
                } else {
                    Some(args.to_string())
                },
            },
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
            working_dir: PathBuf::from("."),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
        }
    }

    #[test]
    fn mode_no_args_lists_modes() {
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        let result = h.execute(make_ctx(""));
        let CommandResult::Lines(lines) = result else {
            panic!("expected Lines")
        };
        let text = lines.join("\n");
        assert!(text.contains("auto"), "should list auto mode");
        assert!(text.contains("plan"), "should list plan mode");
        assert!(text.contains("safe"), "should list safe mode");
    }

    /// agenthicc's four aliases, with the canonical mode each resolves to.
    const AGENTHICC_ALIASES: &[(&str, &str)] = &[
        ("yolo", "auto"),
        ("auto", "auto"),
        ("guard", "safe"),
        ("ask", "safe"),
        ("review", "plan"),
    ];

    #[test]
    fn mode_accepts_every_agenthicc_alias() {
        // docs/parity-matrix.md row 5 recorded `guard` as "**not accepted**".
        // That was false: `canonical_mode_name` has mapped guard -> safe for a
        // while, and this is the assertion that keeps it from regressing at the
        // layer a user actually types at.
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        for (alias, expected) in AGENTHICC_ALIASES {
            let CommandResult::Lines(lines) = h.execute(make_ctx(alias)) else {
                panic!("expected Lines for {alias}")
            };
            let text = lines.join("\n");
            assert!(
                !text.contains("Unknown mode"),
                "`/mode {alias}` must be accepted, got: {text}"
            );
            assert!(
                text.contains(&format!("'{expected}' mode")),
                "`/mode {alias}` must resolve to {expected}, got: {text}"
            );
        }
    }

    #[test]
    fn mode_rejects_a_genuinely_unknown_name() {
        // The counterpart to the test above: acceptance must not have been
        // loosened into "everything is accepted".
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        let CommandResult::Lines(lines) = h.execute(make_ctx("not_a_mode")) else {
            panic!("expected Lines")
        };
        assert!(lines.join("\n").contains("Unknown mode 'not_a_mode'"));
    }

    #[test]
    fn mode_accepts_debug_because_xaft_ships_a_debug_mode() {
        // agenthicc has no debug mode and would answer "Unknown mode" here.
        // xaft deliberately keeps one, so this pins the CURRENT behaviour
        // rather than claiming parity it does not have. See the comment above
        // `ModeManager::is_cyclable` and parity-matrix row 7: closing that row
        // means deleting the debug mode, which is a feature removal awaiting
        // authorization. If this test ever starts failing because debug was
        // removed, row 7 is the thing to update -- not the assertion.
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        let CommandResult::Lines(lines) = h.execute(make_ctx("debug")) else {
            panic!("expected Lines")
        };
        let text = lines.join("\n");
        assert!(
            !text.contains("Unknown mode"),
            "xaft currently accepts /mode debug; got: {text}"
        );
    }

    #[test]
    fn mode_with_name_shows_switch_message() {
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        let result = h.execute(make_ctx("plan"));
        let CommandResult::Lines(lines) = result else {
            panic!("expected Lines")
        };
        assert!(lines.iter().any(|l| l.contains("plan")));
    }
}
