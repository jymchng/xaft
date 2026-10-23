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
        "Show or switch the active mode  (/mode [safe|plan|auto], aliases yolo/ask/guard/review)"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[mode-name]")
    }

    /// The listing, or the verdict on a requested name.
    ///
    /// `AppState` performs the switch itself (it owns the `ModeManager`)
    /// before this runs, with the same rule this applies: a name is accepted
    /// when it resolves — through agenthicc's alias table — to a selectable
    /// mode. `debug` resolves to nothing (agenthicc has no Debug identity) and
    /// the internal `replay` mode is not selectable, so both are refused with
    /// agenthicc's `Unknown mode … Choose one of: …` line.
    fn execute(&self, ctx: CommandContext) -> CommandResult {
        use crate::mode::manager::{CYCLE_NAMES, canonical_mode_name};
        let args = ctx.args.trim().to_lowercase();
        let modes = builtin_modes();
        // What `/mode` lists and accepts by name: selectable, and not shadowed
        // by the alias table (`ask`/`review` resolve to safe/plan).
        let listed: Vec<_> = modes
            .iter()
            .filter(|m| m.selectable && canonical_mode_name(&m.name) == m.name)
            .collect();
        if args.is_empty() {
            // List the interactive cycle (agenthicc parity) then each mode.
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
            for m in &listed {
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
            return CommandResult::Lines(lines);
        }
        let canonical = canonical_mode_name(&args);
        if !modes.iter().any(|m| m.name == canonical && m.selectable) {
            let choices = listed
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return CommandResult::Lines(vec![format!(
                "  Unknown mode '{args}'. Choose one of: {choices}."
            )]);
        }
        CommandResult::Lines(vec![format!(
            "  Switched to '{canonical}' mode — Shift+Tab cycles safe → plan → auto"
        )])
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
            services: Default::default(),
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

    /// Row 8: agenthicc registers no Debug identity, so `/mode debug` is an
    /// unknown mode — and the internal `replay` mode is not selectable either.
    #[test]
    fn mode_rejects_debug_and_the_internal_replay_mode() {
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        for name in ["debug", "replay"] {
            let CommandResult::Lines(lines) = h.execute(make_ctx(name)) else {
                panic!("expected Lines")
            };
            assert_eq!(
                lines,
                vec![format!(
                    "  Unknown mode '{name}'. Choose one of: auto, plan, safe."
                )]
            );
        }
    }

    #[test]
    fn the_listing_omits_internal_and_alias_shadowed_modes() {
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        let CommandResult::Lines(lines) = h.execute(make_ctx("")) else {
            panic!("expected Lines")
        };
        let mode_rows: Vec<&String> = lines.iter().filter(|l| l.contains(" — ")).collect();
        assert_eq!(mode_rows.len(), 3, "{lines:?}");
        let text = lines.join("\n");
        assert!(!text.contains("replay"), "{text}");
        assert!(!text.contains("debug"), "{text}");
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
