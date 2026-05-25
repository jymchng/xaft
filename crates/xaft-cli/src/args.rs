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

    // ── Execution parameters ──────────────────────────────────────────────────
    /// Override the maximum number of agent turns.
    #[arg(long)]
    pub max_turns: Option<u32>,

    /// Override the sampling temperature (0.0 = deterministic, 2.0 = creative).
    #[arg(long)]
    pub temperature: Option<f32>,

    // ── Session management ────────────────────────────────────────────────────
    /// Resume a specific session by ID instead of starting a new one.
    ///
    /// Picks up where the previous session left off, including conversation
    /// history and pending file edits.
    #[arg(long, short = 's', value_name = "SESSION_ID")]
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
            session_id: self.session.clone(),
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
        let cli = XaftCli::try_parse_from(["xaft", "run", "task", "-s", "session-abc"]).unwrap();
        let Some(Commands::Run(args)) = cli.command else {
            panic!("expected Run")
        };
        assert_eq!(args.session.as_deref(), Some("session-abc"));
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
            max_turns: Some(10),
            temperature: Some(0.5),
            session: None,
            config: None,
            project_dir: None,
            headless: true,
            json: false,
            dry_run: true,
            auto_approve: true,
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
}
