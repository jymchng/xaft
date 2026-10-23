//! Slash command parser and COMMAND_TABLE.

use super::SlashCommand;

// ── CommandMeta ───────────────────────────────────────────────────────────────

/// Metadata for one entry in `COMMAND_TABLE`.
pub struct CommandMeta {
    /// Canonical trigger (e.g. "clear").
    pub canonical: &'static str,
    /// Additional aliases (e.g. &["cls"]).
    pub aliases: &'static [&'static str],
    /// Parse function: takes the args string, returns the SlashCommand or Err.
    pub parse_fn: fn(&str) -> Result<SlashCommand, String>,
    /// Human-readable description shown in /help output.
    pub description: &'static str,
    /// Optional argument hint, e.g. "[session-id]".
    pub args_hint: Option<&'static str>,
}

// ── Arg parsers ───────────────────────────────────────────────────────────────

fn parse_args_no_args<F: Fn() -> SlashCommand>(
    ctor: F,
) -> impl Fn(&str) -> Result<SlashCommand, String> {
    move |_args: &str| Ok(ctor())
}

fn parse_help(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Help)
}

fn parse_clear(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Clear)
}

fn parse_compact(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Compact)
}

fn parse_config(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Config)
}

fn parse_cost(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Cost)
}

fn parse_init(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Init)
}

fn parse_agents(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Agents)
}

fn parse_mcp(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Mcp)
}

fn parse_resume(args: &str) -> Result<SlashCommand, String> {
    let id = args.trim();
    Ok(SlashCommand::Resume {
        id: if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        },
    })
}

fn parse_rewind(args: &str) -> Result<SlashCommand, String> {
    let s = args.trim();
    if s.is_empty() {
        return Ok(SlashCommand::Rewind { msg_index: None });
    }
    s.parse::<usize>()
        .map(|n| SlashCommand::Rewind { msg_index: Some(n) })
        .map_err(|_| format!("/rewind expects a message number, got {:?}", s))
}

fn parse_permissions(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Permissions)
}

fn parse_model(args: &str) -> Result<SlashCommand, String> {
    let name = args.trim();
    if name.is_empty() {
        return Err("/model requires a model name.\n  Usage: /model <name>".into());
    }
    Ok(SlashCommand::Model {
        name: name.to_string(),
    })
}

fn parse_vim(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Vim)
}

fn parse_emacs(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Emacs)
}

fn parse_theme(args: &str) -> Result<SlashCommand, String> {
    let name = args.trim();
    Ok(SlashCommand::Theme {
        name: if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        },
    })
}

fn parse_login(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Login)
}

fn parse_logout(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Logout)
}

fn parse_doctor(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Doctor)
}

fn parse_memory(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Memory)
}

fn parse_diff(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Diff)
}

fn parse_commit(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Commit)
}

fn parse_pr(args: &str) -> Result<SlashCommand, String> {
    let title = args.trim();
    Ok(SlashCommand::Pr {
        title: if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        },
    })
}

fn parse_quit(args: &str) -> Result<SlashCommand, String> {
    let _ = args;
    Ok(SlashCommand::Quit)
}

fn parse_mode(args: &str) -> Result<SlashCommand, String> {
    let name = args.trim().to_string();
    Ok(SlashCommand::Mode {
        name: if name.is_empty() { None } else { Some(name) },
    })
}

fn parse_cancel(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Cancel)
}

fn parse_commands(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Commands)
}

fn parse_tools(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Tools)
}

fn parse_status(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Status)
}

/// `/skills` takes no arguments.
///
/// agenthicc accepts exactly one (`reload`) and reports `Usage: /skills
/// [reload]` for anything else. xaft declines the reload branch — skills are
/// loaded once at startup and nothing re-scans them — so accepting `reload`
/// would be the placeholder shape this repo treats as a defect: a command that
/// looks like it did something and did not. Any argument is refused with the
/// reason, and `/skills reload` is refused by name.
fn parse_skills(args: &str) -> Result<SlashCommand, String> {
    let arg = args.trim();
    if arg.is_empty() {
        return Ok(SlashCommand::Skills);
    }
    if arg.eq_ignore_ascii_case("reload") {
        return Err(
            "/skills reload is not supported: xaft loads skills once at startup and \
             has no reload hook.\n  Usage: /skills"
                .into(),
        );
    }
    Err(format!(
        "/skills takes no arguments, got {arg:?}.\n  Usage: /skills"
    ))
}

fn parse_expand(args: &str) -> Result<SlashCommand, String> {
    let target = args.trim().to_string();
    Ok(SlashCommand::Expand {
        target: if target.is_empty() {
            None
        } else {
            Some(target)
        },
    })
}

fn parse_history(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::History)
}

fn parse_models(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Models)
}

fn parse_workflows(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Workflows)
}

fn parse_workflow(args: &str) -> Result<SlashCommand, String> {
    let name = args.trim().to_string();
    Ok(SlashCommand::Workflow {
        name: if name.is_empty() { None } else { Some(name) },
    })
}

fn parse_replay(args: &str) -> Result<SlashCommand, String> {
    let id = args.trim();
    Ok(SlashCommand::Replay {
        id: if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        },
    })
}

/// `/loop [interval] <prompt> | status | pause | resume | stop` — the handler
/// reads the raw args (the manager owns the grammar, as agenthicc's does).
fn parse_loop(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Loop)
}

/// `/loops` — any argument is answered with its usage line by the handler.
fn parse_loops(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Loops)
}

/// `/startup` takes no arguments; any are ignored, as agenthicc's handler
/// ignores them.
fn parse_startup(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Startup)
}

/// `/workers [--json]` — the handler reads the raw args.
fn parse_workers(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Workers)
}

/// `/ps [terminal-id] [--json]` — the handler reads the raw args.
fn parse_ps(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Ps)
}

/// `/stop [terminal-id|all] [--force] [--confirm]` — the handler reads the raw
/// args.
fn parse_stop(_args: &str) -> Result<SlashCommand, String> {
    Ok(SlashCommand::Stop)
}

// ── COMMAND_TABLE ─────────────────────────────────────────────────────────────

/// Static command table, sorted by trigger key. Aliases are separate entries.
///
/// Each entry: (trigger, CommandMeta)
pub static COMMAND_TABLE: &[(&str, CommandMeta)] = &[
    (
        "agents",
        CommandMeta {
            canonical: "agents",
            aliases: &[],
            parse_fn: parse_agents,
            description: "List loaded agents and their tool counts",
            args_hint: None,
        },
    ),
    (
        "cancel",
        CommandMeta {
            canonical: "cancel",
            aliases: &["interrupt"],
            parse_fn: parse_cancel,
            description: "Cancel the currently running intent",
            args_hint: None,
        },
    ),
    (
        "clear",
        CommandMeta {
            canonical: "clear",
            aliases: &["cls"],
            parse_fn: parse_clear,
            description: "Reset the current session transcript",
            args_hint: None,
        },
    ),
    (
        "cls",
        CommandMeta {
            canonical: "clear",
            aliases: &["cls"],
            parse_fn: parse_clear,
            description: "Reset the current session transcript (alias: /clear)",
            args_hint: None,
        },
    ),
    (
        "commands",
        CommandMeta {
            canonical: "commands",
            aliases: &[],
            parse_fn: parse_commands,
            description: "List all registered commands with their description",
            args_hint: None,
        },
    ),
    (
        "commit",
        CommandMeta {
            canonical: "commit",
            aliases: &[],
            parse_fn: parse_commit,
            description: "Commit session changes to git",
            args_hint: Some("[message]"),
        },
    ),
    (
        "compact",
        CommandMeta {
            canonical: "compact",
            aliases: &["ctx"],
            parse_fn: parse_compact,
            description: "Manually trigger context compaction",
            args_hint: None,
        },
    ),
    (
        "config",
        CommandMeta {
            canonical: "config",
            aliases: &[],
            parse_fn: parse_config,
            description: "Show the resolved configuration",
            args_hint: Some("[key.path]"),
        },
    ),
    (
        "cost",
        CommandMeta {
            canonical: "cost",
            aliases: &["tokens", "usage"],
            parse_fn: parse_cost,
            description: "Show per-agent token and cost table",
            args_hint: None,
        },
    ),
    (
        "ctx",
        CommandMeta {
            canonical: "compact",
            aliases: &["ctx"],
            parse_fn: parse_compact,
            description: "Manually trigger context compaction (alias: /compact)",
            args_hint: None,
        },
    ),
    (
        "diff",
        CommandMeta {
            canonical: "diff",
            aliases: &[],
            parse_fn: parse_diff,
            description: "Show working-tree diff against session base",
            args_hint: Some("[path]"),
        },
    ),
    (
        "doctor",
        CommandMeta {
            canonical: "doctor",
            aliases: &[],
            parse_fn: parse_doctor,
            description: "Run configuration and connectivity diagnostics",
            args_hint: None,
        },
    ),
    (
        "emacs",
        CommandMeta {
            canonical: "emacs",
            aliases: &[],
            parse_fn: parse_emacs,
            description: "Switch input bar to Emacs keybindings",
            args_hint: None,
        },
    ),
    (
        "exit",
        CommandMeta {
            canonical: "quit",
            aliases: &["exit", "q"],
            parse_fn: parse_quit,
            description: "Exit xaft",
            args_hint: None,
        },
    ),
    (
        "expand",
        CommandMeta {
            canonical: "expand",
            aliases: &[],
            parse_fn: parse_expand,
            description: "Expand a collapsed tool output or @mention",
            args_hint: Some("[tool-id|@path]"),
        },
    ),
    (
        "help",
        CommandMeta {
            canonical: "help",
            aliases: &[],
            parse_fn: parse_help,
            description: "Show this help (or /help <command> for details)",
            args_hint: Some("[command]"),
        },
    ),
    (
        "history",
        CommandMeta {
            canonical: "history",
            aliases: &[],
            parse_fn: parse_history,
            description: "Browse the input history",
            args_hint: Some("[query]"),
        },
    ),
    (
        "init",
        CommandMeta {
            canonical: "init",
            aliases: &[],
            parse_fn: parse_init,
            description: "Generate AGENTS.md project memory file",
            args_hint: None,
        },
    ),
    (
        "interrupt",
        CommandMeta {
            canonical: "cancel",
            aliases: &["interrupt"],
            parse_fn: parse_cancel,
            description: "Cancel the currently running intent (alias: /cancel)",
            args_hint: None,
        },
    ),
    (
        "login",
        CommandMeta {
            canonical: "login",
            aliases: &[],
            parse_fn: parse_login,
            description: "Sign in to the configured account (OAuth 2.0 + PKCE in your browser)",
            args_hint: None,
        },
    ),
    (
        "logout",
        CommandMeta {
            canonical: "logout",
            aliases: &[],
            parse_fn: parse_logout,
            description: "Sign out: revoke the stored tokens and delete them",
            args_hint: None,
        },
    ),
    (
        "loop",
        CommandMeta {
            canonical: "loop",
            aliases: &[],
            parse_fn: parse_loop,
            description: "Schedule a recurring prompt or registered slash command",
            args_hint: Some("[interval] <prompt> | status | pause | resume | stop"),
        },
    ),
    (
        "loops",
        CommandMeta {
            canonical: "loops",
            aliases: &[],
            parse_fn: parse_loops,
            description: "Manage all scheduled loop jobs",
            args_hint: None,
        },
    ),
    (
        "mcp",
        CommandMeta {
            canonical: "mcp",
            aliases: &[],
            parse_fn: parse_mcp,
            description: "Show MCP server status",
            args_hint: None,
        },
    ),
    (
        "memory",
        CommandMeta {
            canonical: "memory",
            aliases: &[],
            parse_fn: parse_memory,
            description: "Browse the memory store",
            args_hint: Some("[query]"),
        },
    ),
    (
        "mode",
        CommandMeta {
            canonical: "mode",
            aliases: &[],
            parse_fn: parse_mode,
            description: "Show or switch the active mode  (/mode [safe|plan|auto], aliases yolo/ask/guard/review)",
            args_hint: Some("[mode-name]"),
        },
    ),
    (
        "model",
        CommandMeta {
            canonical: "model",
            aliases: &[],
            parse_fn: parse_model,
            description: "Switch the LLM model mid-session",
            args_hint: Some("<name>"),
        },
    ),
    (
        "models",
        CommandMeta {
            canonical: "models",
            aliases: &[],
            parse_fn: parse_models,
            description: "List all configured LLM providers and their default models",
            args_hint: None,
        },
    ),
    (
        "permissions",
        CommandMeta {
            canonical: "permissions",
            aliases: &[],
            parse_fn: parse_permissions,
            description: "Open the per-tool permission editor",
            args_hint: None,
        },
    ),
    (
        "pr",
        CommandMeta {
            canonical: "pr",
            aliases: &[],
            parse_fn: parse_pr,
            description: "Create a GitHub pull request",
            args_hint: Some("[title]"),
        },
    ),
    (
        "processes",
        CommandMeta {
            canonical: "ps",
            aliases: &["processes"],
            parse_fn: parse_ps,
            description: "Open the live manager for owned background terminals (alias: /ps)",
            args_hint: Some("[terminal-id] [--json]"),
        },
    ),
    (
        "ps",
        CommandMeta {
            canonical: "ps",
            aliases: &["processes"],
            parse_fn: parse_ps,
            description: "Open the live manager for owned background terminals",
            args_hint: Some("[terminal-id] [--json]"),
        },
    ),
    (
        "q",
        CommandMeta {
            canonical: "quit",
            aliases: &["exit", "q"],
            parse_fn: parse_quit,
            description: "Exit xaft (alias: /quit)",
            args_hint: None,
        },
    ),
    (
        "quit",
        CommandMeta {
            canonical: "quit",
            aliases: &["exit", "q"],
            parse_fn: parse_quit,
            description: "Exit xaft",
            args_hint: None,
        },
    ),
    (
        "replay",
        CommandMeta {
            canonical: "replay",
            aliases: &[],
            parse_fn: parse_replay,
            description: "Replay a previous session's conversation in the transcript",
            args_hint: Some("[session-id]"),
        },
    ),
    (
        "resume",
        CommandMeta {
            canonical: "resume",
            aliases: &[],
            parse_fn: parse_resume,
            description: "Resume a prior session",
            args_hint: Some("[session-id]"),
        },
    ),
    (
        "rewind",
        CommandMeta {
            canonical: "rewind",
            aliases: &[],
            parse_fn: parse_rewind,
            description: "Roll back transcript to message N",
            args_hint: Some("[N]"),
        },
    ),
    (
        "skills",
        CommandMeta {
            canonical: "skills",
            aliases: &[],
            parse_fn: parse_skills,
            description: "List the skills available to this session",
            args_hint: None,
        },
    ),
    (
        "startup",
        CommandMeta {
            canonical: "startup",
            aliases: &[],
            parse_fn: parse_startup,
            description: "Show startup phase readiness and timing diagnostics",
            args_hint: None,
        },
    ),
    (
        "status",
        CommandMeta {
            canonical: "status",
            aliases: &[],
            parse_fn: parse_status,
            description: "Show the active session and model",
            args_hint: None,
        },
    ),
    (
        "stop",
        CommandMeta {
            canonical: "stop",
            aliases: &["stop-terminal"],
            parse_fn: parse_stop,
            description: "Stop all or one named background terminal",
            args_hint: Some("[terminal-id|all] [--force]"),
        },
    ),
    (
        "stop-terminal",
        CommandMeta {
            canonical: "stop",
            aliases: &["stop-terminal"],
            parse_fn: parse_stop,
            description: "Stop all or one named background terminal (alias: /stop)",
            args_hint: Some("[terminal-id|all] [--force]"),
        },
    ),
    (
        "theme",
        CommandMeta {
            canonical: "theme",
            aliases: &[],
            parse_fn: parse_theme,
            description: "Cycle or set the TUI colour theme",
            args_hint: Some("[name]"),
        },
    ),
    (
        "tokens",
        CommandMeta {
            canonical: "cost",
            aliases: &["tokens", "usage"],
            parse_fn: parse_cost,
            description: "Show per-agent token and cost table (alias: /cost, /usage)",
            args_hint: None,
        },
    ),
    (
        "tools",
        CommandMeta {
            canonical: "tools",
            aliases: &[],
            parse_fn: parse_tools,
            description: "List all tools registered for this session",
            args_hint: None,
        },
    ),
    (
        "usage",
        CommandMeta {
            canonical: "cost",
            aliases: &["tokens", "usage"],
            parse_fn: parse_cost,
            description: "Show per-agent token and cost table (alias: /cost, /usage)",
            args_hint: None,
        },
    ),
    (
        "vi",
        CommandMeta {
            canonical: "vim",
            aliases: &["vi"],
            parse_fn: parse_vim,
            description: "Switch input bar to Vim keybindings (alias: /vim)",
            args_hint: None,
        },
    ),
    (
        "vim",
        CommandMeta {
            canonical: "vim",
            aliases: &["vi"],
            parse_fn: parse_vim,
            description: "Switch input bar to Vim keybindings",
            args_hint: None,
        },
    ),
    (
        "workers",
        CommandMeta {
            canonical: "workers",
            aliases: &[],
            parse_fn: parse_workers,
            description: "Show durable parallel worker tasks",
            args_hint: Some("[--json]"),
        },
    ),
    (
        "workflow",
        CommandMeta {
            canonical: "workflow",
            aliases: &[],
            parse_fn: parse_workflow,
            description: "Show or switch the session's workflow  (/workflow [name|reset])",
            args_hint: Some("[workflow-name|reset]"),
        },
    ),
    (
        "workflows",
        CommandMeta {
            canonical: "workflows",
            aliases: &[],
            parse_fn: parse_workflows,
            description: "List the workflows available in this session",
            args_hint: None,
        },
    ),
];

// ── SlashCommandParser ────────────────────────────────────────────────────────

/// Parses raw input strings into `SlashCommand` values.
pub struct SlashCommandParser;

impl SlashCommandParser {
    /// Parse a raw input string.
    ///
    /// Returns `None` when the input is not a slash command (does not start
    /// with `/`, or is just `/` with no trigger word yet).
    /// Returns `Some(Err(msg))` when the trigger is known but arguments are invalid.
    /// Returns `Some(Ok(cmd))` when parsing succeeds.
    pub fn parse(input: &str) -> Option<Result<SlashCommand, String>> {
        let input = input.trim_end();

        // Must start with '/' (at position 0, no leading spaces).
        if !input.starts_with('/') {
            return None;
        }

        // Strip leading '/'.
        let after_slash = &input[1..];

        // Split on first whitespace.
        let (trigger_raw, args) = match after_slash.find(char::is_whitespace) {
            Some(pos) => (&after_slash[..pos], after_slash[pos + 1..].trim()),
            None => (after_slash, ""),
        };

        // Bare slash: no trigger yet.
        if trigger_raw.is_empty() {
            return None;
        }

        let trigger = trigger_raw.to_ascii_lowercase();

        // Look up in COMMAND_TABLE (linear search on sorted slice).
        match Self::find_entry(&trigger) {
            None => Some(Err(format!(
                "Unknown command: /{}. Type /help for a list.",
                trigger
            ))),
            Some(meta) => Some((meta.parse_fn)(args)),
        }
    }

    /// Return all trigger strings (sorted) whose prefix matches `partial`.
    /// `partial` should NOT include the leading `/`.
    pub fn completions(partial: &str) -> Vec<&'static str> {
        let p = partial.to_ascii_lowercase();
        let mut result: Vec<&'static str> = COMMAND_TABLE
            .iter()
            .filter(|(k, _)| k.starts_with(p.as_str()))
            .map(|(k, _)| *k)
            .collect();
        result.sort_unstable();
        result
    }

    /// Return the single best completion when `partial` uniquely identifies
    /// one command, otherwise `None`.
    pub fn unique_completion(partial: &str) -> Option<&'static str> {
        let matches = Self::completions(partial);
        if matches.len() == 1 {
            Some(matches[0])
        } else {
            None
        }
    }

    /// Look up an entry in COMMAND_TABLE by exact key (binary search since table is sorted).
    fn find_entry(trigger: &str) -> Option<&'static CommandMeta> {
        // Binary search by key.
        let idx = COMMAND_TABLE.binary_search_by_key(&trigger, |(k, _)| k);
        idx.ok().map(|i| &COMMAND_TABLE[i].1)
    }

    /// Get CommandMeta by trigger string (used by palette).
    pub fn get_meta(trigger: &str) -> Option<&'static CommandMeta> {
        Self::find_entry(trigger)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_slash_help() {
        assert_eq!(
            SlashCommandParser::parse("/help"),
            Some(Ok(SlashCommand::Help))
        );
    }

    #[test]
    fn parse_slash_help_uppercase() {
        assert_eq!(
            SlashCommandParser::parse("/HELP"),
            Some(Ok(SlashCommand::Help))
        );
    }

    #[test]
    fn parse_slash_clear_alias() {
        assert_eq!(
            SlashCommandParser::parse("/cls"),
            Some(Ok(SlashCommand::Clear))
        );
    }

    #[test]
    fn parse_slash_compact_alias() {
        assert_eq!(
            SlashCommandParser::parse("/ctx"),
            Some(Ok(SlashCommand::Compact))
        );
    }

    #[test]
    fn parse_slash_model_ok() {
        assert_eq!(
            SlashCommandParser::parse("/model claude-opus-4-8"),
            Some(Ok(SlashCommand::Model {
                name: "claude-opus-4-8".into()
            }))
        );
    }

    #[test]
    fn parse_slash_model_missing_arg() {
        let result = SlashCommandParser::parse("/model");
        assert!(matches!(result, Some(Err(_))));
        if let Some(Err(msg)) = result {
            assert!(msg.contains("/model requires"), "msg was: {msg}");
        }
    }

    #[test]
    fn parse_slash_resume_no_arg() {
        assert_eq!(
            SlashCommandParser::parse("/resume"),
            Some(Ok(SlashCommand::Resume { id: None }))
        );
    }

    #[test]
    fn parse_slash_resume_with_id() {
        assert_eq!(
            SlashCommandParser::parse("/resume f7fba85e"),
            Some(Ok(SlashCommand::Resume {
                id: Some("f7fba85e".into())
            }))
        );
    }

    #[test]
    fn parse_slash_rewind_number() {
        assert_eq!(
            SlashCommandParser::parse("/rewind 5"),
            Some(Ok(SlashCommand::Rewind { msg_index: Some(5) }))
        );
    }

    #[test]
    fn parse_slash_rewind_bad_arg() {
        let result = SlashCommandParser::parse("/rewind foo");
        assert!(matches!(result, Some(Err(_))));
        if let Some(Err(msg)) = result {
            assert!(msg.contains("expects a message number"), "msg was: {msg}");
        }
    }

    #[test]
    fn parse_slash_theme_no_arg() {
        assert_eq!(
            SlashCommandParser::parse("/theme"),
            Some(Ok(SlashCommand::Theme { name: None }))
        );
    }

    #[test]
    fn parse_slash_theme_named() {
        assert_eq!(
            SlashCommandParser::parse("/theme nord"),
            Some(Ok(SlashCommand::Theme {
                name: Some("nord".into())
            }))
        );
    }

    #[test]
    fn parse_slash_pr_no_arg() {
        assert_eq!(
            SlashCommandParser::parse("/pr"),
            Some(Ok(SlashCommand::Pr { title: None }))
        );
    }

    #[test]
    fn parse_slash_pr_with_title() {
        assert_eq!(
            SlashCommandParser::parse("/pr Add entropy improvements"),
            Some(Ok(SlashCommand::Pr {
                title: Some("Add entropy improvements".into())
            }))
        );
    }

    #[test]
    fn parse_plain_text_returns_none() {
        assert_eq!(SlashCommandParser::parse("hello world"), None);
    }

    #[test]
    fn parse_bare_slash_returns_none() {
        assert_eq!(SlashCommandParser::parse("/"), None);
    }

    #[test]
    fn parse_space_prefix_not_slash_command() {
        // " /help" has a leading space — not a slash command.
        assert_eq!(SlashCommandParser::parse(" /help"), None);
    }

    #[test]
    fn parse_unknown_trigger_returns_err() {
        let result = SlashCommandParser::parse("/blorp");
        assert!(matches!(result, Some(Err(_))));
        if let Some(Err(msg)) = result {
            assert!(msg.contains("Unknown command: /blorp"), "msg was: {msg}");
            assert!(msg.contains("/help"), "msg was: {msg}");
        }
    }

    #[test]
    fn completions_empty_prefix_returns_all() {
        let all = SlashCommandParser::completions("");
        // Every known primary trigger must be present.
        for trigger in &["help", "clear", "compact", "cost", "agents", "model"] {
            assert!(all.contains(trigger), "missing {trigger}");
        }
    }

    #[test]
    fn completions_prefix_c() {
        let cs = SlashCommandParser::completions("c");
        assert!(cs.contains(&"clear"), "missing clear");
        assert!(cs.contains(&"compact"), "missing compact");
        assert!(cs.contains(&"cost"), "missing cost");
        assert!(cs.contains(&"config"), "missing config");
        assert!(cs.contains(&"commit"), "missing commit");
        assert!(
            !cs.contains(&"help"),
            "help should not be in 'c' completions"
        );
    }

    #[test]
    fn completions_prefix_cl() {
        let cs = SlashCommandParser::completions("cl");
        // "cl" matches "clear" and "cls"
        assert!(cs.contains(&"clear"), "missing clear");
        // Also check that "cls" is included
        assert!(cs.contains(&"cls"), "missing cls");
        assert!(
            !cs.contains(&"compact"),
            "compact should not be in 'cl' completions"
        );
    }

    #[test]
    fn unique_completion_hit() {
        // "cle" uniquely matches "clear" (and "cls" doesn't start with "cle")
        assert_eq!(SlashCommandParser::unique_completion("cle"), Some("clear"));
    }

    #[test]
    fn unique_completion_miss_ambiguous() {
        assert_eq!(SlashCommandParser::unique_completion("c"), None);
    }

    #[test]
    fn unique_completion_miss_empty() {
        assert_eq!(SlashCommandParser::unique_completion("zzz"), None);
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(
            SlashCommandParser::parse("/CLEAR"),
            Some(Ok(SlashCommand::Clear))
        );
    }

    #[test]
    fn command_table_is_sorted() {
        let keys: Vec<&str> = COMMAND_TABLE.iter().map(|(k, _)| *k).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "COMMAND_TABLE must be sorted by trigger");
    }

    /// Every alias must be reachable as a command in its own right.
    ///
    /// `CommandMeta::aliases` is only metadata: `parse` resolves a trigger with
    /// `find_entry`, which is a lookup on the table *key*. An alias therefore
    /// does not work merely by being listed here — it needs its own entry, the
    /// way `interrupt`, `tokens` and `cls` have. Declaring an alias and
    /// forgetting the row produces a command that appears in `/help` and in
    /// `/commands` yet answers "Unknown command" when typed, which is how
    /// `/usage` shipped: the matrix called the row done while the parser could
    /// not reach it.
    #[test]
    fn every_declared_alias_parses_as_a_command() {
        let mut missing: Vec<&str> = Vec::new();
        for (key, meta) in COMMAND_TABLE {
            for alias in meta.aliases {
                // The alias may legitimately be its own entry (the usual case)
                // or be the key we are currently visiting. Anything else means
                // the alias is advertised but unreachable.
                if *alias != *key && !COMMAND_TABLE.iter().any(|(k, _)| k == alias) {
                    missing.push(alias);
                }
            }
        }
        assert!(
            missing.is_empty(),
            "aliases advertised in CommandMeta but absent from COMMAND_TABLE \
             (they would answer 'Unknown command'): {missing:?}"
        );
    }

    /// The specific regression this guard was added for.
    #[test]
    fn usage_alias_resolves_to_cost() {
        assert_eq!(
            SlashCommandParser::parse("/usage"),
            Some(Ok(SlashCommand::Cost)),
            "/usage is agenthicc's own name for the token/cost table"
        );
    }

    /// Every registered trigger parses without panicking.
    ///
    /// `COMMAND_TABLE` is the single source of truth for what `/` accepts, so
    /// this drives every entry through its own `parse_fn`. A command with no
    /// argument hint must accept an empty argument string rather than erroring.
    #[test]
    fn every_registered_command_parses_with_no_args() {
        for (trigger, meta) in COMMAND_TABLE {
            let parsed = (meta.parse_fn)("");
            if meta.args_hint.is_none() {
                assert!(
                    parsed.is_ok(),
                    "/{trigger} takes no args but rejected an empty arg string: {parsed:?}"
                );
            }
        }
    }

    /// A canonical trigger round-trips to a command whose parser was actually
    /// invoked. Commands that *require* an argument are exercised with one.
    #[test]
    fn every_registered_trigger_round_trips_through_the_parser() {
        for (trigger, meta) in COMMAND_TABLE {
            let args = match meta.args_hint {
                // /rewind takes a message *number*; "sample" would be rejected.
                Some(_) if *trigger == "rewind" => format!("/{trigger} 1"),
                Some(_) => format!("/{trigger} sample"),
                None => format!("/{trigger}"),
            };
            let parsed = SlashCommandParser::parse(&args);
            assert!(
                parsed.is_some(),
                "/{trigger} is in COMMAND_TABLE but the parser does not resolve it"
            );
            let result = parsed.expect("resolved above");
            assert!(
                result.is_ok(),
                "/{args} must parse successfully, got {result:?}"
            );
        }
    }

    /// Triggers are matched case-insensitively.
    #[test]
    fn every_registered_trigger_is_case_insensitive() {
        for (trigger, meta) in COMMAND_TABLE {
            let lower = SlashCommandParser::parse(&format!("/{trigger}"));
            let upper = SlashCommandParser::parse(&format!("/{}", trigger.to_uppercase()));
            assert_eq!(
                lower, upper,
                "/{trigger} must resolve the same regardless of case"
            );
        }
    }

    /// Aliases resolve to the same command as their canonical form.
    #[test]
    fn every_alias_resolves_to_its_canonical_command() {
        for (trigger, meta) in COMMAND_TABLE {
            for alias in meta.aliases {
                let via_alias = SlashCommandParser::parse(&format!("/{alias}"));
                let via_canonical = SlashCommandParser::parse(&format!("/{}", meta.canonical));
                assert_eq!(
                    via_alias, via_canonical,
                    "/{alias} must behave exactly like /{}",
                    meta.canonical
                );
            }
        }
    }

    /// Argument-taking commands: empty means "no argument", non-empty means
    /// "this argument", and surrounding whitespace is trimmed.
    #[test]
    fn argument_commands_distinguish_empty_from_supplied() {
        let cases: &[(&str, &str)] = &[
            ("resume", "sess-42"),
            ("rewind", "7"),
            ("theme", "light"),
            ("pr", "my title"),
            ("mode", "standard"),
            ("expand", "@file.rs"),
            ("workflow", "review"),
            ("replay", "abc"),
        ];
        for (trigger, arg) in cases {
            let with = SlashCommandParser::parse(&format!("/{trigger} {arg}"));
            let without = SlashCommandParser::parse(&format!("/{trigger}"));
            assert!(
                with.clone().is_some_and(|r| r.is_ok()),
                "/{trigger} {arg} must parse: {with:?}"
            );
            assert_ne!(
                with, without,
                "/{trigger} {arg} must differ from bare /{trigger}"
            );
        }
    }

    #[test]
    fn resume_carries_its_session_id_and_trims_whitespace() {
        assert_eq!(
            SlashCommandParser::parse("/resume  sess-9  "),
            Some(Ok(SlashCommand::Resume {
                id: Some("sess-9".to_string())
            })),
        );
        assert_eq!(
            SlashCommandParser::parse("/resume"),
            Some(Ok(SlashCommand::Resume { id: None })),
            "a bare /resume resumes the latest session"
        );
    }

    #[test]
    fn rewind_parses_a_message_index_and_rejects_garbage() {
        assert_eq!(
            SlashCommandParser::parse("/rewind 3"),
            Some(Ok(SlashCommand::Rewind { msg_index: Some(3) }))
        );
        assert_eq!(
            SlashCommandParser::parse("/rewind"),
            Some(Ok(SlashCommand::Rewind { msg_index: None }))
        );
        let bad = SlashCommandParser::parse("/rewind abc").expect("resolves");
        let err = bad.expect_err("non-numeric index must be rejected");
        assert!(
            err.contains("message number"),
            "the error must name what was expected, got {err:?}"
        );
    }

    #[test]
    fn model_requires_a_name() {
        assert_eq!(
            SlashCommandParser::parse("/model"),
            Some(Err(
                "/model requires a model name.\n  Usage: /model <name>".to_string()
            ))
        );
        assert_eq!(
            SlashCommandParser::parse("/model gpt-x"),
            Some(Ok(SlashCommand::Model {
                name: "gpt-x".to_string()
            }))
        );
    }

    #[test]
    fn optional_argument_commands_accept_a_bare_invocation() {
        assert_eq!(
            SlashCommandParser::parse("/theme"),
            Some(Ok(SlashCommand::Theme { name: None }))
        );
        assert_eq!(
            SlashCommandParser::parse("/pr"),
            Some(Ok(SlashCommand::Pr { title: None }))
        );
        assert_eq!(
            SlashCommandParser::parse("/mode"),
            Some(Ok(SlashCommand::Mode { name: None }))
        );
        assert_eq!(
            SlashCommandParser::parse("/expand"),
            Some(Ok(SlashCommand::Expand { target: None }))
        );
    }

    /// The table must stay internally consistent: a canonical trigger cannot
    /// be claimed as somebody else's alias.
    ///
    /// Note that alias rows are their own table entries (the key is the alias,
    /// `canonical` points at the real command), so the key is only required to
    /// equal `canonical` for the row that defines the command.
    #[test]
    fn no_canonical_trigger_is_shadowed_by_a_foreign_alias() {
        let canonicals: std::collections::HashSet<&str> = COMMAND_TABLE
            .iter()
            .filter(|(trigger, meta)| *trigger == meta.canonical)
            .map(|(_, m)| m.canonical)
            .collect();
        assert!(
            !canonicals.is_empty(),
            "COMMAND_TABLE must define at least one canonical trigger"
        );
        for (_, meta) in COMMAND_TABLE {
            for alias in meta.aliases {
                assert!(
                    !canonicals.contains(alias) || *alias == meta.canonical,
                    "/{alias} is an alias of /{} but is also a canonical trigger",
                    meta.canonical
                );
            }
            assert!(
                !meta.canonical.is_empty(),
                "every command needs a canonical trigger"
            );
        }
    }
}
