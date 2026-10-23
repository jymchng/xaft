//! `/tools` handler — list every tool available to the current session.
//!
//! Mirrors agenthicc's `_cmd_tools`, which lists the session's tools with a
//! source column. xaft has no plugin-provided tools yet, so every entry is
//! `builtin`; the column is kept so the shape matches and so a future plugin
//! source does not silently change the format.
//!
//! The browser family is listed the way a run registers it: from
//! `tools.browser_backend` and that section's `enabled`. Building the session
//! manager here launches nothing — a browser starts only when a tool needs it.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};
use xaft_tools::ToolRegistryBuilder;

pub struct ToolsHandler;

impl SlashHandler for ToolsHandler {
    fn description(&self) -> &'static str {
        "List all tools registered for this session"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let reg = match ToolRegistryBuilder::new(&ctx.working_dir)
            .with_shell()
            .with_browser_settings(&ctx.config.tools, ctx.session_id.as_deref())
            .build_coder()
        {
            Ok(r) => r,
            Err(e) => {
                return CommandResult::Error(format!("Could not enumerate tools: {e}"));
            }
        };

        let names: Vec<(String, String)> = reg
            .all()
            .iter()
            .map(|t| (t.name().to_string(), first_line(t.description())))
            .collect();

        if names.is_empty() {
            return CommandResult::Lines(vec!["(no tools found)".into()]);
        }

        let mut lines = vec![format!("Available tools ({}):", names.len())];
        lines.extend(
            names
                .iter()
                .map(|(name, desc)| format!("  {name}  [builtin]  {desc}")),
        );
        CommandResult::Lines(lines)
    }
}

/// The tool description collapsed to its first line.
///
/// Tool doc comments are multi-line; the listing is a one-line-per-tool table,
/// and agenthicc's `_cmd_tools` takes `description_lines[0]` for the same reason.
fn first_line(desc: &str) -> String {
    let line = desc.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        "—".to_string()
    } else {
        line.to_string()
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

    fn ctx(dir: &std::path::Path) -> CommandContext {
        CommandContext {
            args: String::new(),
            command: SlashCommand::Tools,
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
        }
    }

    fn lines(out: CommandResult) -> Vec<String> {
        match out {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn lists_the_tool_names_the_registry_builds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = lines(ToolsHandler.execute(ctx(tmp.path())));
        for expected in ["read_file", "write_file", "edit_file", "bash_exec"] {
            assert!(
                out.iter().any(|l| l.contains(expected)),
                "missing tool {expected} in:\n{}",
                out.join("\n")
            );
        }
    }

    #[test]
    fn every_entry_is_labelled_builtin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = lines(ToolsHandler.execute(ctx(tmp.path())));
        let entries: Vec<_> = out.iter().filter(|l| l.contains("[builtin]")).collect();
        // out() = 1 header + 1 row per tool; every tool row carries a source.
        assert_eq!(
            entries.len(),
            out.len() - 1,
            "every tool row needs a source label: {out:?}"
        );
        assert!(!entries.is_empty(), "expected at least one tool");
        assert!(
            out[0].contains("Available tools"),
            "missing header: {out:?}"
        );
    }

    /// The browser family follows `tools.browser_backend`, as a run does:
    /// CloakBrowser by default (agenthicc's default), Playwright when
    /// selected, none for `none`.
    #[test]
    fn lists_the_browser_family_the_config_selects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = lines(ToolsHandler.execute(ctx(tmp.path())));
        assert!(
            out.iter().any(|l| l.contains("cloakbrowser_open")),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|l| l.contains("playwright_open")),
            "{out:?}"
        );

        let mut config = XaftConfig::default();
        config.tools.browser_backend = "playwright".into();
        let mut playwright = ctx(tmp.path());
        playwright.config = Arc::new(config.clone());
        let out = lines(ToolsHandler.execute(playwright));
        assert!(
            out.iter().any(|l| l.contains("playwright_snapshot")),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|l| l.contains("cloakbrowser_open")),
            "{out:?}"
        );

        config.tools.browser_backend = "none".into();
        let mut none = ctx(tmp.path());
        none.config = Arc::new(config);
        let out = lines(ToolsHandler.execute(none));
        assert!(!out.iter().any(|l| l.contains("_snapshot")), "{out:?}");
    }

    #[test]
    fn first_line_collapses_multiline_descriptions() {
        assert_eq!(first_line("one\ntwo"), "one");
        assert_eq!(first_line("   "), "—");
        assert_eq!(first_line(""), "—");
    }
}
