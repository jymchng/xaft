# Serialization Strategy

This document describes xaft's serialization strategy across the different data domains: tool inputs and outputs, configuration, session status, and persistent storage. Each domain has different requirements for schema stability, performance, and human readability, which leads to different format choices.

---

## Format Overview

xaft uses four serialization formats, each chosen for a specific domain:

| Domain | Format | Crate | Rationale |
|--------|--------|-------|-----------|
| Tool inputs/outputs | JSON | serde_json | LLM APIs speak JSON; schema validation is JSON Schema |
| Configuration | TOML | toml | Human-editable config files; supports comments |
| Session status | JSON | serde_json | Machine-readable; easy to serialize/deserialize |
| Persistence | SQLite + JSON | rusqlite + serde_json | Durable storage with query capability; JSON for blob columns |

The common thread is `serde` — all serialization goes through serde's trait system, which ensures that the same Rust types can be serialized to any format. The `Serialize` and `Deserialize` traits are derived on all data types that cross a serialization boundary, and the specific format is chosen at the call site.

---

## Serde + serde_json for Tool I/O

Tool inputs and outputs are always JSON. This is not a design choice but a constraint imposed by the LLM API — when an LLM requests a tool call, it provides the input as a JSON object, and when the runtime returns a tool result, it is included in the conversation as a JSON object. The `serde_json::Value` type is the lingua franca for tool data.

### Input Deserialization

Tool inputs arrive as `serde_json::Value` and are deserialized into the tool's input struct. This two-step process (receive JSON, then deserialize) allows the runtime to validate the input against the tool's JSON Schema before deserialization, providing an early rejection for malformed inputs.

```rust
// Step 1: Schema validation (runtime does this)
let schema = tool.input_schema();
jsonschema::validate(&schema, &input)?;

// Step 2: Deserialization (tool implementation does this)
let params: HttpRequestInput = serde_json::from_value(input)
    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
```

The input structs derive `Serialize`, `Deserialize`, and `JsonSchema`. The `JsonSchema` derive generates the JSON Schema that is sent to the LLM as part of the tool definition. Keeping the Rust struct, the JSON serialization, and the JSON Schema in sync through derive macros is a key design decision — it eliminates the possibility of the schema drifting from the code.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileInput {
    /// The path of the file to read, relative to the workspace root.
    pub path: String,

    /// The line range to read. If not specified, reads the entire file.
    #[serde(default)]
    pub line_range: Option<LineRange>,

    /// The encoding to use. Defaults to UTF-8.
    #[serde(default = "default_encoding")]
    pub encoding: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}
```

### Output Serialization

Tool outputs are also JSON. The `ToolOutput` struct has two representations: a human-readable summary (string) and a structured payload (`serde_json::Value`). The summary is displayed in the TUI and included in the conversation for the LLM's context. The payload is the machine-readable data that downstream tools or the agent's reasoning can process.

```rust
pub struct ToolOutput {
    summary: String,
    payload: serde_json::Value,
}

impl ToolOutput {
    pub fn new(summary: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            summary: summary.into(),
            payload,
        }
    }

    /// Create a text-only output with no structured payload.
    pub fn text(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            payload: serde_json::Value::Null,
        }
    }

    pub fn summary(&self) -> &str { &self.summary }
    pub fn payload(&self) -> &serde_json::Value { &self.payload }
}
```

The dual representation exists because the LLM and the human need different things. The LLM benefits from structured data it can reason about (for example, a list of file paths returned by a search tool). The human benefits from a readable summary they can glance at to understand what happened (for example, "Found 3 files matching 'authentication'"). Attempting to serve both audiences with a single representation leads to either unreadable JSON or unparseable text.

---

## TOML for Configuration

Configuration files use TOML because it is human-editable, supports comments, and has a clear mapping to Rust's struct types. The configuration is loaded once at startup and is immutable for the duration of the session — there is no hot-reloading of configuration.

```toml
# xaft.toml — the main configuration file

[general]
workspace = "."
default_model = "claude-sonnet-4-20250514"
max_iterations = 30

[providers.openai]
api_key = "${OPENAI_API_KEY}"
default_model = "gpt-4o"

[providers.openai.models.gpt-4o]
max_tokens = 8192
context_window = 128000
cost_per_1k_input = 0.0025
cost_per_1k_output = 0.01

[providers.anthropic]
api_key = "${ANTHROPIC_API_KEY}"
default_model = "claude-sonnet-4-20250514"

[agents.coder]
model = "claude-sonnet-4-20250514"
commit_policy = "on_success"
tools = ["read_file", "write_file", "run_shell", "search_files"]

[agents.reviewer]
model = "gpt-4o"
commit_policy = "never"
tools = ["read_file", "search_files", "list_directory"]

[tui]
fps = 60
mouse = true

[tui.theme]
background = "#1e1e2e"
foreground = "#cdd6f4"
```

The configuration struct is defined with `Deserialize`:

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    pub general: GeneralConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub agents: HashMap<String, AgentConfig>,
    #[serde(default)]
    pub tui: TuiConfig,
}

#[derive(Debug, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_workspace")]
    pub workspace: String,
    pub default_model: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
}

#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub default_model: String,
    pub models: HashMap<String, ModelConfig>,
}
```

### Environment Variable Substitution

Configuration values that contain `${VAR_NAME}` are substituted with the value of the named environment variable at load time. This is implemented as a post-processing step after TOML parsing:

```rust
impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let toml_str = std::fs::read_to_string(path)?;
        let mut config: Config = toml::from_str(&toml_str)?;
        config.substitute_env_vars()?;
        Ok(config)
    }

    fn substitute_env_vars(&mut self) -> Result<(), ConfigError> {
        // Substitute API keys
        for provider in self.providers.values_mut() {
            if let Some(ref key) = provider.api_key {
                provider.api_key = Some(Self::substitute_string(key)?);
            }
        }
        Ok(())
    }

    fn substitute_string(s: &str) -> Result<String, ConfigError> {
        let re = regex::Regex::new(r"\$\{(\w+)\}").unwrap();
        let result = re.replace_all(s, |caps: &regex::Captures| {
            let var_name = &caps[1];
            std::env::var(var_name)
                .unwrap_or_else(|_| {
                    tracing::warn!("Environment variable {} not set", var_name);
                    format!("${{{}}}", var_name) // leave unsubstituted
                })
        });
        Ok(result.to_string())
    }
}
```

The substitution is performed on the string values after parsing, not on the raw TOML text. This is important because the raw TOML might contain `${...}` in comments (which should not be substituted) or in strings that are not environment variable references (like shell glob patterns). By substituting after parsing, we only process values that the TOML parser has identified as strings, avoiding false positives.

---

## JSON for Session Status

Session status is serialized as JSON for two reasons: it is consumed by the TUI (which needs to parse it quickly), and it is stored in the session database (which has a JSON column type). The session status includes the current agent, the plan progress, the cost accumulator totals, and the conversation history metadata.

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionStatus {
    pub id: String,
    pub created_at: String, // ISO 8601
    pub active_agent: String,
    pub plan: Option<PlanStatus>,
    pub costs: CostSnapshot,
    pub agents: HashMap<String, AgentStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentStatus {
    pub state: AgentState,
    pub iterations: usize,
    pub tool_calls: usize,
    pub last_activity: String, // ISO 8601
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Thinking,
    ExecutingTool,
    WaitingApproval,
    Paused,
    Completed,
    Failed,
}
```

The `serde(rename_all = "snake_case")` attribute ensures that the enum variants are serialized as lowercase snake_case strings, which is the convention for JSON APIs. Without this attribute, Rust's default serialization would produce `"Idle"`, `"Thinking"`, etc., which is inconsistent with the rest of the JSON output.

---

## SQLite for Persistence

The `SessionStore` and `ConversationStore` use SQLite for persistent storage. SQLite provides ACID transactions, concurrent reads, and efficient key-range queries, making it ideal for session data that needs to survive process restarts.

### Schema

```sql
CREATE TABLE IF NOT EXISTS session_data (
    session_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL, -- JSON
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (session_id, key)
);

CREATE TABLE IF NOT EXISTS conversation_messages (
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL, -- auto-incrementing sequence number
    role TEXT NOT NULL, -- 'system', 'user', 'assistant', 'tool'
    content TEXT NOT NULL,
    metadata TEXT, -- JSON: tool_call_id, tool_name, etc.
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (session_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_conversation_session_seq
    ON conversation_messages (session_id, seq);
```

The `session_data` table is a key-value store with JSON values. The `key` column uses a hierarchical naming convention (e.g., `plan/step/0`, `plan/step/1`, `approval/write_file/1706000000`) that supports prefix queries via the `list_prefix()` method. The `updated_at` column enables garbage collection of stale entries.

The `conversation_messages` table uses an auto-incrementing sequence number (`seq`) for ordering. This is more reliable than timestamps (which can be ambiguous for sub-second intervals) and more efficient than a linked list structure. The sequence number is assigned by the application, not by SQLite's `AUTOINCREMENT`, which ensures that the numbers are contiguous even if rows are inserted in batches.

### JSON Column Values

The `value` column in `session_data` and the `metadata` column in `conversation_messages` contain JSON text. SQLite does not have a native JSON type — JSON is stored as TEXT. However, SQLite 3.38+ includes the `json_extract()` function, which allows querying JSON values without deserializing the entire blob. This is useful for ad-hoc analysis (for example, finding all tool calls that took longer than 10 seconds).

```rust
impl SqliteSessionStore {
    pub async fn get(&self, key: &str) -> Result<Option<serde_json::Value>, StoreError> {
        let key = format!("{}/{}", self.session_id, key);
        let result: Option<String> = sqlx::query_scalar(
            "SELECT value FROM session_data WHERE key = ?"
        )
        .bind(&key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StoreError::Database(e.to_string()))?;

        match result {
            Some(json_str) => {
                let value: serde_json::Value = serde_json::from_str(&json_str)
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }
}
```

---

## Schema Evolution

All persistent data formats must support schema evolution — the ability to read data that was written by an older version of the software. xaft uses several techniques to handle schema evolution:

### 1. serde(default) for New Fields

When a new field is added to a struct, it is annotated with `#[serde(default)]`, which provides a default value when the field is missing from the serialized data. This allows old data to be deserialized without error.

```rust
#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    pub model: String,
    pub commit_policy: String,
    #[serde(default)]
    pub tools: Vec<String>, // Added in v0.3 — old configs don't have this
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize, // Added in v0.4 — old configs don't have this
}
```

### 2. Version Field for Breaking Changes

When a schema change is not backward-compatible (for example, renaming a field or changing its type), a version field is added to the top-level structure. The deserialization logic checks the version and dispatches to the appropriate parser.

```rust
#[derive(Debug, Deserialize)]
#[serde(tag = "version")]
pub enum SessionStatusV2 {
    #[serde(rename = "1")]
    V1(SessionStatusV1),
    #[serde(rename = "2")]
    V2(SessionStatusV2Data),
}

impl SessionStatusV2 {
    pub fn into_latest(self) -> SessionStatus {
        match self {
            SessionStatusV2::V1(v1) => v1.upgrade(),
            SessionStatusV2::V2(v2) => v2.into(),
        }
    }
}
```

### 3. Migration Scripts for SQLite

When the SQLite schema changes, a migration script is included in the codebase. Migrations are applied automatically at startup using SQLite's `user_version` pragma to track the current schema version.

```rust
pub async fn run_migrations(pool: &SqlitePool) -> Result<(), StoreError> {
    let current_version: i32 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .map_err(|e| StoreError::Database(e.to_string()))?;

    if current_version < 1 {
        sqlx::query(V1_MIGRATION)
            .execute(pool)
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;
    }

    if current_version < 2 {
        sqlx::query(V2_MIGRATION)
            .execute(pool)
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;
    }

    sqlx::query(&format!("PRAGMA user_version = {}", LATEST_VERSION))
        .execute(pool)
        .await
        .map_err(|e| StoreError::Database(e.to_string()))?;

    Ok(())
}
```

### 4. Renamed Fields with alias

When a field is renamed, the `#[serde(alias = "old_name")]` attribute allows deserialization from both the old and new names. This provides a deprecation period where old data still works, giving users time to migrate their configurations.

```rust
#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    #[serde(alias = "model")]  // Old name: "model"
    pub default_model: String,  // New name: "default_model"
}
```

These four techniques — `serde(default)`, version tagging, migration scripts, and field aliases — cover the full spectrum of schema evolution scenarios, from additive changes (new fields) to transformative changes (restructured schemas). The key principle is that old data should always be readable by new code, even if some information is lost or defaulted during the upgrade. Data loss is acceptable; deserialization failure is not.
