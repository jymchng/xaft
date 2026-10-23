//! `/tools` handler — list every tool available to the current session.
//!
//! Mirrors agenthicc's `_cmd_tools`, which lists the session's tools with a
//! source column: `builtin`, or `project` / `user` for a persistent custom
//! tool (`.xaft/tools/*.toml`, `~/.config/xaft/tools/*.toml`). The custom tool
//! directories are re-read on every call — agenthicc's `/tools reload` — and a
//! tool that is not loaded (untrusted, invalid, shadowing) is reported with
//! the reason.
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
        let custom = xaft_tools::custom_tools::discover_default(&ctx.working_dir);
        let reg = match ToolRegistryBuilder::new(&ctx.working_dir)
            .with_shell()
            .with_browser_settings(&ctx.config.tools, ctx.session_id.as_deref())
            .with_custom_tools(custom.specs())
            .build_coder()
        {
            Ok(r) => r,
            Err(e) => {
                return CommandResult::Error(format!("Could not enumerate tools: {e}"));
            }
        };

        let names: Vec<(String, &'static str, String)> = reg
            .all()
            .iter()
            .map(|t| {
                let source = custom
                    .tools
                    .iter()
                    .find(|c| c.spec.name == t.name())
                    .map(|c| c.source.as_str())
                    .unwrap_or("builtin");
                (t.name().to_string(), source, first_line(t.description()))
            })
            .collect();

        if names.is_empty() {
            return CommandResult::Lines(vec!["(no tools found)".into()]);
        }

        let mut lines = vec![format!("Available tools ({}):", names.len())];
        lines.extend(
            names
                .iter()
                .map(|(name, source, desc)| format!("  {name}  [{source}]  {desc}")),
        );
        lines.extend(custom.warnings.iter().map(|w| format!("  ! {w}")));
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
            services: Default::default(),
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
    fn every_entry_is_labelled_with_its_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = lines(ToolsHandler.execute(ctx(tmp.path())));
        let rows: Vec<_> = out.iter().filter(|l| !l.starts_with("  ! ")).collect();
        let entries: Vec<_> = rows
            .iter()
            .filter(|l| l.contains("[builtin]") || l.contains("[user]") || l.contains("[project]"))
            .collect();
        // 1 header + 1 row per tool; every tool row carries a source.
        assert_eq!(
            entries.len(),
            rows.len() - 1,
            "every tool row needs a source label: {out:?}"
        );
        assert!(out.iter().any(|l| l.contains("read_file  [builtin]")));
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

    /// An untrusted project custom tool is not listed as a tool but reported,
    /// with what to do about it.
    #[test]
    fn an_untrusted_project_tool_is_reported_not_listed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tools = tmp.path().join(".xaft/tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(
            tools.join("xaft_tui_test_tool.toml"),
            "description = \"d\"\ncommand = \"date\"\n",
        )
        .unwrap();
        let out = lines(ToolsHandler.execute(ctx(tmp.path())));
        assert!(
            !out.iter().any(|l| l.starts_with("  xaft_tui_test_tool ")),
            "{out:?}"
        );
        assert!(
            out.iter().any(|l| l.starts_with("  ! ")
                && l.contains("xaft_tui_test_tool")
                && l.contains("xaft trust add")),
            "{out:?}"
        );
    }

    #[test]
    fn first_line_collapses_multiline_descriptions() {
        assert_eq!(first_line("one\ntwo"), "one");
        assert_eq!(first_line("   "), "—");
        assert_eq!(first_line(""), "—");
    }
}
