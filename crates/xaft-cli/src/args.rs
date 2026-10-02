//! CLI argument definitions using clap.
//!
//! Defines `XaftCli` (the top-level parser) and all subcommands.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use xaft_config::{CliOverrides, LogLevel};

// ── Top-level CLI ─────────────────────────────────────────────────────────────

/// xaft — autonomous coding agent
///
/// Run `xaft run "task"` to start an agent, or use a subcommand.
///
/// Examples:
///   xaft run "Add error handling to src/api.rs"
///   xaft run "Write tests for the user module" --model gpt-4o
///   xaft config show
///   xaft session list
#[derive(Debug, Parser)]
#[command(
    name = "xaft",
    author,
    version,
    about = "Autonomous coding agent — make autonomous coding safe, observable, and reversible",
    long_about = None,
    // No arg_required_else_help — bare `xaft` opens the interactive TUI
    styles = clap_styles(),
)]
pub struct XaftCli {
    /// Subcommand to run (defaults to interactive TUI when omitted).
    #[command(subcommand)]
    pub command: Option<Commands>,
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Top-level xaft subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run an agent on a coding task.
    ///
    /// The agent will plan, execute, verify, and deliver a code change.
    /// All file edits go through a transactional workspace; git operations
    /// use an isolated branch.
    ///
    /// Examples:
    ///   xaft run "Add rate limiting to the API"
    ///   xaft run "Fix the failing tests in src/auth/" --dry-run
    ///   xaft run "Migrate from reqwest 0.11 to 0.12" --model claude-3-opus
    #[command(alias = "r")]
    Run(RunArgs),

    /// Configuration management.
    ///
    /// Examples:
    ///   xaft config show
    ///   xaft config init
    ///   xaft config init --global
    Config(ConfigCommand),

    /// Session management.
    ///
    /// Sessions persist across interruptions. Use `resume` to continue
    /// a suspended session.
    ///
    /// Examples:
    ///   xaft session list
    ///   xaft session show <id>
    ///   xaft session resume <id>
    #[command(alias = "s")]
    Session(SessionCommand),

    /// Generate shell completion scripts.
    ///
    /// Examples:
    ///   xaft completions bash >> ~/.bashrc
    ///   xaft completions zsh > ~/.zfunc/_xaft
    ///   xaft completions fish > ~/.config/fish/completions/xaft.fish
    #[command(alias = "comp")]
    Completions(CompletionsArgs),

    /// Show version information.
    #[command(alias = "v")]
    Version(VersionArgs),

    /// Manage durable background sessions.
    ///
    /// Examples:
    ///   xaft jobs list
    ///   xaft jobs list --trash
    ///   xaft jobs status <id> --json
    ///   xaft jobs cancel <id>
    ///   xaft jobs approve <id>
    ///   xaft jobs purge
    #[command(alias = "job")]
    Jobs(JobsCommand),

    /// Inspect and control durable goal runs.
    ///
    /// A *goal run* is the product-level record of one `xaft run`: the goal
    /// statement, the repository it targets, and its lifecycle. Distinct from
    /// `jobs`, which tracks individual background sessions.
    ///
    /// Examples:
    ///   xaft runs
    ///   xaft runs list
    ///   xaft runs list --status running
    ///   xaft runs show run_01HQ...
    ///   xaft runs cancel run_01HQ...
    ///
    /// Bare `xaft runs` is an alias of `xaft runs list`, as in agenthicc.
    Runs(RunsCommand),

    /// Manage configured MCP servers.
    ///
    /// These subcommands only read and write the `[[mcp.client]]` stanzas in
    /// your config file. They never open a connection, so they are safe to
    /// run in a script and safe to run when a server is down.
    ///
    /// Examples:
    ///   xaft mcp list
    ///   xaft mcp add fileserver --transport stdio ./mcp-files
    ///   xaft mcp get fileserver
    ///   xaft mcp remove fileserver
    Mcp(McpCommand),

    /// Discover and select workflows.
    ///
    /// `workflows` lists every workflow xaft can run \u2014 the three built-in
    /// orchestrators plus anything declared under `[workflows.<name>]` in your
    /// `xaft.toml` \u2014 and `workflows run` executes one headlessly for a
    /// single intent.
    ///
    /// Examples:
    ///   xaft workflows list
    ///   xaft workflows list --json
    ///   xaft workflows run dynamic "refactor the parser"
    #[command(alias = "workflow")]
    Workflows(WorkflowsCommand),
}

// ── Run command ───────────────────────────────────────────────────────────────

/// Arguments for `xaft run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// The coding task to perform (natural language description).
    ///
    /// Be specific about what you want: mention files, modules, or
    /// acceptance criteria when relevant.
    ///
    /// Examples:
    ///   "Add pagination to the /users API endpoint"
    ///   "Fix the connection pool leak in src/db/pool.rs"
    ///   "Write unit tests for all public functions in src/math.rs"
    #[arg(index = 1)]
    pub task: Option<String>,

    // ── Model / Provider ──────────────────────────────────────────────────────
    /// Override the LLM model (e.g. claude-3-5-sonnet-20241022, gpt-4o).
    ///
    /// Overrides the model configured in your `xaft.toml` and
    /// the `XAFT_MODEL` environment variable.
    #[arg(long, short = 'm', env = "XAFT_MODEL_OVERRIDE")]
    pub model: Option<String>,

    /// Override the LLM provider (anthropic, openai, ollama).
    #[arg(long)]
    pub provider: Option<String>,

    /// Use a named agent preset from your config.
    ///
    /// Presets bundle model, provider, system prompt, and tool permissions.
    /// See `xaft config show` for available presets.
    #[arg(long, short = 'a')]
    pub agent: Option<String>,

    /// Select the workflow that executes this run.
    ///
    /// Accepts a built-in (`standard`, `dynamic`, `meta`) or any
    /// `[workflows.<name>]` table from your `xaft.toml`. With no flag the run
    /// uses the `[workflow] mode` setting, so the config key is honoured rather
    /// than hard-coded. See `xaft workflows list`.
    #[arg(long, short = 'w', env = "XAFT_WORKFLOW")]
    pub workflow: Option<String>,

    // ── Execution parameters ──────────────────────────────────────────────────
    /// Override the maximum number of agent turns.
    #[arg(long)]
    pub max_turns: Option<u32>,

    /// Override the sampling temperature (0.0 = deterministic, 2.0 = creative).
    #[arg(long)]
    pub temperature: Option<f32>,

    // ── Session management ────────────────────────────────────────────────────
    /// Resume the most recent session in the current directory.
    ///
    /// Automatically finds and resumes the last active session for this
    /// project directory, reloading all prior conversation context.
    /// Use `--resume <ID>` to resume a specific session by ID.
    #[arg(long, short = 'k')]
    pub r#continue: bool,

    /// Resume a previous conversation by session ID.
    ///
    /// The agent will load prior context before starting the new task.
    /// Picks up where the previous session left off, including conversation
    /// history and pending file edits.
    #[arg(long, short = 'r', value_name = "SESSION_ID")]
    pub resume: Option<String>,

    /// Alias for --resume (deprecated, kept for backward compatibility).
    #[arg(long, short = 's', value_name = "SESSION_ID", hide = true)]
    pub session: Option<String>,

    // ── Config / Project ──────────────────────────────────────────────────────
    /// Path to a specific config file (skips automatic discovery).
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Override the project root directory.
    #[arg(long, value_name = "DIR")]
    pub project_dir: Option<PathBuf>,

    // ── Output / UX ───────────────────────────────────────────────────────────
    /// Disable the interactive TUI (useful for CI/CD and scripting).
    ///
    /// Outputs structured JSON events to stdout when combined with `--json`.
    #[arg(long, alias = "no-tui")]
    pub headless: bool,

    /// Output structured JSON events to stdout (implies --headless).
    ///
    /// Each event is a newline-delimited JSON object. Useful for piping
    /// xaft output into other tools.
    #[arg(long)]
    pub json: bool,

    /// Show the planned steps without executing any changes.
    ///
    /// The agent will plan the task and print what it would do, but no
    /// files will be modified and no shell commands will be run.
    #[arg(long)]
    pub dry_run: bool,

    /// Auto-approve all confirmation prompts (-y / --yes).
    ///
    /// Equivalent to setting `guardrail.command_approval = false` in config.
    /// Use with care — this allows destructive operations without confirmation.
    #[arg(long, short = 'y', alias = "yes")]
    pub auto_approve: bool,

    /// Skip ALL tool-call approval gates without asking — agents will execute
    /// shell commands, file deletions, etc. without confirmation.
    ///
    /// In TUI mode a danger warning is displayed first and the user must
    /// explicitly confirm before the run proceeds.
    ///
    /// **USE WITH EXTREME CAUTION.**
    #[arg(long)]
    pub dangerously_skip_permissions: bool,

    // ── Observability ─────────────────────────────────────────────────────────
    /// Override the log level.
    #[arg(long, value_enum)]
    pub log_level: Option<LogLevelArg>,

    /// Disable telemetry for this run.
    #[arg(long)]
    pub no_telemetry: bool,
}

impl RunArgs {
    /// Convert `RunArgs` into `CliOverrides` for config loading.
    pub fn to_cli_overrides(&self) -> CliOverrides {
        CliOverrides {
            model: self.model.clone(),
            provider: self.provider.clone(),
            agent_preset: self.agent.clone(),
            max_turns: self.max_turns,
            temperature: self.temperature,
            config_file: self.config.clone(),
            session_id: self.resume.clone().or(self.session.clone()),
            project_dir: self.project_dir.clone(),
            log_level: self.log_level.as_ref().map(|l| l.to_log_level()),
            no_telemetry: self.no_telemetry,
            auto_approve: self.auto_approve,
            headless: self.headless || self.json,
            dry_run: self.dry_run,
        }
    }
}

// ── Config command ────────────────────────────────────────────────────────────

/// Arguments for `xaft config`.
#[derive(Debug, Args)]
pub struct ConfigCommand {
    /// Config subcommand to execute.
    #[command(subcommand)]
    pub subcommand: ConfigSubcommand,
}

/// Subcommands for `xaft config`.
#[derive(Debug, Subcommand)]
pub enum ConfigSubcommand {
    /// Show the fully resolved configuration.
    ///
    /// Displays the effective config after merging all layers:
    /// built-in defaults → user global → project → env vars → CLI flags.
    Show(ConfigShowArgs),

    /// Create a new xaft config file.
    ///
    /// By default creates `.xaft/xaft.toml` in the current directory.
    /// Use `--global` to create `~/.config/xaft/xaft.toml`.
    Init(ConfigInitArgs),

    /// Validate the current configuration.
    ///
    /// Checks all config values and reports errors without running an agent.
    Validate(ConfigValidateArgs),

    /// List all available agent presets.
    Presets,

    /// List the configured provider profiles and their connection metadata.
    Profiles(ConfigProfilesArgs),
}

/// Arguments for `xaft config show`.
#[derive(Debug, Args)]
pub struct ConfigShowArgs {
    /// Output format.
    #[arg(long, short = 'f', value_enum, default_value = "pretty")]
    pub format: OutputFormat,

    /// Show the config file path(s) being loaded.
    #[arg(long)]
    pub show_paths: bool,

    /// Path to a specific config file.
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// Arguments for `xaft config init`.
#[derive(Debug, Args)]
pub struct ConfigInitArgs {
    /// Create the global user config (`~/.config/xaft/xaft.toml`).
    ///
    /// Without this flag, creates `.xaft/xaft.toml` in the current directory.
    #[arg(long)]
    pub global: bool,

    /// Overwrite if file already exists.
    #[arg(long, short = 'f')]
    pub force: bool,

    /// Path to create the config file at.
    #[arg(long, short = 'o', value_name = "PATH")]
    pub output: Option<PathBuf>,
}

/// Arguments for `xaft config validate`.
#[derive(Debug, Args)]
pub struct ConfigValidateArgs {
    /// Path to a specific config file.
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// Arguments for `xaft config profiles`.
#[derive(Debug, Args)]
pub struct ConfigProfilesArgs {
    /// Path to a specific config file.
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,
}

// ── Session command ───────────────────────────────────────────────────────────

/// Arguments for `xaft session`.
#[derive(Debug, Args)]
pub struct SessionCommand {
    /// Session subcommand to execute.
    #[command(subcommand)]
    pub subcommand: SessionSubcommand,
}

/// Subcommands for `xaft session`.
#[derive(Debug, Subcommand)]
pub enum SessionSubcommand {
    /// List all sessions.
    ///
    /// Shows sessions for the current project directory by default.
    List(SessionListArgs),

    /// Show details for a specific session.
    Show(SessionShowArgs),

    /// Resume a suspended session.
    Resume(SessionResumeArgs),

    /// Cancel and clean up a session.
    Cancel(SessionCancelArgs),

    /// Export one session and its conversation as a JSON document.
    ///
    /// This is agenthicc's `sessions export` (`cli/commands/sessions.py`),
    /// which dumps the local session log. It is NOT the `session export` of
    /// `cli/commands/session_service.py` (matrix row 12), which serialises a
    /// capability-checked service snapshot; xaft has no such service.
    Export(SessionsExportArgs),

    /// Summarise one session's durable state.
    ///
    /// This is agenthicc's `sessions inspect` (`cli/commands/sessions.py`).
    Inspect(SessionsInspectArgs),
}

/// Arguments for `xaft session list`.
#[derive(Debug, Args)]
pub struct SessionListArgs {
    /// Show sessions for all projects (not just the current directory).
    #[arg(long, short = 'a')]
    pub all: bool,

    /// Filter by status.
    #[arg(long, value_enum)]
    pub status: Option<SessionStatusFilter>,

    /// Number of sessions to show.
    #[arg(long, short = 'n', default_value = "20")]
    pub limit: usize,

    /// Output format.
    #[arg(long, short = 'f', value_enum, default_value = "table")]
    pub format: OutputFormat,
}

/// Arguments for `xaft session show`.
#[derive(Debug, Args)]
pub struct SessionShowArgs {
    /// Session ID to show.
    pub id: String,

    /// Output format.
    #[arg(long, short = 'f', value_enum, default_value = "pretty")]
    pub format: OutputFormat,
}

/// Arguments for `xaft session resume`.
#[derive(Debug, Args)]
pub struct SessionResumeArgs {
    /// Session ID to resume.
    pub id: String,

    /// Auto-approve all confirmations.
    #[arg(long, short = 'y')]
    pub auto_approve: bool,

    /// Disable TUI.
    #[arg(long)]
    pub headless: bool,
}

/// Arguments for `xaft session cancel`.
#[derive(Debug, Args)]
pub struct SessionCancelArgs {
    /// Session ID to cancel.
    pub id: String,

    /// Skip confirmation prompt.
    #[arg(long, short = 'f')]
    pub force: bool,
}

// ── Sessions export/inspect ─────────────────────────────────────────────────

/// Arguments for `xaft session export`.
///
/// This is agenthicc's `sessions export` from `cli/commands/sessions.py`, which
/// dumps xaft's own durable session log. See the note on
/// `SessionSubcommand::Export` for why it shares `xaft session` with the
/// service-family rows.
///
/// There is deliberately no `--config` here. The runtime — and therefore the
/// `SessionManager` handed to this command as a
/// [`SessionBackend`](xaft_runtime::session_backend::SessionBackend) — is
/// bootstrapped in `main.rs` *before* clap ever runs, and `--config` is not a
/// global flag, so a command-local one would resolve a `core.data_dir` the
/// runtime never used. That is how `xaft session list` and
/// `xaft session inspect` could end up disagreeing about which sessions exist.
#[derive(Debug, Args)]
pub struct SessionsExportArgs {
    /// Session ID to export.
    pub id: String,

    /// Destination file. Defaults to `<SESSION_ID>.json` in the current directory.
    #[arg(long, short = 'o', value_name = "PATH")]
    pub output: Option<PathBuf>,
}

/// Arguments for `xaft session inspect`.
///
/// This is agenthicc's `sessions inspect` from `cli/commands/sessions.py`.
///
/// No `--config`, for the same reason as [`SessionsExportArgs`]: the store is
/// injected, not resolved by this command.
#[derive(Debug, Args)]
pub struct SessionsInspectArgs {
    /// Session ID to inspect.
    pub id: String,

    /// Emit the summary as JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

// ── Completions command ───────────────────────────────────────────────────────

/// Arguments for `xaft completions`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    pub shell: ShellArg,
}

/// Supported shells for completion generation.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ShellArg {
    /// Bash shell.
    Bash,
    /// Zsh shell.
    Zsh,
    /// Fish shell.
    Fish,
    /// PowerShell.
    #[value(alias = "ps")]
    PowerShell,
    /// Elvish shell.
    Elvish,
}

impl From<ShellArg> for clap_complete::Shell {
    fn from(s: ShellArg) -> Self {
        match s {
            ShellArg::Bash => clap_complete::Shell::Bash,
            ShellArg::Zsh => clap_complete::Shell::Zsh,
            ShellArg::Fish => clap_complete::Shell::Fish,
            ShellArg::PowerShell => clap_complete::Shell::PowerShell,
            ShellArg::Elvish => clap_complete::Shell::Elvish,
        }
    }
}

// ── Version command ───────────────────────────────────────────────────────────

/// Arguments for `xaft version`.
#[derive(Debug, Args)]
pub struct VersionArgs {
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

// ── Shared value types ────────────────────────────────────────────────────────

/// Output format for config/session commands.
#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// Human-readable output with ANSI colors.
    #[default]
    Pretty,
    /// Plain text (no ANSI).
    Plain,
    /// JSON output.
    Json,
    /// TOML output.
    Toml,
    /// ASCII table.
    Table,
}

/// Session status filter for `xaft session list`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SessionStatusFilter {
    /// Active sessions.
    Active,
    /// Suspended sessions.
    Suspended,
    /// Completed sessions.
    Completed,
    /// Failed sessions.
    Failed,
    /// Cancelled sessions.
    Cancelled,
}

/// Log level for the `--log-level` flag.
///
/// Maps directly to `xaft_config::LogLevel`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LogLevelArg {
    /// Trace-level (very verbose).
    Trace,
    /// Debug-level.
    Debug,
    /// Info-level (default).
    Info,
    /// Warning-level.
    Warn,
    /// Error-level only.
    Error,
}

impl LogLevelArg {
    /// Convert to `xaft_config::LogLevel`.
    pub fn to_log_level(self) -> LogLevel {
        match self {
            Self::Trace => LogLevel::Trace,
            Self::Debug => LogLevel::Debug,
            Self::Info => LogLevel::Info,
            Self::Warn => LogLevel::Warn,
            Self::Error => LogLevel::Error,
        }
    }
}

// ── clap styling ──────────────────────────────────────────────────────────────

fn clap_styles() -> clap::builder::Styles {
    use clap::builder::styling::{AnsiColor, Effects};
    clap::builder::Styles::styled()
        .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .usage(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .literal(AnsiColor::Green.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Cyan.on_default())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_run_with_task() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "fix the bug"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.task.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn parse_run_with_model_flag() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "--model", "gpt-4o"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn parse_run_with_short_model_flag() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "-m", "claude"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.model.as_deref(), Some("claude"));
    }

    #[test]
    fn parse_run_headless_flag() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "--headless"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert!(args.headless);
        assert!(!args.json);
    }

    #[test]
    fn parse_run_json_implies_headless() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "--json"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert!(args.json);
    }

    #[test]
    fn parse_run_dry_run() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "--dry-run"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert!(args.dry_run);
    }

    #[test]
    fn parse_run_auto_approve_short() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "-y"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert!(args.auto_approve);
    }

    #[test]
    fn parse_run_auto_approve_long() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "--auto-approve"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert!(args.auto_approve);
    }

    #[test]
    fn parse_run_with_session_resume() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "-r", "session-abc"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.resume.as_deref(), Some("session-abc"));
    }

    #[test]
    fn parse_run_with_config_path() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "-c", "/tmp/xaft.toml"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.config.unwrap().to_str(), Some("/tmp/xaft.toml"));
    }

    #[test]
    fn parse_run_max_turns() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "--max-turns", "50"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.max_turns, Some(50));
    }

    #[test]
    fn parse_run_temperature() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "--temperature", "0.7"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert!((args.temperature.unwrap() - 0.7).abs() < 1e-6);
    }

    #[test]
    fn parse_run_log_level() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "--log-level", "debug"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert!(matches!(args.log_level, Some(LogLevelArg::Debug)));
    }

    #[test]
    fn parse_run_no_task_is_ok() {
        // Task is optional — the dispatch layer will prompt or error
        let cli = XaftCli::try_parse_from(["xaft", "run"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert!(args.task.is_none());
    }

    #[test]
    fn parse_config_show() {
        let cli = XaftCli::try_parse_from(["xaft", "config", "show"]).unwrap();
        let Some(Commands::Config(cmd)) = cli.command else {
            panic!("expected Config")
        };
        assert!(matches!(cmd.subcommand, ConfigSubcommand::Show(_)));
    }

    #[test]
    fn parse_config_show_json_format() {
        let cli = XaftCli::try_parse_from(["xaft", "config", "show", "--format", "json"]).unwrap();
        let Some(Commands::Config(cmd)) = cli.command else {
            panic!("expected Config")
        };
        let ConfigSubcommand::Show(args) = cmd.subcommand else {
            panic!()
        };
        assert_eq!(args.format, OutputFormat::Json);
    }

    #[test]
    fn parse_config_profiles() {
        let cli = XaftCli::try_parse_from(["xaft", "config", "profiles"]).unwrap();
        let Some(Commands::Config(cmd)) = cli.command else {
            panic!("expected Config")
        };
        let ConfigSubcommand::Profiles(args) = cmd.subcommand else {
            panic!("expected Profiles")
        };
        // No `-c` means "use the normal config search order".
        assert!(args.config.is_none());
    }

    #[test]
    fn parse_config_profiles_with_explicit_config() {
        let cli = XaftCli::try_parse_from(["xaft", "config", "profiles", "-c", "/tmp/xaft.toml"])
            .unwrap();
        let Some(Commands::Config(cmd)) = cli.command else {
            panic!("expected Config")
        };
        let ConfigSubcommand::Profiles(args) = cmd.subcommand else {
            panic!("expected Profiles")
        };
        assert_eq!(
            args.config.as_deref(),
            Some(std::path::Path::new("/tmp/xaft.toml"))
        );
    }

    #[test]
    fn parse_config_init() {
        let cli = XaftCli::try_parse_from(["xaft", "config", "init"]).unwrap();
        let Some(Commands::Config(cmd)) = cli.command else {
            panic!("expected Config")
        };
        assert!(matches!(cmd.subcommand, ConfigSubcommand::Init(_)));
    }

    #[test]
    fn parse_config_init_global() {
        let cli = XaftCli::try_parse_from(["xaft", "config", "init", "--global"]).unwrap();
        let Some(Commands::Config(cmd)) = cli.command else {
            panic!("expected Config")
        };
        let ConfigSubcommand::Init(args) = cmd.subcommand else {
            panic!()
        };
        assert!(args.global);
    }

    #[test]
    fn parse_config_validate() {
        let cli = XaftCli::try_parse_from(["xaft", "config", "validate"]).unwrap();
        let Some(Commands::Config(cmd)) = cli.command else {
            panic!("expected Config")
        };
        assert!(matches!(cmd.subcommand, ConfigSubcommand::Validate(_)));
    }

    #[test]
    fn parse_session_list() {
        let cli = XaftCli::try_parse_from(["xaft", "session", "list"]).unwrap();
        let Some(Commands::Session(cmd)) = cli.command else {
            panic!("expected Session")
        };
        assert!(matches!(cmd.subcommand, SessionSubcommand::List(_)));
    }

    #[test]
    fn parse_session_list_all() {
        let cli = XaftCli::try_parse_from(["xaft", "session", "list", "-a"]).unwrap();
        let Some(Commands::Session(cmd)) = cli.command else {
            panic!("expected Session")
        };
        let SessionSubcommand::List(args) = cmd.subcommand else {
            panic!()
        };
        assert!(args.all);
    }

    #[test]
    fn parse_session_resume() {
        let cli = XaftCli::try_parse_from(["xaft", "session", "resume", "abc-123"]).unwrap();
        let Some(Commands::Session(cmd)) = cli.command else {
            panic!("expected Session")
        };
        let SessionSubcommand::Resume(args) = cmd.subcommand else {
            panic!()
        };
        assert_eq!(args.id, "abc-123");
    }

    #[test]
    fn session_export_and_inspect_reject_the_old_config_flag() {
        // `export`/`inspect` used to carry their own `--config`, which resolved
        // a `core.data_dir` the runtime had never been bootstrapped with — the
        // exact way the `xaft session` family could end up reading two
        // different databases. The store is injected now, so the flag must be
        // GONE rather than merely ignored: a user with the old flag in a script
        // has to be told, not silently have it do nothing.
        // clap echoes whichever spelling the user typed, so the diagnostic has
        // to name the token that was actually on the command line.
        for (argv, spelled) in [
            (
                ["xaft", "session", "export", "abc", "-c", "/tmp/xaft.toml"],
                "-c",
            ),
            (
                [
                    "xaft",
                    "session",
                    "inspect",
                    "abc",
                    "--config",
                    "/tmp/xaft.toml",
                ],
                "--config",
            ),
        ] {
            let err = XaftCli::try_parse_from(argv)
                .expect_err("`--config` must be rejected, not silently accepted and ignored");
            let rendered = err.to_string();
            assert!(
                rendered.contains(spelled),
                "clap must name the offending flag `{spelled}`, got: {rendered}"
            );
        }
    }

    #[test]
    fn session_export_still_parses_without_a_config_flag() {
        let cli = XaftCli::try_parse_from([
            "xaft",
            "session",
            "export",
            "abc-123",
            "-o",
            "/tmp/out.json",
        ])
        .unwrap();
        let Some(Commands::Session(cmd)) = cli.command else {
            panic!("expected Session")
        };
        let SessionSubcommand::Export(args) = cmd.subcommand else {
            panic!("expected Export")
        };
        assert_eq!(args.id, "abc-123");
        assert_eq!(args.output, Some(std::path::PathBuf::from("/tmp/out.json")));
    }

    #[test]
    fn parse_session_show() {
        let cli = XaftCli::try_parse_from(["xaft", "session", "show", "my-session"]).unwrap();
        let Some(Commands::Session(cmd)) = cli.command else {
            panic!("expected Session")
        };
        let SessionSubcommand::Show(args) = cmd.subcommand else {
            panic!()
        };
        assert_eq!(args.id, "my-session");
    }

    #[test]
    fn parse_completions_bash() {
        let cli = XaftCli::try_parse_from(["xaft", "completions", "bash"]).unwrap();
        let Some(Commands::Completions(args)) = cli.command else {
            panic!("expected Completions")
        };
        assert!(matches!(args.shell, ShellArg::Bash));
    }

    #[test]
    fn parse_completions_zsh() {
        let cli = XaftCli::try_parse_from(["xaft", "completions", "zsh"]).unwrap();
        let Some(Commands::Completions(args)) = cli.command else {
            panic!("expected Completions")
        };
        assert!(matches!(args.shell, ShellArg::Zsh));
    }

    #[test]
    fn parse_version() {
        let cli = XaftCli::try_parse_from(["xaft", "version"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Version(_))));
    }

    #[test]
    fn parse_version_json() {
        let cli = XaftCli::try_parse_from(["xaft", "version", "--json"]).unwrap();
        let Some(Commands::Version(args)) = cli.command else {
            panic!()
        };
        assert!(args.json);
    }

    #[test]
    fn run_alias_r_works() {
        let cli = XaftCli::try_parse_from(["xaft", "r", "task"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Run(_))));
    }

    #[test]
    fn session_alias_s_works() {
        let cli = XaftCli::try_parse_from(["xaft", "s", "list"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Session(_))));
    }

    #[test]
    fn cli_overrides_from_run_args() {
        let args = RunArgs {
            task: Some("test task".into()),
            model: Some("gpt-4o".into()),
            provider: None,
            agent: Some("code-review".into()),
            workflow: None,
            max_turns: Some(10),
            temperature: Some(0.5),
            r#continue: false,
            resume: None,
            session: None,
            config: None,
            project_dir: None,
            headless: true,
            json: false,
            dry_run: true,
            auto_approve: true,
            dangerously_skip_permissions: false,
            log_level: Some(LogLevelArg::Debug),
            no_telemetry: true,
        };
        let overrides = args.to_cli_overrides();
        assert_eq!(overrides.model.as_deref(), Some("gpt-4o"));
        assert_eq!(overrides.agent_preset.as_deref(), Some("code-review"));
        assert_eq!(overrides.max_turns, Some(10));
        assert!(overrides.headless);
        assert!(overrides.dry_run);
        assert!(overrides.auto_approve);
        assert!(overrides.no_telemetry);
        assert!(matches!(overrides.log_level, Some(LogLevel::Debug)));
    }

    #[test]
    fn parse_run_with_resume_long_flag() {
        let cli =
            XaftCli::try_parse_from(["xaft", "run", "task", "--resume", "session-abc"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.resume.as_deref(), Some("session-abc"));
    }

    #[test]
    fn parse_run_with_resume_short_flag() {
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "-r", "session-abc"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.resume.as_deref(), Some("session-abc"));
    }

    #[test]
    fn parse_run_session_alias_hidden() {
        // -s still works as a hidden alias
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "-s", "old-id"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.session.as_deref(), Some("old-id"));
    }

    #[test]
    fn resume_takes_precedence_in_cli_overrides() {
        let args = RunArgs {
            task: Some("t".into()),
            resume: Some("new-id".into()),
            session: Some("old-id".into()),
            ..make_test_run_args()
        };
        let overrides = args.to_cli_overrides();
        assert_eq!(overrides.session_id.as_deref(), Some("new-id"));
    }

    fn make_test_run_args() -> RunArgs {
        RunArgs {
            task: None,
            model: None,
            provider: None,
            agent: None,
            workflow: None,
            max_turns: None,
            temperature: None,
            r#continue: false,
            resume: None,
            session: None,
            config: None,
            project_dir: None,
            headless: true,
            json: false,
            dry_run: false,
            auto_approve: false,
            dangerously_skip_permissions: false,
            log_level: None,
            no_telemetry: false,
        }
    }
}

// ── Jobs command ─────────────────────────────────────────────────────────────────

/// Arguments for `xaft jobs <subcommand>`.
#[derive(Debug, Args)]
pub struct JobsCommand {
    /// Which background-session operation to perform.
    #[command(subcommand)]
    pub subcommand: JobsSubcommand,
}

/// Background-session management, mirroring agenthicc's `jobs` command group.
#[derive(Debug, Subcommand)]
pub enum JobsSubcommand {
    /// List current, archived, or recoverable-trash sessions.
    List(JobsListArgs),

    /// Show one background session.
    Status(JobsStatusArgs),

    /// Cancel a background session.
    Cancel {
        /// Session id.
        session_id: String,
    },

    /// Resume a cancelled, failed, orphaned, or archived background session.
    Resume {
        /// Session id.
        session_id: String,
    },

    /// Retry a failed background session.
    Retry {
        /// Session id.
        session_id: String,
    },

    /// Approve a waiting background session.
    Approve {
        /// Session id.
        session_id: String,
    },

    /// Reject a waiting background session.
    Reject {
        /// Session id.
        session_id: String,
    },

    /// Provide input to a waiting background session.
    Input {
        /// Session id.
        session_id: String,
        /// The value the job asked for.
        value: String,
    },

    /// Set a bounded, local-only display title.
    Rename {
        /// Session id.
        session_id: String,
        /// New display title.
        title: String,
    },

    /// Replace the comma-separated labels on a background session.
    ///
    /// Labels are user metadata and are never sent to providers.
    Labels {
        /// Session id.
        session_id: String,
        /// Comma-separated labels; pass an empty string to clear them.
        #[arg(default_value = "")]
        labels: String,
    },

    /// Archive a background session.
    Archive {
        /// Session id.
        session_id: String,
    },

    /// Move a background session to recoverable trash.
    Delete {
        /// Session id.
        session_id: String,
    },

    /// Restore a deleted background session.
    Restore {
        /// Session id.
        session_id: String,
    },

    /// Permanently remove expired background trash.
    Purge,
}

/// Arguments for `xaft jobs list`.
#[derive(Debug, Args)]
pub struct JobsListArgs {
    /// Include archived sessions.
    #[arg(long)]
    pub all: bool,
    /// Include sessions in recoverable trash.
    #[arg(long)]
    pub trash: bool,
    /// Output format.
    #[arg(long, value_enum, default_value = "pretty")]
    pub format: OutputFormat,
}

/// Arguments for `xaft jobs status`.
#[derive(Debug, Args)]
pub struct JobsStatusArgs {
    /// Session id to inspect.
    pub session_id: String,
    /// Output format.
    #[arg(long, value_enum, default_value = "pretty")]
    pub format: OutputFormat,
}

// ── Runs command ───────────────────────────────────────────────────────────────

/// Arguments for `xaft runs [subcommand]`.
///
/// `args_conflicts_with_subcommands` is what makes the flattened
/// `--status` / `--json` below safe to add. Without it, `xaft runs --status
/// running list` would be ambiguous: is `--status` the alias's filter or
/// `list`'s? With it, clap rejects the mixed form with a usage error while
/// leaving both pure forms alone. clap only raises that conflict when a parent
/// argument was matched *from the command line* — a defaulted one does not
/// count — so `xaft runs list` and `xaft runs list --status running` keep
/// working, because there `--status` is consumed by the subcommand and the
/// parent keeps its default.
///
/// The alternative design, a clap `default_subcommand`, was rejected: it
/// cannot forward arguments to the default subcommand, so the alias would go
/// back to accepting no flags at all.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct RunsCommand {
    /// Which goal-run operation to perform.
    ///
    /// Omitted means `list`: agenthicc registers `runs` *and* `runs list` as
    /// two names for the same command, so `xaft runs` must not fail with
    /// clap's "requires a subcommand" usage error. Use [`RunsCommand::resolve`]
    /// to get the effective subcommand.
    #[command(subcommand)]
    pub subcommand: Option<RunsSubcommand>,

    /// Only list runs in this status. Valid on the bare `xaft runs` alias.
    ///
    /// Flattened from [`RunsListArgs::status`] so the alias takes the same flag
    /// under the same name instead of a spelling of its own.
    #[arg(long, default_value = "")]
    pub status: String,

    /// Emit JSON instead of the column listing. Valid on the bare alias.
    ///
    /// Flattened from [`RunsListArgs::json`], for the same reason.
    #[arg(long)]
    pub json: bool,
}

impl RunsCommand {
    /// The subcommand to run, with the bare `xaft runs` form resolved to `list`.
    ///
    /// agenthicc's `agenthicc/cli/commands/runs.py` registers both
    /// `@command("runs")` and `@command("runs", "list")`, and the former's own
    /// docstring states that "`runs` is an alias of `runs list`". Resolving here
    /// rather than in the handler keeps the alias in one place and lets
    /// [`Self::subcommand`] stay an honest description of what was typed.
    ///
    /// A present `subcommand` is returned untouched. It can only be present
    /// when none of this command's own arguments were also given, per the
    /// `args_conflicts_with_subcommands` note on [`RunsCommand`].
    pub fn resolve(&self) -> RunsSubcommand {
        self.subcommand
            .clone()
            .unwrap_or_else(|| RunsSubcommand::List(self.alias_list_args()))
    }

    /// The [`RunsListArgs`] that a bare `xaft runs` dispatches on.
    ///
    /// Built from this command's own `--status` / `--json` rather than from
    /// [`RunsListArgs::default`], because the alias takes those flags. Routing
    /// both spellings through one struct is what makes "the bare form accepted
    /// `--status` and then ignored it" unrepresentable rather than merely
    /// untested; `RunsListArgs` is `PartialEq` so the equivalence can still be
    /// asserted outright, which
    /// `bare_and_explicit_list_resolve_to_identical_args` does.
    fn alias_list_args(&self) -> RunsListArgs {
        RunsListArgs {
            status: self.status.clone(),
            // `--format` belongs to the explicit form only; the alias exposes
            // `--json`, which `output_format` folds into this same choice.
            format: OutputFormat::Pretty,
            json: self.json,
        }
    }
}

/// Goal-run control plane, mirroring agenthicc's `runs` command group.
#[derive(Debug, Clone, Subcommand)]
pub enum RunsSubcommand {
    /// List durable goal runs.
    ///
    /// Also reachable as bare `xaft runs`, matching agenthicc where the bare
    /// group is an alias of `runs list`. Both forms take the same two flags
    /// under the same names; the bare form declares them on [`RunsCommand`]
    /// and [`RunsCommand::resolve`] folds them into this variant's payload,
    /// so there is only ever one definition of what `--status` and `--json`
    /// mean.
    List(RunsListArgs),

    /// Show one durable goal run.
    Show {
        /// Run id, as printed by `xaft runs list`.
        run_id: String,
        /// Emit JSON. Accepted for parity; the output is JSON either way.
        #[arg(long)]
        json: bool,
    },

    /// Cancel a goal run and its active main session.
    Cancel {
        /// Run id.
        run_id: String,
    },

    /// Resume a recoverable goal run.
    Resume {
        /// Run id.
        run_id: String,
    },
}

/// Arguments for `xaft runs list`.
///
/// This is also what the bare `xaft runs` alias dispatches on, built by
/// [`RunsCommand::alias_list_args`], so the alias cannot drift from the
/// explicit form: both spellings of every flag live on this one struct.
///
/// [`Default`] is kept because it is the shape `xaft runs list` parses to when
/// the user names no flags, and [`PartialEq`] because the whole point of the
/// alias is that the two forms are *the same* — a test asserts that rather
/// than assuming it.
#[derive(Debug, Clone, Default, PartialEq, Args)]
pub struct RunsListArgs {
    /// Only list runs in this status.
    ///
    /// An unrecognised status is a usage error rather than an empty list, so a
    /// typo in a script fails loudly.
    #[arg(long, default_value = "")]
    pub status: String,

    /// Output format.
    ///
    /// xaft's own spelling, and predates the bare alias. Kept working because
    /// `xaft runs list --format json` is already in scripts.
    #[arg(long, value_enum, default_value = "pretty")]
    pub format: OutputFormat,

    /// Emit JSON instead of the column listing.
    ///
    /// agenthicc's spelling for exactly this. `runs.py` declares `json: bool`
    /// on *both* `runs` and `runs list` — there is no format enum anywhere in
    /// that module — and xaft's `runs show` already spells its (decorative)
    /// `--json` the same way, so this is agenthicc's flag rather than a third
    /// way of saying it. See [`Self::output_format`] for how it combines with
    /// `--format`.
    #[arg(long)]
    pub json: bool,
}

impl RunsListArgs {
    /// The format to actually render.
    ///
    /// `--json` (agenthicc) and `--format` (xaft) are two spellings of one
    /// choice, so they are reconciled once here instead of at each call site —
    /// which is also what stops a new call site from honouring only one of
    /// them. `--json` wins: `--format pretty --json` states contradictory
    /// intent, and the JSON request is the more specific of the two.
    pub fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format
        }
    }
}

// ── Workflows command ──────────────────────────────────────────────────────

/// Arguments for `xaft workflows <subcommand>`.
#[derive(Debug, Args)]
pub struct WorkflowsCommand {
    /// Which workflow operation to perform.
    #[command(subcommand)]
    pub subcommand: WorkflowsSubcommand,
}

/// Workflow discovery and headless execution, mirroring agenthicc's `workflows`
/// command group.
#[derive(Debug, Subcommand)]
pub enum WorkflowsSubcommand {
    /// List every workflow xaft can run, with its phase topology.
    List {
        /// Emit the registry as JSON instead of a listing.
        #[arg(long)]
        json: bool,
    },

    /// Run one workflow headlessly for a single intent.
    Run {
        /// Workflow name, as printed by `xaft workflows list`.
        workflow_name: String,
        /// The intent to hand to the workflow.
        #[arg(long, default_value = "")]
        intent: String,
        /// Emit the outcome as JSON.
        #[arg(long)]
        json: bool,
        /// Config file to load.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },
}

// ── Mcp command ────────────────────────────────────────────────────────────────────────

/// Arguments for `xaft mcp <subcommand>`.
#[derive(Debug, Args)]
pub struct McpCommand {
    /// Mcp subcommand to execute.
    #[command(subcommand)]
    pub subcommand: McpSubcommand,
}

/// Subcommands for `xaft mcp`.
///
/// Only the seven config-only commands are present. agenthicc's `connect`,
/// `refresh` and `doctor` build a live `McpSessionManager` guarded by a
/// `NetworkGuard`, which xaft has no equivalent of, so those three stay absent
/// rather than being stubbed into commands that cannot work.
#[derive(Debug, Subcommand)]
pub enum McpSubcommand {
    /// List configured MCP servers.
    List(McpListArgs),

    /// Add an MCP server to configuration.
    Add(McpAddArgs),

    /// Show one configured MCP server.
    Get(McpGetArgs),

    /// Remove one configured MCP server.
    Remove(McpRemoveArgs),

    /// Explain that disconnects are session-scoped.
    ///
    /// Present because it is a real user question with a real answer, not
    /// because there is anything to disconnect from a one-shot CLI process.
    Disconnect(McpNameArgs),

    /// Report whether a server has environment-backed credentials.
    Auth(McpNameArgs),

    /// Explain that the CLI removes no stored credentials.
    Logout(McpNameArgs),
}

/// Which config file a subcommand should read and write.
#[derive(Debug, Args)]
pub struct McpScopeArgs {
    /// Use the user-global config (`~/.config/xaft/xaft.toml`).
    #[arg(long, conflicts_with = "project")]
    pub global: bool,

    /// Use the nearest project config, walking up from the current directory.
    #[arg(long, conflicts_with = "global")]
    pub project: bool,

    /// Path to a specific config file. Overrides `--global` and `--project`.
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// Arguments for `xaft mcp list`.
#[derive(Debug, Args)]
pub struct McpListArgs {
    /// Emit compact JSON instead of one line per server.
    #[arg(long)]
    pub json: bool,

    /// Which config file to read or write.
    #[command(flatten)]
    pub scope: McpScopeArgs,
}

/// Arguments for `xaft mcp add`.
#[derive(Debug, Args)]
pub struct McpAddArgs {
    /// Server name. Letters, numbers, `.`, `_` and `-` only.
    pub name: String,

    /// The command to run (for `stdio`) or the URL to dial.
    ///
    /// Stored in `command` for a `stdio` server and in `url` for anything
    /// else, because those are the two fields xaft's config actually has.
    #[arg(value_name = "COMMAND_OR_URL")]
    pub target: String,

    /// Transport to use.
    #[arg(long, default_value = "stdio")]
    pub transport: String,

    /// Argument for a `stdio` command. Repeat for more than one.
    #[arg(long = "arg", value_name = "ARG")]
    pub args: Vec<String>,

    /// Environment variable holding the bearer token.
    ///
    /// Stored as both `token = "${NAME}"` and `token_env = "NAME"`, so the
    /// reference and the variable name never disagree.
    #[arg(long, value_name = "ENV_NAME")]
    pub token_env: Option<String>,

    /// Do not connect at session start.
    #[arg(long)]
    pub no_auto_connect: bool,

    /// How many times to retry a dropped connection.
    #[arg(long, default_value_t = 3)]
    pub reconnect_attempts: u32,

    /// Seconds to wait between reconnect attempts.
    #[arg(long, default_value_t = 1.0)]
    pub reconnect_delay_seconds: f64,

    /// Which config file to read or write.
    #[command(flatten)]
    pub scope: McpScopeArgs,
}

/// Arguments for `xaft mcp get`.
#[derive(Debug, Args)]
pub struct McpGetArgs {
    /// Server name.
    pub name: String,

    /// Emit compact JSON instead of indented JSON.
    #[arg(long)]
    pub json: bool,

    /// Which config file to read or write.
    #[command(flatten)]
    pub scope: McpScopeArgs,
}

/// Arguments for `xaft mcp remove`.
#[derive(Debug, Args)]
pub struct McpRemoveArgs {
    /// Server name.
    pub name: String,

    /// Which config file to read or write.
    #[command(flatten)]
    pub scope: McpScopeArgs,
}

/// Arguments for the `mcp` subcommands that only need a server name.
#[derive(Debug, Args)]
pub struct McpNameArgs {
    /// Server name.
    pub name: String,
}
