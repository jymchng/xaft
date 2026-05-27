# Configuration Reference

This document provides a comprehensive reference for every configuration type, field, default value, and validation rule in the xaft configuration system. All configuration is expressed in TOML format and loaded through the six-layer precedence model described in the [Configuration Overview](01-overview.md).

## XaftConfig (Top-Level)

The `XaftConfig` struct is the root of the configuration hierarchy. It contains all subsystem configurations as named fields.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `core` | `CoreConfig` | `Default::default()` | Core runtime settings |
| `agent` | `HashMap<String, AgentPreset>` | `{}` (empty) | Named agent presets |
| `provider` | `HashMap<String, ProviderConfig>` | `{}` (empty) | Named provider configurations |
| `tool` | `HashMap<String, ToolConfig>` | `{}` (empty) | Per-tool configurations |
| `guardrail` | `GuardrailConfig` | `Default::default()` | Safety and approval settings |
| `mcp` | `McpConfig` | `Default::default()` | MCP server/client settings |
| `tui` | `TuiConfig` | `Default::default()` | TUI appearance and behavior |
| `plugins` | `PluginConfig` | `Default::default()` | Plugin loading and security |

## CoreConfig

The `CoreConfig` struct contains fundamental runtime settings that affect all subsystems.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `log_level` | `LogLevel` | `Info` | Minimum log level for the runtime |
| `data_dir` | `String` | `~/.xaft` | Directory for persistent data (sessions, worktrees, cache) |
| `telemetry` | `TelemetryConfig` | `Default::default()` | Telemetry and analytics settings |

### LogLevel

The `LogLevel` enum mirrors the standard Rust log levels:

| Variant | Description |
|---------|-------------|
| `Trace` | Extremely verbose output; includes all internal state transitions |
| `Debug` | Detailed output; includes tool parameters and LLM request/response summaries |
| `Info` | Standard output; includes session lifecycle events and phase transitions |
| `Warn` | Warning output; includes guardrail triggers and near-limit conditions |
| `Error` | Error output only; includes only failures and exceptions |

### TelemetryConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Whether to send anonymous usage telemetry |
| `endpoint` | `String` | `https://telemetry.xaft.dev` | Telemetry server endpoint |
| `session_metrics` | `bool` | `true` | Whether to include session duration and token counts |

## AgentPreset

The `AgentPreset` struct defines a named configuration for an agent, including which model to use, how to behave, and which tools are available. Agent presets are stored in a `HashMap<String, AgentPreset>` keyed by preset name (e.g., `"planner"`, `"coder"`, `"qa"`).

| Field | Type | Default | Validation | Description |
|-------|------|---------|------------|-------------|
| `model` | `String` | `"claude-3-opus-20240229"` | Non-empty | Model identifier or alias |
| `provider` | `String` | `"anthropic"` | Must reference a key in `provider` map | Provider to route LLM calls through |
| `system_prompt` | `String` | `""` | — | System prompt for the agent |
| `max_turns` | `u32` | `25` | `> 0` | Maximum number of agent-tool interaction turns |
| `temperature` | `f64` | `0.7` | `0.0 ≤ temperature ≤ 2.0` | Sampling temperature |
| `top_p` | `f64` | `1.0` | `0.0 ≤ top_p ≤ 1.0` | Nucleus sampling threshold |
| `stop_sequences` | `Vec<String>` | `[]` | — | Sequences that cause the model to stop generating |
| `allowed_tools` | `Vec<String>` | `[]` (all allowed) | — | Whitelist of tool names the agent may use |
| `denied_tools` | `Vec<String>` | `[]` (none denied) | — | Blacklist of tool names the agent may not use |
| `phase_hint` | `Option<WorkflowPhase>` | `None` | — | Explicit phase override for TUI display |

When `allowed_tools` is empty, the agent may use all available tools. When it is non-empty, only the listed tools are available. The `denied_tools` list is applied after `allowed_tools`, so a tool that appears in both lists will be denied. This allows a configuration pattern where `allowed_tools` grants a broad set of capabilities and `denied_tools` selectively removes dangerous ones.

### AgentPresetResolver

The `AgentPresetResolver` resolves a requested agent preset name into a fully-qualified `ResolvedAgentPreset`. The resolution process:

1. **Lookup**: Find the preset by name in the `agent` HashMap. If not found, return an error.
2. **Provider Validation**: Verify that the preset's `provider` field references a key in the `provider` HashMap. If not found, return an error.
3. **Model Alias Resolution**: If the `model` field is an alias (e.g., `"opus"` instead of `"claude-3-opus-20240229"`), resolve it using the provider's `models.aliases` map. If the alias is not found, use the raw model string.
4. **Default Inheritance**: Fill in any missing optional fields with sensible defaults derived from the provider type.

## ProviderConfig

The `ProviderConfig` struct defines a connection to an LLM API provider. Providers are stored in a `HashMap<String, ProviderConfig>` keyed by provider name (e.g., `"anthropic"`, `"openai"`, `"custom"`).

| Field | Type | Default | Validation | Description |
|-------|------|---------|------------|-------------|
| `provider_type` | `ProviderType` | Required | Must be valid enum variant | Provider implementation type |
| `api_key` | `Option<String>` | `None` | — | Direct API key value |
| `api_key_env` | `Option<String>` | `None` | — | Environment variable name containing the API key |
| `base_url` | `Option<String>` | Provider-specific | Non-empty if present | Custom API endpoint URL |
| `organization` | `Option<String>` | `None` | — | Organization ID for multi-tenant APIs |
| `max_retries` | `u32` | `3` | `> 0` | Maximum number of retry attempts for transient failures |
| `timeout_secs` | `u64` | `120` | `> 0` | Request timeout in seconds |
| `headers` | `HashMap<String, String>` | `{}` | — | Custom HTTP headers to include in every request |
| `rpm_limit` | `Option<u32>` | `None` | `> 0` if present | Maximum requests per minute |
| `tpm_limit` | `Option<u32>` | `None` | `> 0` if present | Maximum tokens per minute |
| `models` | `ModelsConfig` | `Default::default()` | — | Model aliases and overrides |

### ProviderType Enum

| Variant | Description |
|---------|-------------|
| `Anthropic` | Anthropic Claude API (default base URL: `https://api.anthropic.com`) |
| `Openai` | OpenAI API (default base URL: `https://api.openai.com`) |
| `OpenaiCompatible` | Any OpenAI-compatible API (e.g., Together, Groq, local LLMs). Requires `base_url`. |

### ModelsConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `aliases` | `HashMap<String, String>` | `{}` | Short name → full model ID mapping |

The `aliases` map allows configurations to use short model names (e.g., `"opus"`, `"sonnet"`, `"gpt4"`) that are resolved to full model identifiers at runtime. This makes configurations more readable and allows model upgrades to be performed in a single place.

## GuardrailConfig

The `GuardrailConfig` struct defines safety boundaries that prevent the agent from causing unintended damage.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `file_destruction` | `FileDestructionConfig` | `Default::default()` | Controls for file deletion and overwriting |
| `secret_leakage` | `SecretLeakageConfig` | `Default::default()` | Controls for secret detection and prevention |
| `cost_limit` | `CostLimitConfig` | `Default::default()` | Cost limits for API usage |
| `command_approval` | `CommandApprovalConfig` | `Default::default()` | Shell command approval rules |

### CostLimitConfig

| Field | Type | Default | Validation | Description |
|-------|------|---------|------------|-------------|
| `enabled` | `bool` | `true` | — | Whether cost limits are enforced |
| `per_session_usd` | `Option<f64>` | `10.0` | `> 0.0` if present | Maximum cost per session in USD |
| `per_task_usd` | `Option<f64>` | `5.0` | `> 0.0` if present | Maximum cost per task in USD |
| `warning_threshold_pct` | `u8` | `80` | `1-99` | Percentage of limit at which a warning is emitted |

### SecretLeakageConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Whether secret leakage detection is active |
| `patterns` | `Vec<String>` | Built-in regex patterns | Custom regex patterns for secret detection |
| `block_on_detection` | `bool` | `true` | Whether to block tool output containing secrets |

### CommandApprovalConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_policy` | `ApprovalPolicy` | `RequireApproval` | Default policy for unlisted commands |
| `auto_approve` | `Vec<String>` | `["ls", "cat", "head", "tail", "pwd", "echo"]` | Glob patterns for auto-approved commands |
| `require_approval` | `Vec<String>` | `["rm *", "sudo *", "chmod *", "curl *\\| *", "wget *"]` | Glob patterns for commands requiring approval |
| `deny` | `Vec<String>` | `["rm -rf /", "mkfs.*", "dd *"]` | Glob patterns for permanently denied commands |

## McpConfig

The `McpConfig` struct configures the Model Context Protocol integration, which allows xaft to connect to external tool servers and expose its own capabilities to MCP clients.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `server` | `McpServerConfig` | `Default::default()` | Configuration for xaft acting as an MCP server |
| `client` | `Vec<McpClientConfig>` | `[]` | List of MCP servers to connect to as a client |

### McpServerConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Whether to start the MCP server |
| `transport` | `TransportType` | `Stdio` | Transport mechanism (Stdio, Tcp, UnixSocket) |
| `port` | `u16` | `3000` | TCP port (when transport is Tcp) |
| `socket_path` | `String` | `/tmp/xaft-mcp.sock` | Unix socket path (when transport is UnixSocket) |
| `allowed_tools` | `Vec<String>` | `[]` (all tools) | Tools exposed to MCP clients |

### McpClientConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | `String` | Required | Human-readable name for the MCP server |
| `command` | `String` | Required | Command to start the MCP server process |
| `args` | `Vec<String>` | `[]` | Arguments to pass to the command |
| `env` | `HashMap<String, String>` | `{}` | Environment variables for the server process |
| `transport` | `TransportType` | `Stdio` | Transport mechanism for communicating with the server |

## TuiConfig

The `TuiConfig` struct controls the appearance and behavior of the terminal user interface.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `theme` | `Theme` | `Dark` | Color theme (Dark, Light, Solarized, or custom) |
| `mouse` | `bool` | `true` | Whether mouse events are processed |
| `timestamps` | `bool` | `true` | Whether to show timestamps in conversation |
| `conversation_height` | `u16` | `0` (auto) | Fixed height for conversation pane (0 = auto) |
| `keybindings` | `KeybindingsConfig` | `Default::default()` | Custom keybinding overrides |
| `layout` | `LayoutConfig` | `Default::default()` | Panel size proportions |

### KeybindingsConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `submit` | `String` | `"Enter"` | Key to submit input |
| `newline` | `String` | `"Shift+Enter"` | Key to insert a newline |
| `cancel` | `String` | `"Ctrl+C"` | Key to cancel current operation |
| `quit` | `String` | `"Ctrl+Q"` | Key to quit the application |
| `focus_next` | `String` | `"Tab"` | Key to cycle focus to next pane |
| `focus_prev` | `String` | `"Shift+Tab"` | Key to cycle focus to previous pane |
| `toggle_theme` | `String` | `"Ctrl+T"` | Key to cycle through themes |
| `scroll_up` | `String` | `"PageUp"` | Key to scroll up in focused pane |
| `scroll_down` | `String` | `"PageDown"` | Key to scroll down in focused pane |

### LayoutConfig

| Field | Type | Default | Validation | Description |
|-------|------|---------|------------|-------------|
| `left_width` | `u8` | `60` | `1-99` | Width percentage for the left column |
| `right_width` | `u8` | `40` | `1-99` | Width percentage for the right column |
| `chat_height` | `u8` | `70` | `1-99` | Height percentage for the chat pane |
| `activity_height` | `u8` | `30` | `1-99` | Height percentage for the agent activity pane |

Validation requires that `left_width + right_width = 100` and that the height proportions for each column sum to 100.

## PluginConfig

The `PluginConfig` struct controls the plugin subsystem, which allows extending xaft with custom tools, providers, and agents.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `search_paths` | `Vec<String>` | `["~/.xaft/plugins"]` | Directories to search for plugin binaries |
| `allow_dynamic` | `bool` | `false` | Whether to load dynamically-linked plugins |
| `allow_wasm` | `bool` | `true` | Whether to load WASM-based plugins |
| `security` | `PluginSecurityConfig` | `Default::default()` | Security restrictions for plugins |

### PluginSecurityConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `sandbox` | `bool` | `true` | Whether to run plugins in a sandboxed environment |
| `max_memory_mb` | `u32` | `128` | Maximum memory allocation for a single plugin |
| `network_access` | `bool` | `false` | Whether plugins can make network requests |
| `filesystem_access` | `FilesystemAccess` | `WorktreeOnly` | Filesystem access level for plugins |

## Validation Rules

The `validate()` function runs after all configuration layers have been merged and interpolated. It checks the following invariants:

### Agent Validation
- Every agent's `provider` field must reference a key in the `provider` HashMap
- `temperature` must be in the range `[0.0, 2.0]`
- `top_p` must be in the range `[0.0, 1.0]`
- `max_turns` must be greater than 0
- `allowed_tools` and `denied_tools` must not both contain the same tool name

### Provider Validation
- `timeout_secs` must be greater than 0
- If `provider_type` is `OpenaiCompatible`, `base_url` must be non-empty
- `max_retries` must be greater than 0
- At least one of `api_key` or `api_key_env` must be set for each provider (unless using a provider that doesn't require authentication)

### Guardrail Validation
- `per_session_usd` and `per_task_usd` must be greater than 0 if `cost_limit.enabled` is true
- `warning_threshold_pct` must be in the range `[1, 99]`
- `patterns` in `secret_leakage` must be valid regex

### TUI Validation
- Layout width percentages must sum to exactly 100
- Layout height percentages within each column must sum to exactly 100
- Keybinding values must be valid key combinations parseable by `crossterm`

### Tool Validation
- File size limits (if configured) must be greater than 0
- Tool timeout values must be greater than 0

If any validation rule fails, `validate()` returns a detailed error message identifying the field, the failing value, and the expected constraint. This ensures that configuration errors are caught early — at startup — rather than surfacing as runtime failures during agent execution.
