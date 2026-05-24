# XAFT Configuration System — Product Requirements Document

> **Status**: Draft v0.1  
> **Last Updated**: 2025-03-04  
> **Authors**: xaft core team  
> **Scope**: xaft.toml format, environment variable overrides, CLI flag overrides, hierarchical loading, agent presets, tool configuration, provider configuration, keybinding configuration, TUI layout persistence

---

## 1. Overview

xaft's behavior must be configurable at multiple levels: global defaults, per-project settings, per-session overrides, and runtime CLI flags. This PRD defines the configuration system that governs how xaft loads, merges, and applies configuration across all these layers.

### 1.1 Goals

| # | Goal | Metric |
|---|------|--------|
| G1 | Predictable configuration precedence | Clear, documented override order with no ambiguity |
| G2 | Zero-cost defaults | No allocation if no config file exists; all defaults are const |
| G3 | Hot-reload for project config | Watch `xaft.toml`; apply changes without restart |
| G4 | Validation at load time | Fail fast with actionable error messages |
| G5 | Type-safe access | All config values accessed through typed Rust structs |

### 1.2 Non-Goals

- A GUI configuration editor (TUI-only)
- Configuration migration across major versions (breaking changes require manual update)
- Encrypted secret storage (delegate to system keychain / env vars)
- Configuration templating or inheritance across projects

---

## 2. Configuration Hierarchy

### 2.1 Precedence Order (highest wins)

```
  Priority    Source                          Location
  ────────    ──────────────────────────      ──────────────────────────────
  1 (highest) CLI flags                       Command-line arguments
  2           Environment variables           XAFT_* prefix
  3           Session overrides               ~/.xaft/sessions/<id>/config.toml
  4           Project config                  <project>/.xaft/xaft.toml
  5           User global config              ~/.xaft/xaft.toml
  6 (lowest)  Built-in defaults               Hardcoded in binary
```

### 2.2 Merge Strategy

```
  ┌───────────────┐   ┌───────────────┐   ┌───────────────┐
  │  Built-in      │   │  User Global  │   │  Project      │
  │  Defaults      │   │  ~/.xaft/     │   │  .xaft/       │
  │                │   │  xaft.toml    │   │  xaft.toml    │
  └───────┬───────┘   └───────┬───────┘   └───────┬───────┘
          │                   │                   │
          └───────────┬───────┴───────────┬───────┘
                      │   Deep Merge      │
                      ▼                   │
              ┌───────────────┐           │
              │  Merged Base  │           │
              └───────┬───────┘           │
                      │                   │
                      ▼                   │
              ┌───────────────┐           │
              │  Session      │           │
              │  Overrides    │◄──────────┘
              └───────┬───────┘
                      │
                      ▼
              ┌───────────────┐
              │  Env Vars     │
              │  (XAFT_*)     │
              └───────┬───────┘
                      │
                      ▼
              ┌───────────────┐
              │  CLI Flags    │
              └───────┬───────┘
                      │
                      ▼
              ┌───────────────┐
              │  Final Config │
              └───────────────┘
```

### 2.3 Deep Merge Rules

| Type | Merge Behavior |
|------|---------------|
| Scalar (string, int, bool) | Higher-priority value wins; full replacement |
| Array | Replace entirely (no concatenation) |
| Table/Map | Recursive deep merge: each key merged independently |
| `Option<T>` | `Some` from higher priority overrides `None`; `None` does not clear `Some` |
| Nested table | Merge recursively, preserving keys not present in override |

```rust
impl ConfigValue {
    fn deep_merge(base: &mut serde_json::Value, override_val: &serde_json::Value) {
        match (base, override_val) {
            (serde_json::Value::Object(base_map), serde_json::Value::Object(over_map)) => {
                for (key, over_val) in over_map {
                    let entry = base_map.entry(key.clone()).or_insert(serde_json::Value::Null);
                    Self::deep_merge(entry, over_val);
                }
            }
            (base, over) => *base = over.clone(),  // Full replacement for scalars and arrays
        }
    }
}
```

---

## 3. `xaft.toml` Format

### 3.1 Complete Schema

```toml
# ═══════════════════════════════════════════
# XAFT Configuration File
# ═══════════════════════════════════════════

[core]
# Minimum log level for the xaft process
log_level = "info"               # "trace" | "debug" | "info" | "warn" | "error"
# Directory for session data (default: ~/.xaft/sessions)
data_dir = "~/.xaft"
# Disable telemetry
telemetry = true

# ═══════════════════════════════════════════
# Agent Presets
# ═══════════════════════════════════════════

[agent.default]
model = "claude-3.5-sonnet"
provider = "anthropic"
system_prompt = ""                # Override default system prompt
max_turns = 25
temperature = 0.0
top_p = 1.0
stop_sequences = []
# Tools this agent is allowed to use (glob patterns)
allowed_tools = ["*"]
# Tools this agent is forbidden from using
denied_tools = []

[agent.code-review]
model = "gpt-4o"
provider = "openai"
system_prompt = """You are a code review agent. Focus on:
- Security vulnerabilities
- Performance issues
- Code style consistency"""
max_turns = 15
temperature = 0.2
allowed_tools = ["com.xaft.file-read", "com.xaft.grep", "com.xaft.shell"]

[agent.refactor]
model = "claude-3.5-sonnet"
provider = "anthropic"
max_turns = 40
temperature = 0.0
allowed_tools = ["com.xaft.*"]
denied_tools = ["com.xaft.shell"]

# ═══════════════════════════════════════════
# Provider Configuration
# ═══════════════════════════════════════════

[provider.anthropic]
type = "anthropic"
api_key = ""                      # Prefer XAFT_ANTHROPIC_API_KEY env var
base_url = "https://api.anthropic.com"
max_retries = 3
timeout_secs = 120
# Rate limiting
rpm_limit = 50                    # Requests per minute
tpm_limit = 100000                # Tokens per minute

[provider.openai]
type = "openai"
api_key = ""                      # Prefer XAFT_OPENAI_API_KEY env var
base_url = "https://api.openai.com/v1"
organization = ""
max_retries = 3
timeout_secs = 120

[provider.ollama]
type = "openai-compatible"
base_url = "http://localhost:11434/v1"
api_key = "ollama"                # Some compatible servers need a dummy key
max_retries = 1
timeout_secs = 300

[provider.custom]
type = "openai-compatible"
base_url = "https://llm.internal.corp/v1"
api_key_env = "CORP_LLM_KEY"     # Read API key from this env var
headers = { "X-Custom-Auth" = "Bearer ${CORP_TOKEN}" }
max_retries = 3
timeout_secs = 120

# ═══════════════════════════════════════════
# Tool Configuration
# ═══════════════════════════════════════════

[tool.file-read]
enabled = true
max_file_size = "10MB"
max_line_length = 2000

[tool.file-edit]
enabled = true
# Require confirmation before writing files
confirm_on_write = false
# Maximum number of concurrent file edits
max_concurrent_edits = 5

[tool.shell]
enabled = true
# Shell executable
shell = "/bin/bash"
# Timeout for shell commands
timeout_secs = 300
# Blocked commands (matched by prefix)
blocked_commands = ["rm -rf /", "mkfs", "dd if=/dev/zero"]
# Maximum output size before truncation
max_output_bytes = 65536

[tool.grep]
enabled = true
# Default search tool: "ripgrep" | "git-grep"
engine = "ripgrep"
# Max results per search
max_results = 100

[tool.web-fetch]
enabled = false                   # Disabled by default (network capability)
timeout_secs = 30
max_response_bytes = 1048576

# ═══════════════════════════════════════════
# Guardrail Configuration
# ═══════════════════════════════════════════

[guardrail]
# Enable/disable specific guardrails
file_destruction = true           # Block rm -rf, force push, etc.
secret_leakage = true             # Detect API keys in tool outputs
cost_limit = true                 # Enforce token/cost limits
command_approval = false          # Require user approval for shell commands

[guardrail.cost_limit]
# Maximum total spend per session (USD)
max_spend = 10.0
# Maximum tokens per single request
max_tokens_per_request = 100000
# Warn when approaching limit
warn_at_percent = 80

[guardrail.secret_leakage]
# Patterns to detect
patterns = [
    "sk-[a-zA-Z0-9]{48}",          # OpenAI keys
    "sk-ant-[a-zA-Z0-9-]{95}",     # Anthropic keys
    "ghp_[a-zA-Z0-9]{36}",         # GitHub PATs
    "AKIA[0-9A-Z]{16}",            # AWS access keys
]
# Action: "block" | "redact" | "warn"
action = "redact"

# ═══════════════════════════════════════════
# MCP Configuration
# ═══════════════════════════════════════════

[mcp.server]
enabled = false
transport = "http+sse"
host = "127.0.0.1"
port = 3001

[mcp.server.tools]
include = ["*"]
exclude = ["com.xaft.internal.*"]

[[mcp.client]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
enabled = true

# ═══════════════════════════════════════════
# TUI Configuration
# ═══════════════════════════════════════════

[tui]
# Theme: "dark" | "light" | "solarized" | custom theme name
theme = "dark"
# Mouse support
mouse = true
# Display timestamps in conversation
timestamps = true
# Max visible conversation lines before scroll
conversation_height = 40

[tui.keybindings]
# Format: "key = action" or "key = [action, action]"
# Modifier prefixes: ctrl-, alt-, shift-
# Special keys: enter, esc, tab, backspace, up, down, left, right,
#               pageup, pagedown, home, end, f1-f12

# Navigation
"ctrl+n" = "new_task"
"ctrl+q" = "quit"
"ctrl+s" = "stop_agent"
"ctrl+r" = "resume_agent"
"ctrl+p" = "command_palette"

# Panels
"ctrl+1" = "focus_conversation"
"ctrl+2" = "focus_task_tree"
"ctrl+3" = "focus_file_diff"
"ctrl+4" = "focus_tools"

# Scrolling
"ctrl+up" = "scroll_up"
"ctrl+down" = "scroll_down"
"pageup" = "page_up"
"pagedown" = "page_down"

# Editing
"ctrl+e" = "toggle_edit_mode"
"ctrl+z" = "undo"
"ctrl+y" = "redo"

# Agent interaction
"ctrl+space" = "interrupt_agent"
"ctrl+enter" = "submit_input"
"alt+enter" = "newline_in_input"

[tui.layout]
# Panel sizes (proportional, sum = 100)
conversation_width = 60
sidebar_width = 40
# Sidebar panels (top to bottom)
sidebar_panels = ["task_tree", "file_diff", "tools"]
# File diff panel height (lines)
file_diff_height = 15

# ═══════════════════════════════════════════
# Plugin Configuration
# ═══════════════════════════════════════════

[plugins]
# Additional plugin search paths
search_paths = []
# Allow dynamic plugin loading
allow_dynamic = true
# Allow WASM plugins (experimental)
allow_wasm = false

[plugins.security]
# Require signed plugins
require_signature = false
# Allowed capability sets for unsigned plugins
unsigned_capabilities = ["fs_read", "fs_write", "shell"]
```

---

## 4. Configuration Loading

### 4.1 Typed Configuration Structs

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct XaftConfig {
    pub core: CoreConfig,
    #[serde(default)]
    pub agent: HashMap<String, AgentPreset>,
    #[serde(default)]
    pub provider: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub tool: HashMap<String, ToolConfig>,
    #[serde(default)]
    pub guardrail: GuardrailConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub plugins: PluginConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoreConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_telemetry")]
    pub telemetry: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentPreset {
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_top_p")]
    pub top_p: f32,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    #[serde(default = "default_allowed_tools")]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub r#type: ProviderType,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    pub base_url: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub rpm_limit: Option<u32>,
    #[serde(default)]
    pub tpm_limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    Anthropic,
    Openai,
    OpenaiCompatible,
}
```

### 4.2 Configuration Loader

```rust
pub struct ConfigLoader;

impl ConfigLoader {
    /// Load configuration with full precedence chain.
    pub fn load(cli_overrides: &CliOverrides) -> Result<XaftConfig, ConfigError> {
        // Step 1: Built-in defaults (zero allocation, all const)
        let mut config = XaftConfig::defaults();

        // Step 2: User global config
        let global_path = dirs::config_dir()
            .unwrap_or_default()
            .join("xaft/xaft.toml");
        if global_path.exists() {
            let global = Self::load_toml(&global_path)?;
            Self::merge(&mut config, global);
        }

        // Step 3: Project config
        let project_path = Self::find_project_config()?;
        if let Some(path) = project_path {
            let project = Self::load_toml(&path)?;
            Self::merge(&mut config, project);
        }

        // Step 4: Session overrides
        if let Some(session_id) = &cli_overrides.session_id {
            let session_path = dirs::config_dir()
                .unwrap_or_default()
                .join(format!("xaft/sessions/{}/config.toml", session_id));
            if session_path.exists() {
                let session = Self::load_toml(&session_path)?;
                Self::merge(&mut config, session);
            }
        }

        // Step 5: Environment variable overrides
        Self::apply_env_overrides(&mut config)?;

        // Step 6: CLI flag overrides
        Self::apply_cli_overrides(&mut config, cli_overrides);

        // Step 7: Validate
        Self::validate(&config)?;

        Ok(config)
    }

    fn load_toml(path: &Path) -> Result<XaftConfig, ConfigError> {
        let content = fs::read_to_string(path)
            .map_err(|e| ConfigError::Io { path: path.to_path_buf(), source: e })?;

        let raw: toml::Value = toml::from_str(&content)
            .map_err(|e| ConfigError::Parse { path: path.to_path_buf(), source: e })?;

        let config: XaftConfig = raw.clone().try_into()
            .map_err(|e| ConfigError::Validation { path: path.to_path_buf(), source: e.to_string() })?;

        Ok(config)
    }

    fn find_project_config() -> Result<Option<PathBuf>, ConfigError> {
        let mut dir = std::env::current_dir()?;
        loop {
            let config_path = dir.join(".xaft/xaft.toml");
            if config_path.exists() {
                return Ok(Some(config_path));
            }
            if !dir.pop() {
                return Ok(None);
            }
        }
    }
}
```

---

## 5. Environment Variable Overrides

### 5.1 Variable Naming Convention

| Pattern | Example | Maps To |
|---------|---------|---------|
| `XAFT_<SECTION>__<KEY>` | `XAFT_CORE__LOG_LEVEL` | `core.log_level` |
| `XAFT_PROVIDER_<NAME>__<KEY>` | `XAFT_PROVIDER_ANTHROPIC__API_KEY` | `provider.anthropic.api_key` |
| `XAFT_AGENT_<NAME>__<KEY>` | `XAFT_AGENT_DEFAULT__MODEL` | `agent.default.model` |
| `XAFT_TOOL_<NAME>__<KEY>` | `XAFT_TOOL_SHELL__TIMEOUT_SECS` | `tool.shell.timeout_secs` |
| `XAFT_TUI__<KEY>` | `XAFT_TUI__THEME` | `tui.theme` |

### 5.2 Implementation

```rust
impl ConfigLoader {
    fn apply_env_overrides(config: &mut XaftConfig) -> Result<(), ConfigError> {
        // Core overrides
        if let Ok(val) = env::var("XAFT_CORE__LOG_LEVEL") {
            config.core.log_level = val;
        }
        if let Ok(val) = env::var("XAFT_CORE__DATA_DIR") {
            config.core.data_dir = PathBuf::from(val);
        }
        if let Ok(val) = env::var("XAFT_CORE__TELEMETRY") {
            config.core.telemetry = val.parse().map_err(|_| ConfigError::EnvParse {
                var: "XAFT_CORE__TELEMETRY".to_string(),
                expected: "boolean",
            })?;
        }

        // Provider overrides
        for (name, provider) in &mut config.provider {
            let prefix = format!("XAFT_PROVIDER_{}", name.to_uppercase().replace('-', "_"));
            if let Ok(val) = env::var(format!("{}__API_KEY", prefix)) {
                provider.api_key = val;
            }
            if let Ok(val) = env::var(format!("{}__BASE_URL", prefix)) {
                provider.base_url = val;
            }
        }

        // Agent overrides
        for (name, agent) in &mut config.agent {
            let prefix = format!("XAFT_AGENT_{}", name.to_uppercase().replace('-', "_"));
            if let Ok(val) = env::var(format!("{}__MODEL", prefix)) {
                agent.model = val;
            }
            if let Ok(val) = env::var(format!("{}__PROVIDER", prefix)) {
                agent.provider = val;
            }
        }

        // Common shorthand: XAFT_MODEL overrides the default agent model
        if let Ok(val) = env::var("XAFT_MODEL") {
            if let Some(default_agent) = config.agent.get_mut("default") {
                default_agent.model = val;
            }
        }

        // Common shorthand: XAFT_API_KEY sets API key for the default provider
        if let Ok(val) = env::var("XAFT_API_KEY") {
            if let Some(default_provider) = config.provider.values_mut().next() {
                default_provider.api_key = val;
            }
        }

        Ok(())
    }
}
```

### 5.3 Variable Interpolation

Config values support `${ENV_VAR}` interpolation:

```rust
/// Resolve ${VAR} references in string config values.
fn interpolate_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            *s = interpolate_env(s);
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                interpolate_strings(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                interpolate_strings(v);
            }
        }
        _ => {}
    }
}

fn interpolate_env(s: &str) -> String {
    let re = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
    re.replace_all(s, |caps: &regex::Captures| {
        let var = &caps[1];
        env::var(var).unwrap_or_else(|_| format!("${{{}}}", var))
    }).into_owned()
}
```

---

## 6. CLI Flag Overrides

### 6.1 CLI Structure

```
xaft [OPTIONS] [TASK]

Options:
  --model <MODEL>              Override default agent model
  --provider <PROVIDER>        Override default provider
  --agent <PRESET>             Use a named agent preset
  --max-turns <N>              Override max agent turns
  --temperature <T>            Override agent temperature
  --config <PATH>              Use specific config file
  --session <ID>               Resume specific session
  --project-dir <DIR>          Override project directory
  --log-level <LEVEL>          Override log level
  --no-telemetry               Disable telemetry
  --tui / --no-tui             Enable/disable TUI
  --dry-run                    Show plan without executing
  --yes / -y                   Auto-approve all guardrails
```

### 6.2 CLI Override Application

```rust
#[derive(Debug, Parser)]
pub struct CliOverrides {
    pub task: Option<String>,

    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub max_turns: Option<u32>,
    #[arg(long)]
    pub temperature: Option<f32>,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub project_dir: Option<PathBuf>,
    #[arg(long)]
    pub log_level: Option<String>,
    #[arg(long)]
    pub no_telemetry: bool,
    #[arg(long)]
    pub tui: Option<bool>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

impl ConfigLoader {
    fn apply_cli_overrides(config: &mut XaftConfig, cli: &CliOverrides) {
        // Apply to default agent (or the --agent specified preset)
        let agent_name = cli.agent.as_deref().unwrap_or("default");
        if let Some(agent) = config.agent.get_mut(agent_name) {
            if let Some(model) = &cli.model {
                agent.model = model.clone();
            }
            if let Some(provider) = &cli.provider {
                agent.provider = provider.clone();
            }
            if let Some(max_turns) = cli.max_turns {
                agent.max_turns = max_turns;
            }
            if let Some(temperature) = cli.temperature {
                agent.temperature = temperature;
            }
        }

        if let Some(log_level) = &cli.log_level {
            config.core.log_level = log_level.clone();
        }

        if cli.no_telemetry {
            config.core.telemetry = false;
        }

        if cli.yes {
            config.guardrail.command_approval = false;
            config.guardrail.file_destruction = false;
            config.tool.file_edit.confirm_on_write = false;
        }
    }
}
```

---

## 7. Agent Presets

### 7.1 Built-in Presets

| Preset | Model | Max Turns | Temperature | Key Differences |
|--------|-------|-----------|-------------|-----------------|
| `default` | claude-3.5-sonnet | 25 | 0.0 | Full tool access |
| `code-review` | gpt-4o | 15 | 0.2 | Read-only tools, no shell |
| `refactor` | claude-3.5-sonnet | 40 | 0.0 | No shell, extended turns |
| `debug` | claude-3.5-sonnet | 50 | 0.3 | Shell allowed, extended turns |
| `docs` | gpt-4o | 20 | 0.5 | Creative temperature, file-edit only |

### 7.2 Preset Resolution

```rust
pub struct AgentPresetResolver;

impl AgentPresetResolver {
    /// Resolve the active agent preset from configuration.
    pub fn resolve(config: &XaftConfig, requested: Option<&str>) -> Result<ResolvedAgentPreset, ConfigError> {
        let preset_name = requested.unwrap_or("default");
        let preset = config.agent.get(preset_name)
            .ok_or(ConfigError::UnknownPreset { name: preset_name.to_string() })?
            .clone();

        // Validate that the provider exists
        config.provider.get(&preset.provider)
            .ok_or(ConfigError::UnknownProvider { name: preset.provider.clone() })?;

        // Validate that the model is served by the provider
        // (deferred to runtime — provider may not be reachable at config load time)

        // Resolve tool access lists
        let allowed = Self::resolve_tool_patterns(&preset.allowed_tools, config)?;
        let denied = Self::resolve_tool_patterns(&preset.denied_tools, config)?;

        Ok(ResolvedAgentPreset {
            name: preset_name.to_string(),
            model: preset.model,
            provider: preset.provider,
            system_prompt: preset.system_prompt,
            max_turns: preset.max_turns,
            temperature: preset.temperature,
            top_p: preset.top_p,
            stop_sequences: preset.stop_sequences,
            allowed_tools: allowed,
            denied_tools: denied,
        })
    }

    fn resolve_tool_patterns(patterns: &[String], config: &XaftConfig) -> Result<Vec<String>, ConfigError> {
        // If patterns contain "*", expand to all known tool IDs
        if patterns.iter().any(|p| p == "*") {
            return Ok(vec!["*".to_string()]);
        }

        let mut resolved = Vec::new();
        for pattern in patterns {
            if pattern.contains('*') {
                // Glob pattern — will be matched at runtime against PluginRegistry
                resolved.push(pattern.clone());
            } else {
                // Exact tool ID — validate existence in tool config
                if !config.tool.contains_key(pattern) && !pattern.starts_with("com.xaft.") {
                    // Warning, not error — plugin tools may not be in static config
                    tracing::warn!("Unknown tool ID in preset: {}", pattern);
                }
                resolved.push(pattern.clone());
            }
        }
        Ok(resolved)
    }
}
```

---

## 8. Tool Configuration

### 8.1 Tool Config Schema

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ToolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}
```

### 8.2 Type-Safe Tool Config Access

Each built-in tool defines a typed config struct:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ShellToolConfig {
    pub enabled: bool,
    #[serde(default = "default_shell")]
    pub shell: String,
    #[serde(default = "default_shell_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub blocked_commands: Vec<String>,
    #[serde(default = "default_max_output")]
    pub max_output_bytes: usize,
}

impl ShellToolConfig {
    pub fn from_tool_config(config: &ToolConfig) -> Result<Self, ConfigError> {
        let json = serde_json::to_value(config)?;
        serde_json::from_value(json)
            .map_err(|e| ConfigError::ToolConfig { tool: "shell".to_string(), source: e })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEditToolConfig {
    pub enabled: bool,
    #[serde(default)]
    pub confirm_on_write: bool,
    #[serde(default = "default_max_concurrent_edits")]
    pub max_concurrent_edits: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileReadToolConfig {
    pub enabled: bool,
    #[serde(default = "default_max_file_size")]
    pub max_file_size: String,   // Parsed with human-friendly units: "10MB"
    #[serde(default = "default_max_line_length")]
    pub max_line_length: usize,
}
```

### 8.3 Human-Friendly Size Parsing

```rust
pub fn parse_size(s: &str) -> Result<u64, ConfigError> {
    let s = s.trim().to_uppercase();
    let (num_part, multiplier) = if s.ends_with("GB") {
        (&s[..s.len()-2], 1_073_741_824u64)
    } else if s.ends_with("MB") {
        (&s[..s.len()-2], 1_048_576u64)
    } else if s.ends_with("KB") {
        (&s[..s.len()-2], 1024u64)
    } else if s.ends_with("B") {
        (&s[..s.len()-1], 1u64)
    } else {
        (s.as_str(), 1u64)
    };

    let num: f64 = num_part.trim().parse()
        .map_err(|_| ConfigError::InvalidSize(s.to_string()))?;

    Ok((num * multiplier as f64) as u64)
}
```

---

## 9. Provider Configuration

### 9.1 Provider Resolution

```rust
pub struct ProviderResolver;

impl ProviderResolver {
    /// Build a concrete Provider from configuration.
    pub fn resolve(
        name: &str,
        config: &ProviderConfig,
        plugin_registry: &PluginRegistry,
    ) -> Result<Arc<dyn Provider>, ConfigError> {
        // 1. Check if a plugin provides this
        if let Some(plugin_provider) = plugin_registry.get_provider(name) {
            return Ok(plugin_provider);
        }

        // 2. Resolve API key
        let api_key = if !config.api_key.is_empty() {
            config.api_key.clone()
        } else if let Some(env_var) = &config.api_key_env {
            env::var(env_var).map_err(|_| ConfigError::MissingApiKey {
                provider: name.to_string(),
                env_var: env_var.clone(),
            })?
        } else {
            // Fallback to conventional env var name
            let env_var = format!("XAFT_{}_API_KEY", name.to_uppercase().replace('-', "_"));
            env::var(&env_var).map_err(|_| ConfigError::MissingApiKey {
                provider: name.to_string(),
                env_var,
            })?
        };

        // 3. Construct the provider
        match config.r#type {
            ProviderType::Anthropic => {
                Ok(Arc::new(AnthropicProvider::new(api_key, config.clone())?) as Arc<dyn Provider>)
            }
            ProviderType::Openai => {
                Ok(Arc::new(OpenaiProvider::new(api_key, config.clone())?) as Arc<dyn Provider>)
            }
            ProviderType::OpenaiCompatible => {
                Ok(Arc::new(OpenaiCompatibleProvider::new(api_key, config.clone())?) as Arc<dyn Provider>)
            }
        }
    }
}
```

### 9.2 Model Alias Resolution

```toml
# Users can define model aliases for convenience
[provider.anthropic.models]
"sonnet" = "claude-3.5-sonnet-20241022"
"haiku" = "claude-3.5-haiku-20241022"
"opus" = "claude-3-opus-20240229"

[provider.openai.models]
"4o" = "gpt-4o"
"4o-mini" = "gpt-4o-mini"
"o1" = "o1"
```

```rust
impl ProviderResolver {
    fn resolve_model_alias(model: &str, config: &ProviderConfig) -> String {
        if let Some(alias) = config.models.get(model) {
            alias.clone()
        } else {
            model.to_string()
        }
    }
}
```

---

## 10. Keybinding Configuration

### 10.1 Keybinding Data Model

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct KeybindingConfig {
    #[serde(flatten)]
    pub bindings: HashMap<String, KeyAction>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum KeyAction {
    Single(String),              // "quit"
    Sequence(Vec<String>),       // ["save", "close"]
}

impl KeybindingConfig {
    /// Parse a keybinding string like "ctrl+shift+k" into a crossterm KeyEvent.
    pub fn parse_key(s: &str) -> Result<KeyEvent, KeyParseError> {
        let parts: Vec<&str> = s.split('+').collect();
        let mut modifiers = KeyModifiers::NONE;
        let mut key_code = None;

        for part in parts {
            match part.to_lowercase().as_str() {
                "ctrl" => modifiers.insert(KeyModifiers::CONTROL),
                "alt" => modifiers.insert(KeyModifiers::ALT),
                "shift" => modifiers.insert(KeyModifiers::SHIFT),
                "enter" => key_code = Some(KeyCode::Enter),
                "esc" => key_code = Some(KeyCode::Esc),
                "tab" => key_code = Some(KeyCode::Tab),
                "backspace" => key_code = Some(KeyCode::Backspace),
                "up" => key_code = Some(KeyCode::Up),
                "down" => key_code = Some(KeyCode::Down),
                "left" => key_code = Some(KeyCode::Left),
                "right" => key_code = Some(KeyCode::Right),
                "pageup" => key_code = Some(KeyCode::PageUp),
                "pagedown" => key_code = Some(KeyCode::PageDown),
                "home" => key_code = Some(KeyCode::Home),
                "end" => key_code = Some(KeyCode::End),
                "space" => key_code = Some(KeyCode::Char(' ')),
                f if f.starts_with('f') && f[1..].parse::<u8>().ok().map(|n| n >= 1 && n <= 12).unwrap_or(false) => {
                    key_code = Some(KeyCode::F(f[1..].parse().unwrap()));
                }
                c if c.len() == 1 => key_code = Some(KeyCode::Char(c.chars().next().unwrap())),
                _ => return Err(KeyParseError::UnknownKey(part.to_string())),
            };
        }

        let code = key_code.ok_or(KeyParseError::NoKeyCode)?;
        Ok(KeyEvent::new(code, modifiers))
    }
}
```

### 10.2 Keybinding Registry

```rust
pub struct KeybindingRegistry {
    bindings: HashMap<KeyEvent, KeyAction>,
    reverse: HashMap<String, Vec<KeyEvent>>,  // action → keys
}

impl KeybindingRegistry {
    pub fn from_config(config: &KeybindingConfig) -> Result<Self, KeyParseError> {
        let mut bindings = HashMap::new();
        let mut reverse = HashMap::new();

        for (key_str, action) in &config.bindings {
            let key_event = KeybindingConfig::parse_key(key_str)?;

            bindings.insert(key_event, action.clone());
            reverse.entry(action.action_name().to_string())
                .or_insert_with(Vec::new)
                .push(key_event);
        }

        Ok(Self { bindings, reverse })
    }

    /// Look up the action for a key event.
    pub fn lookup(&self, key: &KeyEvent) -> Option<&KeyAction> {
        self.bindings.get(key)
    }

    /// Get all keys bound to a given action.
    pub fn keys_for_action(&self, action: &str) -> &[KeyEvent] {
        self.reverse.get(action).map(|v| v.as_slice()).unwrap_or(&[])
    }
}
```

---

## 11. TUI Layout Persistence

### 11.1 Layout State

The TUI layout (panel sizes, scroll positions, focus) is persisted per session so users don't need to reconfigure their view every time they reconnect.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiLayoutState {
    pub conversation_width: u16,
    pub sidebar_width: u16,
    pub sidebar_panels: Vec<SidebarPanel>,
    pub file_diff_height: u16,
    pub scroll_positions: HashMap<String, u16>,
    pub focused_panel: FocusedPanel,
    pub input_height: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SidebarPanel {
    TaskTree,
    FileDiff,
    Tools,
    Logs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FocusedPanel {
    Conversation,
    Sidebar,
    Input,
}
```

### 11.2 Layout Persistence

```rust
pub struct TuiLayoutPersistence {
    path: PathBuf,
    state: TuiLayoutState,
    dirty: bool,
}

impl TuiLayoutPersistence {
    pub fn load_or_default(session_id: &SessionId) -> Self {
        let path = dirs::config_dir()
            .unwrap_or_default()
            .join(format!("xaft/sessions/{}/tui-layout.toml", session_id));

        let state = if path.exists() {
            let content = fs::read_to_string(&path).ok();
            content.and_then(|c| toml::from_str(&c).ok())
                .unwrap_or_default()
        } else {
            TuiLayoutState::default()
        };

        Self { path, state, dirty: false }
    }

    pub fn update<F: FnOnce(&mut TuiLayoutState)>(&mut self, f: F) {
        f(&mut self.state);
        self.dirty = true;
    }

    /// Persist layout state to disk. Called on checkpoint and on graceful shutdown.
    pub fn persist(&mut self) -> Result<(), io::Error> {
        if !self.dirty {
            return Ok(());
        }
        let content = toml::to_string_pretty(&self.state)?;
        fs::write(&self.path, content)?;
        self.dirty = false;
        Ok(())
    }
}
```

### 11.3 Debounced Auto-Save

```rust
pub fn spawn_layout_saver(
    persistence: Arc<Mutex<TuiLayoutPersistence>>,
    mut rx: tokio::sync::watch::Receiver<TuiLayoutState>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_save = Instant::now();
        let debounce = Duration::from_secs(5);

        loop {
            if rx.changed().await.is_err() {
                break;
            }

            let now = Instant::now();
            if now.duration_since(last_save) < debounce {
                tokio::time::sleep_until(last_save + debounce).await;
            }

            let mut p = persistence.lock().await;
            p.persist().ok();
            last_save = Instant::now();
        }
    })
}
```

---

## 12. Validation

### 12.1 Validation Rules

```rust
impl ConfigLoader {
    fn validate(config: &XaftConfig) -> Result<(), ConfigError> {
        // 1. Agent presets must reference valid providers
        for (name, agent) in &config.agent {
            if !config.provider.contains_key(&agent.provider) {
                return Err(ConfigError::Validation {
                    section: format!("agent.{}", name),
                    message: format!("provider '{}' not found", agent.provider),
                });
            }
        }

        // 2. Temperature must be in [0.0, 2.0]
        for (name, agent) in &config.agent {
            if agent.temperature < 0.0 || agent.temperature > 2.0 {
                return Err(ConfigError::Validation {
                    section: format!("agent.{}", name),
                    message: format!("temperature {} out of range [0.0, 2.0]", agent.temperature),
                });
            }
        }

        // 3. Max turns must be positive
        for (name, agent) in &config.agent {
            if agent.max_turns == 0 {
                return Err(ConfigError::Validation {
                    section: format!("agent.{}", name),
                    message: "max_turns must be > 0".to_string(),
                });
            }
        }

        // 4. Guardrail cost limit must be positive if enabled
        if config.guardrail.cost_limit_enabled() {
            if config.guardrail.cost_limit.max_spend <= 0.0 {
                return Err(ConfigError::Validation {
                    section: "guardrail.cost_limit".to_string(),
                    message: "max_spend must be > 0".to_string(),
                });
            }
        }

        // 5. Tool configs: parse and validate typed configs
        for (name, tool_config) in &config.tool {
            Self::validate_tool_config(name, tool_config)?;
        }

        // 6. TUI keybindings: parse all key strings
        for (key_str, _action) in &config.tui.keybindings.bindings {
            KeybindingConfig::parse_key(key_str)
                .map_err(|e| ConfigError::Validation {
                    section: "tui.keybindings".to_string(),
                    message: format!("invalid key '{}': {}", key_str, e),
                })?;
        }

        // 7. Layout values must be reasonable
        if config.tui.layout.conversation_width + config.tui.layout.sidebar_width != 100 {
            return Err(ConfigError::Validation {
                section: "tui.layout".to_string(),
                message: "conversation_width + sidebar_width must equal 100".to_string(),
            });
        }

        Ok(())
    }
}
```

### 12.2 Validation Error Format

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error reading {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },

    #[error("parse error in {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },

    #[error("validation error in [{section}]: {message}")]
    Validation { section: String, message: String },

    #[error("unknown agent preset: {name}")]
    UnknownPreset { name: String },

    #[error("unknown provider: {name}")]
    UnknownProvider { name: String },

    #[error("missing API key for provider '{provider}': set {env_var}")]
    MissingApiKey { provider: String, env_var: String },

    #[error("tool config error for '{tool}': {source}")]
    ToolConfig { tool: String, source: serde_json::Error },

    #[error("environment variable parse error: {var} expected {expected}")]
    EnvParse { var: String, expected: String },

    #[error("invalid size string: {0}")]
    InvalidSize(String),
}
```

---

## 13. Hot-Reload

### 13.1 File Watcher

```rust
pub struct ConfigWatcher {
    paths: Vec<PathBuf>,
    last_modified: HashMap<PathBuf, SystemTime>,
    tx: watch::Sender<XaftConfig>,
}

impl ConfigWatcher {
    pub fn spawn(
        config: XaftConfig,
        paths: Vec<PathBuf>,
    ) -> (watch::Receiver<XaftConfig>, JoinHandle<()>) {
        let (tx, rx) = watch::channel(config);
        let mut watcher = Self {
            paths,
            last_modified: HashMap::new(),
            tx,
        };

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                if watcher.check_for_changes() {
                    if let Ok(new_config) = ConfigLoader::load(&CliOverrides::default()) {
                        let _ = watcher.tx.send(new_config);
                    }
                }
            }
        });

        (rx, handle)
    }

    fn check_for_changes(&mut self) -> bool {
        let mut changed = false;
        for path in &self.paths {
            if let Ok(metadata) = fs::metadata(path) {
                if let Ok(modified) = metadata.modified() {
                    let prev = self.last_modified.insert(path.clone(), modified);
                    if prev != Some(modified) && prev.is_some() {
                        tracing::info!("Config file changed: {}", path.display());
                        changed = true;
                    }
                }
            }
        }
        changed
    }
}
```

### 13.2 Hot-Reload Application

```rust
impl Session {
    pub async fn watch_config_changes(&self, mut rx: watch::Receiver<XaftConfig>) {
        while rx.changed().await.is_ok() {
            let new_config = rx.borrow().clone();

            tracing::info!("Applying hot-reloaded configuration");

            // Apply safe-to-reload settings
            self.agent.update_temperature(new_config.agent.get("default").unwrap().temperature);
            self.agent.update_max_turns(new_config.agent.get("default").unwrap().max_turns);

            // Settings that require restart are logged but not applied
            if self.config().provider != new_config.provider {
                tracing::warn!("Provider changes require session restart to take effect");
            }

            self.set_config(new_config);
        }
    }
}
```

---

## 14. Testing Strategy

| Level | Test | Approach |
|-------|------|----------|
| Unit | TOML parsing | Valid + invalid fixtures |
| Unit | Env var override | Set env, load, assert override |
| Unit | CLI flag override | Parse args, apply, assert |
| Unit | Deep merge | Test all type combinations |
| Integration | Full hierarchy | Create config files at each level, verify merge |
| Integration | Hot-reload | Write new config, verify applied |
| Property | Validation fuzz | Generate random configs, ensure no panics |
| E2E | Config-driven agent | Run agent with preset, verify behavior matches config |

---

## 15. Milestones

| Phase | Deliverable | Timeline |
|-------|-------------|----------|
| P1 | `xaft.toml` schema + typed structs + defaults | Week 1 |
| P2 | Config loader with hierarchical merge | Week 2 |
| P3 | Env var + CLI flag overrides | Week 3 |
| P4 | Agent presets + provider resolution | Week 4 |
| P5 | Tool config + validation | Week 5 |
| P6 | Keybinding config + TUI layout persistence | Week 6 |
| P7 | Hot-reload + file watcher | Week 7 |

---

## 16. Open Questions

1. **Secret management**: Should xaft integrate with OS keychains (keychain on macOS, libsecret on Linux) instead of requiring API keys in env vars or config files?
2. **Config profiles**: Should we support named config profiles (e.g., `xaft --profile work`) that swap entire configuration sets?
3. **Validation strictness**: Should unknown keys in `xaft.toml` be errors or warnings? Strict mode helps catch typos but breaks forward compatibility.
4. **Remote config**: Should xaft support loading config from a remote URL (e.g., corporate policy server)?
5. **Config diffing**: When multiple config layers override the same value, should xaft log the full resolution chain for debugging?
