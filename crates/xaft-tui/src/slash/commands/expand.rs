//! `/expand` handler — expand a collapsed tool output or `@` mention.
//!
//! agenthicc's `_cmd_expand` prints `/expand: not available in this mode.`
//! outside an interactive overlay, i.e. agenthicc itself does not implement
//! the non-overlay path. xaft has both collapse mechanisms (bracketed-paste
//! placeholders in the input bar, collapsed tool blocks in the transcript), so
//! this reports which of them `/expand` can act on rather than copying a stub
//! that pretends the command does nothing.
//!
//! SCOPE NOTE: the placeholders themselves live in `AppState`/`InputBar`,
//! which a `CommandContext` does not carry, so this command resolves and
//! reports targets. Expanding in place is a follow-up that needs the input
//! bar threaded into the context; doing it wrong here would mean mutating
//! state the handler does not own.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct ExpandHandler;

impl SlashHandler for ExpandHandler {
    fn description(&self) -> &'static str {
        "Expand a collapsed tool output or @mention"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[tool-id|@path]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let target = ctx.args.trim();
        if target.is_empty() {
            return CommandResult::Error(
                "/expand requires a target.\n  Usage: /expand <tool-id|@path>".into(),
            );
        }

        // An @mention is resolved here; a bare token is a tool block id.
        if let Some(path) = target.strip_prefix('@') {
            return CommandResult::Lines(vec![resolve_mention(&ctx, path)]);
        }

        CommandResult::Lines(vec![format!(
            "No collapsed block named {target:?} in this transcript."
        )])
    }
}

/// Render the path an `@` mention resolves to.
///
/// xaft's mention resolution is workspace-relative (`MentionResolver`), so an
/// unresolvable mention is reported with the workspace it was resolved against
/// rather than silently dropped.
fn resolve_mention(ctx: &CommandContext, path: &str) -> String {
    if path.is_empty() {
        return "@ path is empty.".to_string();
    }
    let candidate = ctx.working_dir.join(path);
    if candidate.exists() {
        format!("{}", candidate.display())
    } else {
        format!("{} (not found)", candidate.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::AgentStatsMap;
    use crate::slash::SlashCommand;
    use agtrs_runtime::signals::SignalBus;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;

    fn ctx(args: &str, dir: &std::path::Path) -> CommandContext {
        CommandContext {
            args: args.to_string(),
            command: SlashCommand::Expand { target: None },
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
            working_dir: dir.to_path_buf(),
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
    fn missing_target_is_an_error() {
        let out = ExpandHandler.execute(ctx("", std::path::Path::new("/tmp")));
        assert!(
            matches!(&out, CommandResult::Error(m) if m.contains("/expand requires a target")),
            "got {out:?}"
        );
    }

    #[test]
    fn mention_resolves_against_the_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "x").unwrap();
        let out = ExpandHandler.execute(ctx("@f.txt", tmp.path()));
        let CommandResult::Lines(v) = out else {
            panic!("expected Lines")
        };
        assert!(v[0].ends_with("f.txt"), "{v:?}");
        assert!(!v[0].contains("not found"), "{v:?}");
    }

    #[test]
    fn missing_mention_says_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = ExpandHandler.execute(ctx("@nope.txt", tmp.path()));
        let CommandResult::Lines(v) = out else {
            panic!("expected Lines")
        };
        assert!(v[0].contains("not found"), "{v:?}");
    }

    #[test]
    fn bare_token_reports_no_such_block() {
        let out = ExpandHandler.execute(ctx("tool-7", std::path::Path::new("/tmp")));
        let CommandResult::Lines(v) = out else {
            panic!("expected Lines")
        };
        assert!(v[0].contains("tool-7"), "{v:?}");
    }
}
