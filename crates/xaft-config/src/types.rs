//! All configuration struct types for xaft.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ── XaftConfig (root) ─────────────────────────────────────────────────────────

/// Root configuration struct — all xaft settings in one place.
///
/// Loaded via [`ConfigLoader::load`](crate::loader::ConfigLoader::load) which
/// applies the full precedence chain: built-in defaults → user global →
/// project → session → env vars → CLI flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct XaftConfig {
    /// Core runtime settings.
    pub core: CoreConfig,
    /// Agent presets keyed by name. Key `"default"` is always present.
    pub agent: HashMap<String, AgentPreset>,
    /// Provider configurations keyed by name.
    pub provider: HashMap<String, ProviderConfig>,
    /// Tool configurations keyed by tool name.
    pub tool: HashMap<String, ToolConfig>,
    /// Guardrail configuration.
    pub guardrail: GuardrailConfig,
    /// MCP (Model Context Protocol) configuration.
    pub mcp: McpConfig,
    /// TUI configuration.
    pub tui: TuiConfig,
    /// Plugin system configuration.
    pub plugins: PluginConfig,
    /// Three-tier model routing configuration.
    pub model_tiers: ModelTierConfig,
    /// Memory system configuration.
    pub memory: MemoryConfig,
    /// F3 @-mention file-input configuration.
    pub mention: MentionConfig,
}

// ── ModelTierConfig ───────────────────────────────────────────────────────────

/// Three-tier model configuration for cost-aware routing.
///
/// All fields are optional; if unset, the default agent model is used for all tiers.
/// Environment variables override file config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelTierConfig {
    /// Model for complex reasoning: planner + QA.
    /// Env: `XAFT_FLAGSHIP_MODEL`
    pub flagship_model: Option<String>,
    /// Model for code generation: coder + fixer.
    /// Env: `XAFT_STANDARD_MODEL`
    pub standard_model: Option<String>,
    /// Model for lightweight tasks: summarizer.
    /// Env: `XAFT_FAST_MODEL`
    pub fast_model: Option<String>,
}

impl ModelTierConfig {
    /// Resolve each tier, falling back to `default_model` if not set.
    /// Env vars take priority over file config.
    pub fn resolve(&self, default_model: &str) -> ResolvedTiers {
        let flagship = std::env::var("XAFT_FLAGSHIP_MODEL")
            .ok()
            .or_else(|| self.flagship_model.clone())
            .unwrap_or_else(|| default_model.to_string());
        let standard = std::env::var("XAFT_STANDARD_MODEL")
            .ok()
            .or_else(|| self.standard_model.clone())
            .unwrap_or_else(|| default_model.to_string());
        let fast = std::env::var("XAFT_FAST_MODEL")
            .ok()
            .or_else(|| self.fast_model.clone())
            .unwrap_or_else(|| default_model.to_string());
        ResolvedTiers {
            flagship,
            standard,
            fast,
        }
    }
}

/// Resolved model names for each tier.
#[derive(Debug, Clone)]
pub struct ResolvedTiers {
    /// Model for complex reasoning (planner + QA).
    pub flagship: String,
    /// Model for code generation (coder + fixer).
    pub standard: String,
    /// Model for lightweight tasks (summarizer).
    pub fast: String,
}

impl ResolvedTiers {
    /// `true` when all three tiers use the same model (no routing needed).
    pub fn all_same(&self) -> bool {
        self.flagship == self.standard && self.standard == self.fast
    }
}

// ── CoreConfig ────────────────────────────────────────────────────────────────

/// Core xaft runtime settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreConfig {
    /// Minimum log level: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`.
    pub log_level: LogLevel,
    /// Base directory for session data. Defaults to `~/.xaft`.
    pub data_dir: PathBuf,
    /// Enable telemetry (anonymous usage statistics).
    pub telemetry: bool,
}

/// Log level enum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Very verbose trace output.
    Trace,
    /// Debug output.
    Debug,
    /// Informational messages (default).
    Info,
    /// Warnings only.
    Warn,
    /// Errors only.
    Error,
}

impl LogLevel {
    /// Return the tracing filter string for this level.
    pub fn as_filter(&self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

// ── MemoryConfig ──────────────────────────────────────────────────────────────

/// Memory system configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Whether the memory system is enabled.
    pub enabled: bool,
    /// Storage backend: `"sqlite"` or `"in_memory"`.
    pub backend: String,
    /// Auto-remember facts extracted from agent turns.
    pub auto_remember: bool,
    /// Auto-summarize old memories when the store grows large.
    pub auto_summarize: bool,
    /// Default to project-scoped memory (workspace scope).
    pub project_scope_default: bool,
    /// Maximum number of memories before auto-summarization triggers.
    pub max_entries: Option<usize>,
    /// Maximum search results returned by recall.
    pub max_search_results: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: "sqlite".into(),
            auto_remember: true,
            auto_summarize: true,
            project_scope_default: true,
            max_entries: Some(10_000),
            max_search_results: 10,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_filter())
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" | "err" => Ok(Self::Error),
            other => Err(format!("unknown log level: '{other}'")),
        }
    }
}

// ── AgentPreset ───────────────────────────────────────────────────────────────

/// A named agent configuration preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentPreset {
    /// LLM model identifier (e.g. `"claude-3-5-sonnet-20241022"`).
    pub model: String,
    /// Provider name (must match a key in `XaftConfig::provider`).
    pub provider: String,
    /// Custom system prompt. Empty string uses the built-in default.
    pub system_prompt: String,
    /// Maximum number of ReAct loop turns.
    pub max_turns: u32,
    /// Sampling temperature in `[0.0, 2.0]`.
    pub temperature: f32,
    /// Nucleus sampling parameter.
    pub top_p: f32,
    /// Stop sequences for LLM completion.
    pub stop_sequences: Vec<String>,
    /// Tool IDs or glob patterns this agent is allowed to use. `["*"]` = all.
    pub allowed_tools: Vec<String>,
    /// Tool IDs explicitly denied (even if in `allowed_tools`).
    pub denied_tools: Vec<String>,
}

/// A fully-resolved agent preset ready for use at runtime.
#[derive(Debug, Clone)]
pub struct ResolvedAgentPreset {
    /// Preset name.
    pub name: String,
    /// Resolved model ID (after alias expansion).
    pub model: String,
    /// Provider name.
    pub provider: String,
    /// System prompt.
    pub system_prompt: String,
    /// Max turns.
    pub max_turns: u32,
    /// Temperature.
    pub temperature: f32,
    /// Top-P.
    pub top_p: f32,
    /// Stop sequences.
    pub stop_sequences: Vec<String>,
    /// Allowed tool patterns.
    pub allowed_tools: Vec<String>,
    /// Denied tool patterns.
    pub denied_tools: Vec<String>,
}

impl ResolvedAgentPreset {
    /// Return `true` if the given tool ID is permitted by this preset.
    pub fn allows_tool(&self, tool_id: &str) -> bool {
        // Check denied list first
        for pattern in &self.denied_tools {
            if glob_matches(pattern, tool_id) {
                return false;
            }
        }
        // Then check allowed list
        for pattern in &self.allowed_tools {
            if glob_matches(pattern, tool_id) {
                return true;
            }
        }
        false
    }
}

/// Simple glob match: `*` matches any substring, `?` matches one character.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return pattern == text;
    }
    let mut pi = pattern.chars().peekable();
    let mut ti = text.chars().peekable();
    glob_match_inner(&mut pi, &mut ti)
}

fn glob_match_inner(
    pi: &mut std::iter::Peekable<std::str::Chars>,
    ti: &mut std::iter::Peekable<std::str::Chars>,
) -> bool {
    loop {
        match pi.peek() {
            None => return ti.peek().is_none(),
            Some('*') => {
                pi.next();
                // Try matching zero or more chars in text
                let remaining_pattern: String = pi.clone().collect();
                let remaining_text: String = ti.clone().collect();
                for i in 0..=remaining_text.len() {
                    let suffix = &remaining_text[i..];
                    let mut pp = remaining_pattern.chars().peekable();
                    let mut tp = suffix.chars().peekable();
                    if glob_match_inner(&mut pp, &mut tp) {
                        return true;
                    }
                }
                return false;
            }
            Some('?') => {
                pi.next();
                if ti.next().is_none() {
                    return false;
                }
            }
            Some(&p) => {
                pi.next();
                match ti.next() {
                    Some(t) if t == p => {}
                    _ => return false,
                }
            }
        }
    }
}

// ── ProviderConfig ────────────────────────────────────────────────────────────

/// Configuration for an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Provider type.
    #[serde(rename = "type")]
    pub provider_type: ProviderType,
    /// API key (prefer `api_key_env` or env var over embedding here).
    pub api_key: String,
    /// Read API key from this environment variable name.
    pub api_key_env: Option<String>,
    /// Provider API base URL.
    pub base_url: String,
    /// OpenAI organization ID (OpenAI only).
    pub organization: String,
    /// Maximum retry attempts for failed requests.
    pub max_retries: u32,
    /// Request timeout in seconds.
    pub timeout_secs: u64,
    /// Extra HTTP headers.
    pub headers: HashMap<String, String>,
    /// Rate limit: requests per minute.
    pub rpm_limit: Option<u32>,
    /// Rate limit: tokens per minute.
    pub tpm_limit: Option<u32>,
    /// Model alias map: `short_name → full_model_id`.
    pub models: HashMap<String, String>,
}

/// Provider backend type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    /// Anthropic Claude API.
    Anthropic,
    /// OpenAI API.
    Openai,
    /// Any OpenAI-compatible API (Ollama, Together, etc.).
    OpenaiCompatible,
}

impl ProviderConfig {
    /// Resolve the actual API key, checking `api_key_env` before `api_key`.
    pub fn resolve_api_key(&self, provider_name: &str) -> Option<String> {
        // 1. Explicit env var override
        if let Some(ref env_var) = self.api_key_env {
            if let Ok(val) = std::env::var(env_var) {
                return Some(val);
            }
        }
        // 2. Inline api_key (may have been set from env during loading)
        if !self.api_key.is_empty() {
            return Some(self.api_key.clone());
        }
        // 3. Conventional env var: XAFT_ANTHROPIC_API_KEY, XAFT_OPENAI_API_KEY, etc.
        let env_name = format!(
            "XAFT_{}_API_KEY",
            provider_name.to_uppercase().replace('-', "_")
        );
        std::env::var(&env_name).ok()
    }

    /// Resolve a model name through the alias table.
    pub fn resolve_model(&self, model: &str) -> String {
        self.models
            .get(model)
            .cloned()
            .unwrap_or_else(|| model.to_string())
    }
}

// ── ToolConfig ────────────────────────────────────────────────────────────────

/// Generic tool configuration (common fields + extra).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolConfig {
    /// Whether the tool is enabled.
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// Extra tool-specific settings.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Typed config for the `shell` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellToolConfig {
    /// Whether the tool is enabled.
    pub enabled: bool,
    /// Shell executable path.
    pub shell: String,
    /// Command timeout in seconds.
    pub timeout_secs: u64,
    /// Command prefixes that are blocked.
    pub blocked_commands: Vec<String>,
    /// Maximum output size before truncation (bytes).
    pub max_output_bytes: usize,
}

/// Typed config for the `file-read` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileReadToolConfig {
    /// Whether the tool is enabled.
    pub enabled: bool,
    /// Maximum file size (human format e.g. "10MB").
    pub max_file_size: String,
    /// Maximum line length before truncation.
    pub max_line_length: usize,
}

/// Typed config for the `file-edit` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileEditToolConfig {
    /// Whether the tool is enabled.
    pub enabled: bool,
    /// Require user confirmation before writing.
    pub confirm_on_write: bool,
    /// Maximum concurrent file edits.
    pub max_concurrent_edits: usize,
}

/// Typed config for the `grep` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GrepToolConfig {
    /// Whether the tool is enabled.
    pub enabled: bool,
    /// Search engine: `"ripgrep"` or `"git-grep"`.
    pub engine: String,
    /// Maximum search results.
    pub max_results: usize,
}

// ── GuardrailConfig ───────────────────────────────────────────────────────────

/// Configuration for safety guardrails.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardrailConfig {
    /// Block destructive file operations.
    pub file_destruction: bool,
    /// Detect and redact secrets in tool outputs.
    pub secret_leakage: bool,
    /// Enforce token/cost limits.
    pub cost_limit: bool,
    /// Require approval before shell commands.
    pub command_approval: bool,
    /// Cost limit configuration.
    pub cost_limit_config: CostLimitConfig,
    /// Secret leakage detection configuration.
    pub secret_leakage_config: SecretLeakageConfig,
}

impl GuardrailConfig {
    /// Return `true` if cost limit guardrail is configured and active.
    pub fn cost_limit_enabled(&self) -> bool {
        self.cost_limit
    }
}

/// Cost limit guardrail settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CostLimitConfig {
    /// Maximum spend per session in USD.
    pub max_spend: f64,
    /// Maximum tokens per single LLM request.
    pub max_tokens_per_request: u64,
    /// Warn when this percentage of the limit is reached.
    pub warn_at_percent: u8,
}

/// Secret leakage detection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretLeakageConfig {
    /// Regex patterns to detect as secrets.
    pub patterns: Vec<String>,
    /// Action on detection: `"block"`, `"redact"`, or `"warn"`.
    pub action: SecretAction,
}

/// Action to take when a secret is detected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SecretAction {
    /// Block the output entirely.
    Block,
    /// Replace with `[REDACTED]`.
    Redact,
    /// Log a warning.
    Warn,
}

// ── McpConfig ─────────────────────────────────────────────────────────────────

/// MCP (Model Context Protocol) configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct McpConfig {
    /// MCP server settings.
    pub server: McpServerConfig,
    /// MCP client connections.
    pub client: Vec<McpClientConfig>,
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// Enable the MCP server.
    pub enabled: bool,
    /// Transport type: `"http+sse"` or `"websocket"`.
    pub transport: String,
    /// Bind host.
    pub host: String,
    /// Bind port.
    pub port: u16,
    /// Tool filter.
    pub tools: McpToolFilter,
}

/// MCP tool filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpToolFilter {
    /// Include patterns.
    pub include: Vec<String>,
    /// Exclude patterns.
    pub exclude: Vec<String>,
}

/// MCP client connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpClientConfig {
    /// Connection name.
    pub name: String,
    /// Transport: `"stdio"` or `"http+sse"`.
    pub transport: String,
    /// Command to execute (for stdio transport).
    pub command: Option<String>,
    /// Arguments (for stdio transport).
    #[serde(default)]
    pub args: Vec<String>,
    /// URL (for http+sse transport).
    pub url: Option<String>,
    /// Whether this client is active.
    #[serde(default = "bool_true")]
    pub enabled: bool,
}

// ── TuiConfig ─────────────────────────────────────────────────────────────────

/// TUI appearance and behavior settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    /// Color theme.
    pub theme: TuiTheme,
    /// Enable mouse support.
    pub mouse: bool,
    /// Show timestamps in conversation.
    pub timestamps: bool,
    /// Max visible conversation lines.
    pub conversation_height: u16,
    /// Keybinding configuration.
    pub keybindings: KeybindingConfig,
    /// Layout proportions.
    pub layout: TuiLayoutConfig,
    /// Preserve TUI content in terminal after exit (like Claude Code / lazygit).
    /// When true, the final frame is replayed to stdout so it stays in scrollback.
    pub preserve_output_on_exit: bool,
    /// Use the alternate screen buffer. When false, renders directly to the
    /// primary screen (content stays in scrollback naturally).
    pub use_alternate_screen: bool,
    /// When using alternate screen with `preserve_output_on_exit`, replay the
    /// final frame buffer to stdout after leaving the alternate screen.
    pub persist_final_frame: bool,
    /// Show a session summary footer (tokens, cost, elapsed time) on exit.
    pub show_exit_summary: bool,
}

/// TUI color theme.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TuiTheme {
    /// Dark background (default).
    Dark,
    /// Light background.
    Light,
    /// Solarized.
    Solarized,
}

/// TUI layout configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiLayoutConfig {
    /// Conversation pane width (0–100 proportional).
    pub conversation_width: u8,
    /// Sidebar pane width (0–100 proportional).
    pub sidebar_width: u8,
    /// Sidebar panel ordering.
    pub sidebar_panels: Vec<SidebarPanel>,
    /// File diff panel height (lines).
    pub file_diff_height: u16,
}

/// Available sidebar panels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SidebarPanel {
    /// Task tree showing plan steps.
    TaskTree,
    /// File diff viewer.
    FileDiff,
    /// Tool call log.
    Tools,
    /// Structured log viewer.
    Logs,
}

/// Keybinding configuration: raw key strings → action names.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeybindingConfig {
    /// Map of `"key_string"` → `KeyAction`.
    #[serde(flatten)]
    pub bindings: HashMap<String, KeyAction>,
}

/// A key action: single action name or sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeyAction {
    /// Single action name.
    Single(String),
    /// Ordered list of action names.
    Sequence(Vec<String>),
}

impl KeyAction {
    /// Return the primary action name.
    pub fn action_name(&self) -> &str {
        match self {
            Self::Single(s) => s.as_str(),
            Self::Sequence(v) => v.first().map(|s| s.as_str()).unwrap_or(""),
        }
    }
}

/// TUI layout state persisted across sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiLayoutState {
    /// Conversation width (proportional).
    pub conversation_width: u8,
    /// Sidebar width (proportional).
    pub sidebar_width: u8,
    /// Sidebar panel order.
    pub sidebar_panels: Vec<SidebarPanel>,
    /// File diff height (lines).
    pub file_diff_height: u16,
    /// Scroll positions per panel.
    pub scroll_positions: HashMap<String, u16>,
    /// Currently focused panel.
    pub focused_panel: FocusedPanel,
    /// Input box height.
    pub input_height: u16,
}

/// Which panel has focus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FocusedPanel {
    /// Main conversation pane.
    Conversation,
    /// Sidebar.
    Sidebar,
    /// Text input area.
    Input,
}

// ── PluginConfig ──────────────────────────────────────────────────────────────

/// Plugin system configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PluginConfig {
    /// Extra plugin search directories.
    pub search_paths: Vec<PathBuf>,
    /// Allow loading dynamic (dylib) plugins.
    pub allow_dynamic: bool,
    /// Allow WASM plugins (experimental).
    pub allow_wasm: bool,
    /// Plugin security settings.
    pub security: PluginSecurityConfig,
}

/// Plugin security settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSecurityConfig {
    /// Require signed plugins.
    pub require_signature: bool,
    /// Capabilities allowed for unsigned plugins.
    pub unsigned_capabilities: Vec<String>,
}

// ── CLI overrides ─────────────────────────────────────────────────────────────

/// CLI flag overrides passed from the command line.
///
/// All fields are `Option` — only set values override the config.
/// Created by `xaft-cli` from clap-parsed args and passed to `ConfigLoader`.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    /// Override the default agent model.
    pub model: Option<String>,
    /// Override the default provider.
    pub provider: Option<String>,
    /// Select a named agent preset.
    pub agent_preset: Option<String>,
    /// Override max turns.
    pub max_turns: Option<u32>,
    /// Override sampling temperature.
    pub temperature: Option<f32>,
    /// Path to an explicit config file (skips project discovery).
    pub config_file: Option<PathBuf>,
    /// Session ID to resume.
    pub session_id: Option<String>,
    /// Override the project directory.
    pub project_dir: Option<PathBuf>,
    /// Override log level.
    pub log_level: Option<LogLevel>,
    /// Disable telemetry.
    pub no_telemetry: bool,
    /// Auto-approve all guardrails (equivalent to `-y`).
    pub auto_approve: bool,
    /// Run in headless mode (no TUI).
    pub headless: bool,
    /// Show plan without executing.
    pub dry_run: bool,
}

// ── Helper fns ────────────────────────────────────────────────────────────────

fn bool_true() -> bool {
    true
}

// ── MentionConfig ─────────────────────────────────────────────────────────────

/// Configuration for the F3 @-mention file-input feature.
///
/// All fields are optional; if unset, sensible defaults are applied. The
/// mention feature itself is always enabled — the only opt-out is to not
/// type `@<path>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MentionConfig {
    /// Maximum lines included in a single text-file `FileRef` before the
    /// resolver truncates and appends a `(truncated, N more lines)` note.
    pub max_inline_lines: usize,
    /// Maximum bytes included in a single text-file `FileRef` body. Applied
    /// after the line cap; whichever hits first wins.
    pub max_inline_bytes: usize,
    /// Maximum bytes of an image file. Images larger than this are
    /// downgraded to a path-only text reference with a warning.
    pub image_max_bytes: usize,
    /// Hard cap on the size of any single file the resolver will read. Files
    /// larger than this are rejected with a size warning (not truncated).
    pub resolver_max_file_bytes: usize,
    /// Deduplicate identical `FileRef` blocks within a single submission.
    /// When `true`, only the first occurrence is kept; subsequent
    /// references are replaced with literal text.
    pub dedupe: bool,
    /// Policy for handling workspace-escape mentions (parent traversal,
    /// absolute paths, home expansion).
    pub escape_policy: EscapePolicy,
    /// Allowlist of glob patterns matched against the canonicalised absolute
    /// path of escape mentions. When `escape_policy = "always"`, escape
    /// mentions matching one of these patterns are attached silently
    /// (no dialog). Empty by default.
    pub escape_allowlist: Vec<String>,
}

/// How the TUI handles workspace-escape mentions (paths that resolve to
/// locations outside the user's working directory).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EscapePolicy {
    /// Show a per-submission confirmation dialog. User must approve
    /// (or approve-for-session) before the file is attached. **Default.**
    Confirm,
    /// Silently attach every escape mention. Use with care — the
    /// `[mention].escape_allowlist` patterns can carve out a narrower
    /// subset that still shows the dialog.
    Always,
    /// Reject every escape mention with a `⚠ Workspace escape disabled`
    /// warning. The literal token is preserved in the user message but
    /// the file is never attached. Matches the v0.1 default behaviour.
    Never,
}

impl Default for MentionConfig {
    fn default() -> Self {
        Self {
            max_inline_lines: 2_000,
            max_inline_bytes: 50_000,
            image_max_bytes: 10_000_000,
            resolver_max_file_bytes: 100_000_000,
            dedupe: false,
            escape_policy: EscapePolicy::Confirm,
            escape_allowlist: Vec::new(),
        }
    }
}

impl Default for EscapePolicy {
    fn default() -> Self {
        EscapePolicy::Confirm
    }
}
