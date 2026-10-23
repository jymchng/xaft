//! All configuration struct types for xaft.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::provider::{
    ClientOptions, ProviderCapabilities, ProviderProtocol, RequestOptionsConfig,
};
use crate::security::SecurityConfig;

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
    /// Tool configurations keyed by tool name (`[tool.<name>]`).
    pub tool: HashMap<String, ToolConfig>,
    /// Cross-tool settings (`[tools]`), including `[[tools.mcp_servers]]`.
    pub tools: ToolSettingsConfig,
    /// Guardrail configuration.
    pub guardrail: GuardrailConfig,
    /// Tool security policy (`[security]`).
    pub security: SecurityConfig,
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
    /// Context-window compaction settings.
    pub compaction: CompactionConfig,
    /// Execution limits and transport policy.
    pub execution: ExecutionConfig,
    /// Workflow orchestration mode and per-mode configuration.
    pub workflow: WorkflowModeConfig,
    /// Custom workflow definitions keyed by name — `[workflows.<name>]`.
    ///
    /// This is the xaft analogue of agenthicc's `[workflows.<name>]` table.
    /// agenthicc reads those tables with each plugin's `build_params()`; xaft
    /// has no Python plugin loader, so the table is declarative and is read by
    /// [`crate::workflows::WorkflowRegistry`]. See that module for exactly
    /// which fields change execution and which are display-only.
    pub workflows: BTreeMap<String, WorkflowDefinition>,
}

// ── WorkflowModeConfig ────────────────────────────────────────────────────────

/// Workflow mode configuration — serialized as `[workflow]` in TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowModeConfig {
    /// Workflow mode: standard, dynamic, or meta.
    pub mode: WorkflowMode,
    /// Meta mode: maximum total specialist agents the coordinator may spawn.
    pub meta_max_spawned: usize,
    /// Meta mode: maximum concurrent specialist agents.
    pub meta_max_parallel: usize,
    /// Meta mode: allow spawned specialists to spawn their own agents.
    pub meta_allow_nesting: bool,
    /// Meta mode: custom system prompt for the meta/coordinator agent.
    pub meta_prompt: Option<String>,
    /// Dynamic mode: name of the initial agent.
    pub dynamic_initial_agent: Option<String>,
    /// Dynamic mode: maximum total agent handoffs.
    pub dynamic_max_handoffs: Option<usize>,
    /// Dynamic mode: restrict to this named subset of the agent registry.
    pub dynamic_agent_subset: Option<Vec<String>>,
    /// The workflow a run uses when none is named — any registered workflow,
    /// built-in or custom. Unset follows `mode`.
    pub default_workflow: Option<String>,
}

impl Default for WorkflowModeConfig {
    fn default() -> Self {
        Self {
            mode: WorkflowMode::Standard,
            meta_max_spawned: 8,
            meta_max_parallel: 4,
            meta_allow_nesting: false,
            meta_prompt: None,
            dynamic_initial_agent: None,
            dynamic_max_handoffs: None,
            dynamic_agent_subset: None,
            default_workflow: None,
        }
    }
}

/// Workflow mode selector.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowMode {
    /// Classic fixed pipeline: planner → coder → QA ↔ fixer (default).
    #[default]
    Standard,
    /// Dynamic handoff: any registered agent can hand off to any other.
    Dynamic,
    /// Meta workflow: a coordinator agent designs and spawns specialists.
    Meta,
    /// Declarative phase graph executed by xaft's phased workflow engine.
    ///
    /// This is the analogue of agenthicc's generic `WorkflowRunner`: every
    /// [`PhaseDefinition`] is run as an agent turn (or a human checkpoint),
    /// transitions follow `next` / `on_reject`, and the run is checkpointed
    /// after every phase so it can be resumed.
    Phased,
}

// ── Custom workflow definitions ─────────────────────────────────────────────────────

/// A custom workflow — `[workflows.<name>]` in `xaft.toml`, or one file under
/// `.xaft/workflows/` / `~/.config/xaft/workflows/`.
///
/// This is the xaft analogue of an agenthicc `WorkflowPlugin` plus its
/// `[workflows.<name>]` parameter table. agenthicc loads Python classes; xaft
/// loads this declarative shape, and its phased engine executes it.
///
/// # How the orchestrator is chosen
///
/// * `mode = "standard" | "dynamic" | "meta"` selects one of the three fixed
///   orchestrators. A declared `phases` list is then descriptive only.
/// * `mode = "phased"`, **or** no `mode` with a non-empty `phases` list, runs
///   the declared phase graph — each phase is an agent turn with its own
///   prompt, tool capabilities, turn budget and transition rules.
/// * no `mode` and no `phases` runs the standard pipeline, so declaring a
///   workflow is never a silent no-op.
///
/// # Which fields change execution
///
/// * `model` / `max_turns` — override the default agent preset for the run.
/// * `dynamic_initial_agent` / `dynamic_max_handoffs` — `dynamic` mode.
/// * `meta_max_spawned` / `meta_max_parallel` — `meta` mode.
/// * `phases`, `max_total_phase_runs`, `phase_models` and any
///   `<phase>_model = "..."` key (agenthicc's `WorkflowParams` spelling) —
///   phased mode.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct WorkflowDefinition {
    /// Workflow name. Only read from standalone workflow files; inside
    /// `xaft.toml` the `[workflows.<name>]` table key is the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One-line summary shown in listings. Empty renders as `no description`.
    pub description: String,
    /// Modes this workflow is a candidate default for, e.g. `["yolo", "plan"]`.
    ///
    /// Read by [`crate::workflows::WorkflowRegistry::mode_default_map`], which
    /// resolves `{mode: workflow}` using the first binding per mode.
    pub mode_bindings: Vec<String>,
    /// Which orchestrator to run: `standard`, `dynamic`, `meta` or `phased`.
    pub mode: Option<WorkflowMode>,
    /// Model override applied to the run's default agent preset.
    pub model: Option<String>,
    /// Turn-budget override applied to the run's default agent preset.
    pub max_turns: Option<u32>,
    /// `dynamic` mode: the first agent to run.
    pub dynamic_initial_agent: Option<String>,
    /// `dynamic` mode: maximum total agent handoffs.
    pub dynamic_max_handoffs: Option<u32>,
    /// `meta` mode: maximum total specialist agents the coordinator may spawn.
    pub meta_max_spawned: Option<usize>,
    /// `meta` mode: maximum concurrent specialist agents.
    pub meta_max_parallel: Option<usize>,
    /// Hard ceiling on total phase runs in phased mode (`0`/unset = no cap).
    pub max_total_phase_runs: Option<u32>,
    /// Per-phase model overrides, `{phase name: model}`. An empty model means
    /// "use the run's model".
    pub phase_models: BTreeMap<String, String>,
    /// The phase graph. Executed in phased mode; descriptive otherwise.
    pub phases: Vec<PhaseDefinition>,
    /// Every other key in the table — agenthicc's free-form `WorkflowParams`.
    ///
    /// A `<phase>_model = "..."` key here is read as a per-phase model
    /// override, exactly like agenthicc's `CodePlanParams.execute_model`.
    #[serde(flatten)]
    pub params: BTreeMap<String, toml::Value>,
}

impl WorkflowDefinition {
    /// Whether this definition runs the phased engine.
    pub fn is_phased(&self) -> bool {
        match &self.mode {
            Some(WorkflowMode::Phased) => true,
            Some(_) => false,
            None => !self.phases.is_empty(),
        }
    }

    /// The resolved orchestrator mode — see the type docs for the rules.
    pub fn resolved_mode(&self) -> WorkflowMode {
        if self.is_phased() {
            WorkflowMode::Phased
        } else {
            self.mode.clone().unwrap_or(WorkflowMode::Standard)
        }
    }

    /// The model override for `phase`, if any.
    ///
    /// `phase_models.<phase>` wins over a `<phase>_model` param, which wins
    /// over the phase's own `model` field. Blank values mean "no override".
    pub fn model_for_phase(&self, phase: &PhaseDefinition) -> Option<String> {
        let non_blank = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        if let Some(m) = self
            .phase_models
            .get(&phase.name)
            .and_then(|m| non_blank(m))
        {
            return Some(m);
        }
        if let Some(toml::Value::String(m)) = self.params.get(&format!("{}_model", phase.name)) {
            if let Some(m) = non_blank(m) {
                return Some(m);
            }
        }
        phase.model.as_deref().and_then(non_blank)
    }
}

/// One phase of a workflow's graph — the analogue of agenthicc's `PhaseSpec`.
///
/// Every field is optional in TOML; the defaults reproduce agenthicc's
/// `PhaseSpec` defaults (`agent_type = "auto"`, `max_turns = 20`,
/// `max_iterations = -1`, foreground terminals, one-shot commands).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct PhaseDefinition {
    /// Unique phase identifier; the target of `next` / `on_reject`.
    pub name: String,
    /// Agent role: `auto`, `planner`, `executor`, `reviewer`, `explorer`,
    /// `verifier`, `custom` or `human`. Empty means `auto`.
    pub agent_type: String,
    /// Role prompt for the phase. Replaces the role's default prompt; the
    /// engine still appends its own clarification and transition-tool blocks.
    #[serde(
        alias = "system_prompt_override",
        skip_serializing_if = "String::is_empty"
    )]
    pub system_prompt: String,
    /// File holding the role prompt, relative to the workflow file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    /// The phase's task message. Placeholders: `{intent}`, `{phase}`,
    /// `{workflow}`, `{context}`, `{iteration}`, `{previous_output}` and
    /// `{output.<phase>}`. Empty uses the engine's default task message. For a
    /// `human` phase this is the question put to the user.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// File holding the task message, relative to the workflow file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    /// Operational mode for this phase (`plan`, `safe`, `auto`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode_override: Option<String>,
    /// Tool-capability allowlist (`read`, `write`, `execute`, `git_read`,
    /// `git_write`, `network`, `memory`). Unset uses the role default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_capabilities: Option<Vec<String>>,
    /// Explicit tool-name allowlist, applied after the capability filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Named workflow tool groups to add to the phase (e.g. `goal_list`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_tools: Vec<String>,
    /// Maximum LLM sub-turns within one run of the phase (default 20).
    pub max_turns: Option<u32>,
    /// How to parse the phase output: `plan`, `review_result`, `free_text`
    /// or `json`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<String>,
    /// Phase to run after this one succeeds; `None` ends the workflow.
    pub next: Option<String>,
    /// Phase to run when this phase's output is rejected.
    pub on_reject: Option<String>,
    /// Phase to run when this phase fails with an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
    /// Maximum times this phase may be entered in one run (`-1` = unlimited).
    /// With an explicit-completion flag it also caps continuation turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<i64>,
    /// Loop until `mark_execute_complete` is called.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_explicit_completion: bool,
    /// Loop until `finalize_plan` is called.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_plan_finalization: bool,
    /// Loop until `approve_review` or `reject_review` is called.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_explicit_review: bool,
    /// Sibling phases to run concurrently with this one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parallel_with: Vec<String>,
    /// Model override for this phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `foreground` (default) or `background` terminal execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_wait_policy: Option<String>,
    /// `oneshot` (default) or `service` command lifecycle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_lifecycle: Option<String>,
    /// Fail the phase when any shell command it ran exited non-zero.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_successful_commands: bool,
    /// Require a successful service readiness check before transition.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_readiness: bool,
    /// Custom transition tools for this phase.
    ///
    /// Each entry becomes a callable tool. The phase keeps running (with a
    /// continuation reminder) until one of them is called, and the call
    /// decides the next phase — the declarative analogue of the hand-written
    /// transition tools agenthicc's custom runners define.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<TransitionDefinition>,
    /// Message sent on continuation turns, when the agent ended a turn
    /// without calling a required transition tool. Same placeholders as
    /// `prompt`. Empty uses the engine's reminder.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub continuation_prompt: String,
}

/// A declarative phase-transition tool.
///
/// Calling it records its arguments as the phase's structured output and moves
/// the workflow along the declared edge. Prose never advances a phase; only a
/// successful transition call does.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct TransitionDefinition {
    /// Tool name, `lower_snake_case`.
    pub tool: String,
    /// What the tool means, shown to the model.
    pub description: String,
    /// The tool's string parameters, all required unless marked otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<TransitionParam>,
    /// Phase to enter after the call. Unset follows the phase's `next`
    /// (or `on_reject` for a `rejected` outcome).
    pub next: Option<String>,
    /// End the workflow after this call instead of following any edge.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub end: bool,
    /// `approved` (default), `rejected` or `exit`. `exit` ends the run early,
    /// as agenthicc's `exit_code_plan` does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Ask the user to approve the call first; a refusal is returned to the
    /// agent as feedback and the phase continues.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_user_approval: bool,
    /// A precondition the engine enforces before accepting the call, e.g.
    /// `goals_complete`. An unmet guard is returned to the agent as an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    /// A deterministic side effect the engine performs when the call is
    /// accepted, e.g. `publish_workflow_draft`. A failing effect rejects the
    /// call and the phase continues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
}

/// One string parameter of a [`TransitionDefinition`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TransitionParam {
    /// Parameter name, `lower_snake_case`.
    pub name: String,
    /// What the value should contain, shown to the model.
    pub description: String,
    /// Whether the call is rejected when the value is missing or blank.
    pub required: bool,
}

impl Default for TransitionParam {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            required: true,
        }
    }
}

impl TransitionDefinition {
    /// The outcome, with unset read as `approved`.
    pub fn outcome_or_default(&self) -> &str {
        self.outcome.as_deref().unwrap_or("approved")
    }
}

impl PhaseDefinition {
    /// The agent role, with an empty value read as `auto`.
    pub fn role(&self) -> &str {
        let t = self.agent_type.trim();
        if t.is_empty() { "auto" } else { t }
    }

    /// Whether this phase is a human checkpoint rather than an agent turn.
    pub fn is_human(&self) -> bool {
        self.role() == "human"
    }

    /// `max_iterations`, with unset read as agenthicc's `-1` (unlimited).
    pub fn max_iterations_or_unlimited(&self) -> i64 {
        self.max_iterations.unwrap_or(-1)
    }

    /// `max_turns`, with unset read as agenthicc's default of 20.
    pub fn max_turns_or_default(&self) -> u32 {
        self.max_turns.unwrap_or(20)
    }
}

// ── ConfigPatch ───────────────────────────────────────────────────────────────

/// A single runtime config override applied via the TUI `/config` editor.
/// Patches are session-layer only — not written to disk.
#[derive(Debug, Clone)]
pub struct ConfigPatch {
    /// Dotted key path (e.g. `"agent.default.max_turns"`).
    pub dotted_key: String,
    /// The new value as a TOML value.
    pub value: toml::Value,
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
    /// Enable AGENTS.md auto-loading.
    pub agents_md_enabled: bool,
    /// Maximum bytes to load from a single AGENTS.md file.
    pub agents_md_max_bytes: usize,
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
    /// Directory holding the SQLite memory database.
    ///
    /// `None` uses the platform data directory (`dirs::data_dir()/xaft/memory`).
    /// Set this to relocate persistent memory, or to point a test at a temp
    /// directory. agenthicc's equivalent is `security`/`MemorySettings`
    /// `project_memory_path` (parity row 120).
    pub project_memory_path: Option<String>,
    /// TTL in seconds applied to newly remembered entries.
    ///
    /// `None` (the default) means entries never expire. This is the
    /// user-facing half of `xaft_memory::MemoryConfig::auto_remember_ttl_secs`,
    /// which existed at runtime but had **no** key that could set it:
    /// `src/main.rs` hardcoded it to `None`, and nothing in `xaft-memory`
    /// read it at all. Both halves are now wired — see
    /// `XaftMemoryManager::remember_with_agent`.
    ///
    /// agenthicc's equivalent is `MemorySettings.session_ttl_seconds`
    /// (parity row 122), which defaults to 86400. xaft deliberately does
    /// NOT match that default: agenthicc expires session-tier memory after a
    /// day, whereas this key covers every newly remembered entry, so adopting
    /// 86400 would begin silently deleting long-lived project facts. Set it
    /// explicitly if you want expiry.
    pub auto_remember_ttl_secs: Option<u64>,
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
            project_memory_path: None,
            // Never expire by default — see the field doc for why this does
            // not match agenthicc's 86400.
            auto_remember_ttl_secs: None,
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
    /// Human-readable summary of what this agent is for.
    ///
    /// Parity with agenthicc's `AgentSettings.description`.
    pub description: String,
    /// Skill names or glob patterns this agent may activate.
    ///
    /// `None` means no allowlist is applied — every skill not denied is
    /// available, which is agenthicc's `allowed_skills=None` semantics and the
    /// default. `Some(vec![])` is a real allowlist that permits nothing.
    pub allowed_skills: Option<Vec<String>>,
    /// Skill names or glob patterns this agent may never activate.
    ///
    /// Checked before `allowed_skills`, exactly like `denied_tools`.
    pub denied_skills: Vec<String>,
    /// Parallel tool call policy: `"sequential"` | `"annotated"` | `"all"`. `None` uses `"annotated"`.
    pub parallel_tool_policy: Option<String>,
    /// Max concurrent tool calls. `None` uses the default (4).
    pub max_concurrent_tools: Option<u32>,
    /// Allow this agent to register new tools at runtime via `tool_factory`.
    pub allow_dynamic_tools: bool,
    /// Maximum dynamic tools this agent may register per run.
    pub max_dynamic_tools: usize,
    /// Require TUI approval gate before each dynamic tool executes.
    pub dynamic_tool_approval: bool,
}

impl AgentPreset {
    /// Return `true` if this preset may activate the given skill.
    ///
    /// Mirrors [`ResolvedAgentPreset::allows_tool`] and agenthicc's
    /// `SkillPermissionSet`: `denied_skills` wins outright, and an absent
    /// (`None`) `allowed_skills` imposes no restriction. The difference from
    /// the tool rule is that `Some([])` is a real allowlist permitting
    /// nothing, not an unrestricted one.
    pub fn allows_skill(&self, skill: &str) -> bool {
        for pattern in &self.denied_skills {
            if glob_matches(pattern, skill) {
                return false;
            }
        }
        match &self.allowed_skills {
            None => true,
            Some(allowed) => allowed.iter().any(|p| glob_matches(p, skill)),
        }
    }
}

/// A fully-resolved agent preset ready for use at runtime.
#[derive(Debug, Clone)]
pub struct ResolvedAgentPreset {
    /// Preset name.
    pub name: String,
    /// Human-readable summary carried through from `AgentPreset::description`.
    pub description: String,
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
    /// Upper bound on completion tokens; `None` means unbounded.
    pub max_completion_tokens: Option<u32>,
    /// Stop sequences.
    pub stop_sequences: Vec<String>,
    /// Allowed tool patterns.
    pub allowed_tools: Vec<String>,
    /// Denied tool patterns.
    pub denied_tools: Vec<String>,
    /// Skill allowlist; `None` means unrestricted.
    pub allowed_skills: Option<Vec<String>>,
    /// Skills this preset may never activate.
    pub denied_skills: Vec<String>,
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

    /// Return `true` if this preset may activate the given skill.
    ///
    /// Same rule as [`AgentPreset::allows_skill`]: `denied_skills` wins, and
    /// `allowed_skills = None` imposes no restriction.
    pub fn allows_skill(&self, skill: &str) -> bool {
        for pattern in &self.denied_skills {
            if glob_matches(pattern, skill) {
                return false;
            }
        }
        match &self.allowed_skills {
            None => true,
            Some(allowed) => allowed.iter().any(|p| glob_matches(p, skill)),
        }
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
    /// Default model for this endpoint, used when a preset names no model.
    ///
    /// Parity with agenthicc's `ProviderProfile.model`.
    #[serde(default)]
    pub model: String,
    /// Default sampling temperature. `None` leaves it to the preset.
    ///
    /// Parity with agenthicc's `ProviderProfile.temperature`.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Default nucleus sampling parameter. `None` leaves it to the preset.
    ///
    /// Parity with agenthicc's `ProviderProfile.top_p`.
    #[serde(default)]
    pub top_p: Option<f32>,
    /// Upper bound on completion tokens. `None` means unbounded.
    ///
    /// Parity with agenthicc's `ProviderProfile.max_completion_tokens`.
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    /// Query parameters added to every request from this endpoint.
    ///
    /// Parity with agenthicc's `ProviderProfile.default_query`. JSON values so
    /// an `api-version` string and a `beta=1` integer both round-trip; xaft
    /// does not re-type them, the transport layer does.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub default_query: BTreeMap<String, serde_json::Value>,
    /// Transport-level client options.
    ///
    /// Parity with agenthicc's `ProviderProfile.client_options`.
    #[serde(default, skip_serializing_if = "ClientOptions::is_empty")]
    pub client_options: ClientOptions,
    /// Per-request options for this endpoint.
    ///
    /// Parity with agenthicc's `ProviderProfile.request_options`. `None` when
    /// the profile sets none, which is what keeps a preset's own request
    /// options from being silently replaced by an empty table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_options: Option<RequestOptionsConfig>,
    /// Per-request timeout in seconds. `None` defers to `timeout_secs`.
    ///
    /// Parity with agenthicc's `ProviderProfile.timeout_s`, which is nullable
    /// so "no profile override" is distinct from any particular value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<f64>,
    /// Wire protocol. `None` derives one from `type` at resolve time.
    ///
    /// Parity with agenthicc's `ProviderProfile.protocol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ProviderProtocol>,
    /// Declared provider capabilities.
    ///
    /// Parity with agenthicc's `ProviderProfile.capabilities`. Unset flags read
    /// as `false`, so an endpoint is never assumed to support a feature.
    #[serde(default, skip_serializing_if = "ProviderCapabilities::is_default")]
    pub capabilities: ProviderCapabilities,
    /// Name of a header carrying the session identity.
    ///
    /// Parity with agenthicc's `ProviderProfile.session_header`. Empty means
    /// "do not send one"; a non-empty name requires a session conversation id
    /// before a transport can be built.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_header: String,
}

/// Provider backend type.
///
/// Fieldless, so `Copy`: a resolved profile carries one by value and there is
/// no reason for a caller to have to clone it to get there.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    /// Anthropic Claude API.
    Anthropic,
    /// OpenAI API.
    Openai,
    /// Any OpenAI-compatible API (Ollama, Together, etc.).
    OpenaiCompatible,
}

impl ProviderType {
    /// The exact wire string, matching what `type = "..."` takes in TOML.
    ///
    /// `serde(rename_all = "kebab-case")` produces the same spelling when
    /// serializing, but `Serialize` cannot be borrowed out of `&self` without
    /// allocating, so a caller that only needs the string (such as
    /// `xaft config profiles`) gets it from here instead.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::OpenaiCompatible => "openai-compatible",
        }
    }
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ProviderConfig {
    /// Resolve the actual API key, checking `api_key_env` before `api_key`.
    pub fn resolve_api_key(&self, provider_name: &str) -> Option<String> {
        self.resolve_api_key_with(&|name| std::env::var(name).ok(), provider_name)
    }

    /// Resolve the actual API key through an injected environment lookup.
    ///
    /// The indirection keeps this testable: mutating the process environment is
    /// global, and tests that race on it are the usual reason a config test
    /// suite becomes flaky.
    pub fn resolve_api_key_with<F>(&self, lookup: &F, provider_name: &str) -> Option<String>
    where
        F: Fn(&str) -> Option<String>,
    {
        // 1. Explicit env var override
        if let Some(ref env_var) = self.api_key_env {
            if let Some(val) = lookup(env_var) {
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
        lookup(&env_name)
    }

    /// Resolve a model name through the alias table.
    pub fn resolve_model(&self, model: &str) -> String {
        self.models
            .get(model)
            .cloned()
            .unwrap_or_else(|| model.to_string())
    }

    /// The model this endpoint uses when a preset does not name one.
    ///
    /// `None` when the profile sets no default, which is not the same as a
    /// preset that asks for the empty string.
    pub fn default_model(&self) -> Option<&str> {
        if self.model.is_empty() {
            None
        } else {
            Some(&self.model)
        }
    }
}

// ── ToolConfig ────────────────────────────────────────────────────────────────

/// Execution limits and transport policy (`[execution]`).
///
/// Parity with agenthicc's `config.py::ExecutionSettings`. These are the
/// scheduler- and transport-level knobs that bound a run; per-agent knobs
/// (model, max_turns, temperature) live on [`AgentPreset`], and provider
/// connection details live on [`ProviderConfig`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionConfig {
    /// How many intents (user turns) may be in flight at once.
    pub max_concurrent_intents: usize,
    /// How many tasks one intent may run in parallel.
    pub max_parallel_tasks: usize,
    /// Size of the worker pool backing the above.
    pub agent_pool_size: usize,
    /// Hard ceiling on ReAct iterations for a single intent.
    pub max_agent_turns: u32,
    /// Completion-token budget for one LLM round-trip.
    pub max_output_tokens: u32,
    /// Per-turn watchdog in seconds; `0.0` disables it.
    pub turn_timeout_s: f64,
    /// Enable proactive LLM compaction before the hard pre-send guard.
    pub auto_compact: bool,
    /// Per-model context windows, keyed by lower-cased model id. The reserved
    /// key `"default"` is the fallback for unknown or proxied models.
    pub context_windows: HashMap<String, u32>,
    /// Incremental caching of the system prompt, tools and conversation
    /// prefix. A no-op on providers that do not support prompt caching.
    pub prompt_cache: bool,
    /// Durable, freshness-validated cache of file reads.
    pub file_cache: bool,
    /// Turn-level retry budget for transient transport failures.
    pub transport_max_retries: u32,
    /// Base delay for transport retry backoff, seconds.
    pub transport_retry_base_delay_s: f64,
    /// Attempts a single generated artifact may take.
    ///
    /// Parity with agenthicc's `ExecutionSettings.authoring_max_generation_attempts`.
    /// `None` means unbounded -- distinct from a cap of zero, which
    /// `validate` rejects -- so a caller can tell "no authoring limit was
    /// configured" from "the limit is zero".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authoring_max_generation_attempts: Option<u32>,
    /// Turns a single authoring phase may take.
    ///
    /// Parity with agenthicc's `ExecutionSettings.authoring_max_phase_turns`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authoring_max_phase_turns: Option<u32>,
    /// Named provider profile the run should use.
    ///
    /// Parity with agenthicc's `ExecutionSettings.profile`. `None` lets the agent
    /// preset pick, which is why it is nullable rather than an empty string:
    /// an empty string names no profile, and treating it as "use the default"
    /// would make a typo silently select a provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Capability overrides applied on top of the resolved profile.
    ///
    /// Parity with agenthicc's `ExecutionSettings.provider_capabilities`. A map
    /// so a single capability can be turned off without restating the rest;
    /// `None` means "use whatever the profile declares".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_capabilities: Option<BTreeMap<String, bool>>,
    /// Wall-clock ceiling across all transport retries; `0.0` means no cap.
    pub transport_retry_max_total_s: f64,
    /// SDK-internal retry budget, kept low to avoid a retry multiplier.
    pub llm_sdk_max_retries: u32,
    /// Separate bounded recovery budget for permanent provider errors
    /// (an unsupported model, for example) before the run is checkpointed.
    pub irrecoverable_error_max_retries: u32,
}

impl ExecutionConfig {
    /// The context window for `model`, falling back to the `"default"` entry.
    ///
    /// Returns `None` when neither the model nor `"default"` is configured, so
    /// callers can distinguish "no opinion" from "zero".
    pub fn context_window_for(&self, model: &str) -> Option<u32> {
        let key = model.to_lowercase();
        self.context_windows
            .get(&key)
            .or_else(|| self.context_windows.get("default"))
            .copied()
    }

    /// Whether the turn watchdog applies to a turn of `elapsed` seconds.
    ///
    /// `0.0` disables the watchdog entirely.
    pub fn turn_timed_out(&self, elapsed: f64) -> bool {
        self.turn_timeout_s > 0.0 && elapsed >= self.turn_timeout_s
    }

    /// Whether a provider error of this class should be retried at all.
    ///
    /// Permanent errors draw on the separate, smaller budget rather than the
    /// transport budget, which exists for transient faults.
    pub fn should_retry_permanent_error(&self, attempt: u32) -> bool {
        attempt < self.irrecoverable_error_max_retries
    }
}

/// Tool settings that are not per-tool (`[tools]`).
///
/// Parity with agenthicc's `ToolSettings`, which is the `[tools]` table rather
/// than xaft's per-tool `[tool.<name>]` map. The two are separate sections on
/// purpose: a global policy is not a default for one tool.
///
/// `mcp_servers`, `allowed` and `denied` are read by the runtime and the
/// sandbox. The keys added alongside them (`plugins`, `max_live_tool_calls`,
/// `group_exploratory_calls`, `http_timeout_s`, `question_timeout_s`) are
/// **loadable and validated but not yet consumed** by anything outside this
/// crate, so their rows in `docs/parity-matrix.md` stay ❌ — under this
/// repository's rule that a declared field nobody reads is not a working key.
///
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolSettingsConfig {
    /// MCP servers, written `[[tools.mcp_servers]]`.
    ///
    /// Parity with agenthicc's `ToolSettings.mcp_servers`. agenthicc declares
    /// these under `[tools]`; xaft historically took them under
    /// `[[mcp.client]]`. Both spellings are read, and a server named in both is
    /// reported as a duplicate rather than silently resolved — a config that
    /// says two different things about one server has no safe winner.
    ///
    /// Skipped on serialization when empty, so a config that declares no
    /// servers does not start writing `mcp_servers = []` back out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<McpClientConfig>,
    /// Glob patterns for tools the agent may call.
    ///
    /// Parity with agenthicc's `ToolSettings.allowed`. **Fail-closed**: an empty
    /// list means no tool matches, not "every tool matches". A config that
    /// wants everything writes `"*"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
    /// Glob patterns for tools the agent may never call.
    ///
    /// Parity with agenthicc's `ToolSettings.denied`. Evaluated before
    /// [`Self::allowed`], so deny wins regardless of config order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied: Vec<String>,
    /// Plugins loaded into this run.
    ///
    /// Parity with agenthicc's `ToolSettings.plugins`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
    /// How many tool calls may be shown live at once.
    ///
    /// Parity with agenthicc's `ToolSettings.max_live_tool_calls`. A *display*
    /// budget: calls beyond this are finished but not rendered, so the TUI
    /// stays responsive without changing what was executed.
    #[serde(
        default = "default_max_live_tool_calls",
        skip_serializing_if = "is_default_max_live_tool_calls"
    )]
    pub max_live_tool_calls: usize,
    /// Collapse consecutive read-only calls into one line.
    ///
    /// Parity with agenthicc's `ToolSettings.group_exploratory_calls`. Only
    /// affects rendering, never what runs.
    #[serde(default = "bool_true", skip_serializing_if = "is_true")]
    pub group_exploratory_calls: bool,
    /// Outbound HTTP timeout, seconds; `0.0` disables it.
    ///
    /// Parity with agenthicc's `ToolSettings.http_timeout_s`. Distinct from
    /// `[execution].turn_timeout_s`, which covers a whole turn rather than one
    /// HTTP request.
    #[serde(
        default = "default_http_timeout_s",
        skip_serializing_if = "is_default_http_timeout_s"
    )]
    pub http_timeout_s: f64,
    /// Timeout waiting on an interactive question, seconds.
    ///
    /// Parity with agenthicc's `ToolSettings.question_timeout_s`. Rejected by
    /// `validate` when zero or negative: a question that times out immediately
    /// would deny the agent the chance to answer at all.
    #[serde(
        default = "default_question_timeout_s",
        skip_serializing_if = "is_default_question_timeout_s"
    )]
    pub question_timeout_s: f64,
}

/// `skip_serializing_if` predicate for a field whose default is `true`.
fn is_true(value: &bool) -> bool {
    *value
}

impl Default for ToolSettingsConfig {
    /// Matches agenthicc's `ToolSettings` defaults.
    ///
    /// Hand-written rather than derived because the parity keys carry non-zero
    /// defaults. `Default::default()` is what builds the root `XaftConfig`, so a
    /// derived impl would hand a fresh config `max_live_tool_calls = 0` and
    /// `question_timeout_s = 0.0` -- silently different from the same struct
    /// deserialized from a file that simply omits the keys, and a zero question
    /// timeout would deny the agent any chance to answer.
    fn default() -> Self {
        Self {
            mcp_servers: Vec::new(),
            allowed: Vec::new(),
            denied: Vec::new(),
            plugins: Vec::new(),
            max_live_tool_calls: default_max_live_tool_calls(),
            group_exploratory_calls: true,
            http_timeout_s: default_http_timeout_s(),
            question_timeout_s: default_question_timeout_s(),
        }
    }
}

/// agenthicc's `ToolSettings.max_live_tool_calls` default.
fn default_max_live_tool_calls() -> usize {
    5
}

/// agenthicc's `ToolSettings.http_timeout_s` default.
fn default_http_timeout_s() -> f64 {
    30.0
}

/// agenthicc's `DEFAULT_QUESTION_TIMEOUT_S`.
fn default_question_timeout_s() -> f64 {
    300.0
}

fn is_default_max_live_tool_calls(value: &usize) -> bool {
    *value == 5
}

fn is_default_http_timeout_s(value: &f64) -> bool {
    *value == 30.0
}

fn is_default_question_timeout_s(value: &f64) -> bool {
    *value == 300.0
}

impl ToolSettingsConfig {
    /// Duplicate server names, sorted.
    ///
    /// Two stanzas for one name is a config error, not a merge: `mcp add`
    /// refuses to create the duplicate, and a hand-edited file that already has
    /// one should be reported rather than resolved by ordering.
    pub fn duplicate_server_names(&self) -> Vec<String> {
        let mut names: Vec<&str> = self.mcp_servers.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        let mut duplicates: Vec<String> = names
            .iter()
            .filter(|name| names.iter().filter(|other| *other == *name).count() > 1)
            .map(|name| (*name).to_string())
            .collect();
        duplicates.dedup();
        duplicates
    }
}

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

/// Transports xaft accepts for an [`McpClientConfig`] stanza.
///
/// This is agenthicc's set (`stdio`, `ws`, `websocket`, `streamable`, `http`)
/// plus `http+sse`, which xaft's own docs on this struct have advertised since
/// the field was introduced. Accepting the union means a config file written
/// against either project keeps parsing.
pub const MCP_TRANSPORTS: &[&str] = &["stdio", "http", "http+sse", "streamable", "ws", "websocket"];

/// MCP client connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpClientConfig {
    /// Connection name. Defaults to `""` when the stanza omits it.
    ///
    /// A stanza with no `name` is unusable — no `mcp` subcommand can address it
    /// — but that is not a reason to reject the file. This default exists so
    /// the typed loader tolerates a partially-specified `[[mcp.client]]`
    /// stanza instead of failing with `missing field 'name'`, which rejected
    /// the *whole file* and so bricked every xaft command with exit 3,
    /// including unrelated ones like `xaft version`.
    ///
    /// agenthicc reaches the same outcome by a different route: its `mcp list`
    /// never consults the typed config at all — `_redacted_servers` reads the
    /// TOML directly — and a stanza it cannot build is reported as
    /// `{"name": "<invalid>", "status": "invalid"}`. xaft's typed loader runs
    /// at startup for *every* command, so it has to be the tolerant side of
    /// that bargain. The empty name this default produces is never rendered as
    /// a valid server: `xaft_cli::mcp::parse_servers` reads `name` off the raw
    /// table and classifies a stanza that has no such key as
    /// `McpEntry::Invalid`.
    #[serde(default)]
    pub name: String,
    /// Transport: one of [`MCP_TRANSPORTS`].
    ///
    /// Defaults to `stdio`, matching agenthicc's
    /// `McpServerConfig.transport` dataclass default. Without this default a
    /// hand-written `[[mcp.client]]` stanza that omits the field made the
    /// strict typed loader reject the *whole file*, which bricked every xaft
    /// command with exit 3 — including unrelated ones like `xaft version`.
    /// That was a parity divergence: the identical file is valid under
    /// agenthicc, because a dataclass field with a default is not required.
    #[serde(default = "default_transport_stdio")]
    pub transport: String,
    /// Command to execute (for stdio transport).
    pub command: Option<String>,
    /// Arguments (for stdio transport).
    #[serde(default)]
    pub args: Vec<String>,
    /// URL (for remote transports).
    pub url: Option<String>,
    /// Whether this client is active.
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// Bearer token. May be a `${ENV_VAR}` reference rather than a literal.
    ///
    /// Never echoed by any CLI command: see [`McpClientConfig::redacted`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Name of the environment variable that holds the bearer token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    /// Header name -> environment variable name.
    ///
    /// Only the *variable names* are meant to live here, never the header
    /// values. That is what makes the map safe to print, and
    /// [`McpClientConfig::redacted`] relies on it — but "meant to" is not
    /// "enforced", because `xaft.toml` is a hand-edited file and nothing in
    /// the loader rejects a value that is not a variable name. A pasted
    /// literal used to reach stdout through `mcp get` and `mcp list --json`
    /// verbatim; see [`redacted_env_headers`] for the check that now closes it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_headers: BTreeMap<String, String>,
    /// Connect at session start without being asked.
    #[serde(default = "bool_true")]
    pub auto_connect: bool,
    /// How many times to retry a dropped connection.
    #[serde(default = "default_reconnect_attempts")]
    pub reconnect_attempts: u32,
    /// Delay between reconnect attempts, in seconds.
    #[serde(default = "default_reconnect_delay")]
    pub reconnect_delay_seconds: f64,
}

impl McpClientConfig {
    /// The transport, lowercased and trimmed.
    ///
    /// Config files are hand-edited, so `"STDIO"` and `" stdio "` have to mean
    /// the same thing as `"stdio"` for display and comparison.
    pub fn normalized_transport(&self) -> String {
        self.transport.trim().to_ascii_lowercase()
    }

    /// Whether this server has environment-backed credentials.
    ///
    /// Mirrors agenthicc's `mcp auth` rule exactly: authentication is
    /// configured when at least one `env_headers` entry exists, or when
    /// `token` is a `${ENV_VAR}` *reference*. A literal token in the file is
    /// deliberately not counted — it is a secret somebody pasted in, not
    /// environment-backed configuration.
    pub fn has_auth(&self) -> bool {
        !self.env_headers.is_empty() || self.token.as_deref().is_some_and(is_env_reference)
    }

    /// The `env_headers` view that is safe to print.
    ///
    /// This is where the "an `env_headers` value is a variable *name*, so it
    /// reveals nothing" claim is actually enforced. A value that is not a
    /// valid environment-variable name cannot be a name, so it must be
    /// something else — in practice a credential somebody pasted into a
    /// hand-edited file — and it is replaced with
    /// [`NON_ENV_HEADER_VALUE`] rather than printed.
    ///
    /// A conforming value is copied through unchanged, because that is the
    /// information the command exists to show: which variable backs which
    /// header. Blanking the whole map would be safe and useless.
    ///
    /// The header *names* are always kept. They are keys chosen by the user,
    /// not values, and `Authorization`/`X-Api-Key` are exactly what a reader
    /// needs in order to know which credential is misconfigured.
    pub fn redacted_env_headers(&self) -> BTreeMap<String, String> {
        self.env_headers
            .iter()
            .map(|(header, value)| {
                let shown = if is_env_var_name(value) {
                    value.clone()
                } else {
                    NON_ENV_HEADER_VALUE.to_string()
                };
                (header.clone(), shown)
            })
            .collect()
    }

    /// A JSON view of this server that is safe to print.
    ///
    /// `token` is omitted entirely and `env_headers` goes through
    /// [`redacted_env_headers`], which copies conforming variable names and
    /// masks anything else. The asymmetry between the two fields is agenthicc's
    /// (`McpServerConfig.redacted()`): an `env_headers` entry maps
    /// `Authorization` to the *name* `MCP_TOKEN`, which reveals nothing,
    /// whereas `token` holds the credential itself.
    ///
    /// # Where the invariant is enforced, and why not earlier
    ///
    /// agenthicc asserts the same shape with `McpServerConfig.validate()`,
    /// which rejects any `env_headers` value that fails `_ENV_NAME_RE`
    /// (`^[A-Z_][A-Z0-9_]*$`). **That check is not a load-time check in
    /// agenthicc either**: `validate()` is only called from `McpSessionManager`
    /// (`tools/mcp_manager.py:328` and `:850`), so it runs when a server is
    /// connected. `mcp list` and `mcp get` build the config with
    /// `from_dict()` — which never validates — and print it, so agenthicc's own
    /// `list`/`get` would echo a pasted literal. It has the invariant; it just
    /// does not enforce it on the path that prints.
    ///
    /// xaft enforces it at render time instead, and rejects nothing:
    ///
    /// * Rejecting at load time would be a *divergence*, not a port. xaft's
    ///   [`crate::loader::ConfigLoader`] runs before dispatching any command and
    ///   exits 3 on error, so one malformed `env_headers` value would brick
    ///   `xaft version` and every unrelated command. That is exactly the defect
    ///   `McpClientConfig`'s `name` and `transport` serde defaults were added to
    ///   undo, and it is the reason those fields must never become mandatory
    ///   again.
    /// * Render-time enforcement cannot be bypassed by the thing that actually
    ///   causes leaks here — hand-editing. There is one print path,
    ///   [`redacted`], and both `mcp get` and `mcp list --json` go through it.
    ///
    /// The observable difference is deliberate and worth stating: a file with a
    /// bad value loads, starts every command, and `mcp get` reports the header
    /// with a placeholder rather than refusing to start.
    pub fn redacted(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("name".into(), self.name.clone().into());
        map.insert("transport".into(), self.normalized_transport().into());
        map.insert(
            "url".into(),
            match self.normalized_transport().as_str() {
                "stdio" => "<stdio>".into(),
                _ => self.url.clone().unwrap_or_default().into(),
            },
        );
        map.insert("command".into(), self.command.clone().into());
        map.insert("args".into(), self.args.clone().into());
        map.insert("enabled".into(), self.enabled.into());
        map.insert("auto_connect".into(), self.auto_connect.into());
        map.insert(
            "env_headers".into(),
            serde_json::Value::Object(
                self.redacted_env_headers()
                    .into_iter()
                    .map(|(k, v)| (k, serde_json::Value::String(v)))
                    .collect(),
            ),
        );
        map.insert("reconnect_attempts".into(), self.reconnect_attempts.into());
        map.insert(
            "reconnect_delay_seconds".into(),
            self.reconnect_delay_seconds.into(),
        );
        serde_json::Value::Object(map)
    }
}

/// Whether `value` is treated as an environment reference by `mcp auth`.
///
/// This mirrors agenthicc exactly, which tests `token.startswith("${")` and
/// nothing more. The rule is deliberately looser than it looks: `"${"`,
/// `"${lower}"` and `"${MCP_TOKEN} and more"` all count as references.
///
/// That looseness is accepted on purpose, for two reasons. parity is the point
/// of this command family, and a value that reports `not_configured` when
/// agenthicc reports `configured` is a bug a user would file. And the strict
/// check does exist, one layer up: `--token-env` on `mcp add` only accepts an
/// uppercase environment-variable name, so a malformed reference can only reach
/// here by hand-editing the config, where `configured` is a fine answer.
pub fn is_env_reference(value: &str) -> bool {
    value.starts_with("${")
}

/// The placeholder [`McpClientConfig::redacted_env_headers`] prints in place of
/// an `env_headers` value that is not an environment-variable name.
///
/// The wording matters: it has to say *why* the value is missing, because the
/// most likely cause is a credential the user pasted into `xaft.toml` where a
/// variable name belongs. A bare `<redacted>` would leave them guessing between
/// "secret" and "typo", and those have opposite fixes.
pub const NON_ENV_HEADER_VALUE: &str = "<redacted: not an environment variable name>";

/// Whether `name` is a syntactically valid environment-variable name.
///
/// This is agenthicc's `_ENV_NAME_RE` — `^[A-Z_][A-Z0-9_]*$`, declared twice in
/// agenthicc (`tools/mcp.py:30` and `cli/mcp_config.py:26`) — and it is the
/// single implementation of that rule in xaft. It backs two things that must not
/// disagree about what an env-var name is: [`McpClientConfig::redacted`], which
/// masks a value that fails it, and `xaft mcp add --token-env`, which rejects
/// one that fails it. Keep one definition, so a name the CLI accepts can never
/// be one the redactor hides, or the other way round.
///
/// Hand-rolled for the same reason `valid_server_name` is: it is one character
/// class and a leading-character rule, and neither a regex crate nor a new
/// dependency is warranted.
pub fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
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
    /// Maximum number of background pipelines that can run concurrently.
    /// When this limit is reached, `Ctrl+B` is rejected with a status message.
    pub max_background_tasks: usize,
    /// Number of newest turns replayed into the transcript when `--resume`
    /// opens an existing session.
    ///
    /// Mirrors agenthicc's `behaviour.resume_transcript_turns`. This bounds
    /// ONLY the visual transcript: nothing is deleted from the durable session
    /// log, and the agent still receives full provider-side history. `0` keeps
    /// the complete-history behaviour (agenthicc: "`0` keeps the
    /// complete-history behavior").
    ///
    /// Read by `--resume` in `crates/xaft-tui/src/app.rs` through
    /// `xaft_tui::replay::project_resume_transcript`.
    #[serde(default = "default_resume_transcript_turns")]
    pub resume_transcript_turns: usize,

    // ── Markdown rendering (F26) ───────────────────────────────────────────────
    /// Render CommonMark Markdown in agent output. When `false`, raw
    /// Markdown syntax is displayed.
    pub render_markdown: bool,
    /// Emit a 60-cell `─` separator after H1 headings.
    pub markdown_h1_separator: bool,
    /// Maximum column width (chars) in rendered tables.
    pub markdown_table_max_col_width: usize,
    /// Maximum number of data rows rendered in a table; excess rows are
    /// replaced with an "N more rows omitted" notice.
    pub markdown_table_max_rows: usize,
    /// When `true`, block-quote lines are prefixed with a dim `│ `.
    pub markdown_blockquote_bar: bool,

    // ── agenthicc `BehaviourSettings` parity ──
    //
    // agenthicc's `[behaviour]` is the same `[tui]` section; these are its two
    // keys.
    /// Verbose diagnostics: tool timings, resolved providers, policy decisions.
    ///
    /// Parity with agenthicc's `BehaviourSettings.verbose`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub verbose: bool,
    /// Ask before quitting an in-progress session.
    ///
    /// Parity with agenthicc's `BehaviourSettings.confirm_exits`. Default-on:
    /// what this guards is an unsaved session, and the cost of a spurious
    /// prompt is one keystroke.
    #[serde(default = "bool_true", skip_serializing_if = "is_true")]
    pub confirm_exits: bool,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    // ── agenthicc `PluginSettings` parity ──
    //
    // The four fields above predate agenthicc's `PluginSettings`, which is the
    // same `[plugins]` section. Its nine keys live here rather than in a second
    // struct so the wire name stays `plugins`.
    /// Trust a plugin the first time it is seen, without prompting.
    ///
    /// Parity with agenthicc's `PluginSettings.auto_trust`, documented there as
    /// the CI/CD escape hatch. It still records what it trusted in
    /// [`Self::trust_file`], so the decision remains auditable afterwards.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_trust: bool,
    /// Install missing plugin dependencies automatically.
    ///
    /// Parity with agenthicc's `PluginSettings.auto_install`. Off by default,
    /// because it makes loading a config file run package installation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_install: bool,
    /// Where [`Self::auto_install`] puts installed packages.
    ///
    /// Parity with agenthicc's `PluginSettings.install_target`. Free-form,
    /// because the valid values belong to the installer rather than to xaft.
    #[serde(
        default = "default_install_target",
        skip_serializing_if = "is_default_install_target"
    )]
    pub install_target: String,
    /// Importable modules a plugin may depend on.
    ///
    /// Parity with agenthicc's `PluginSettings.allowed_modules`. Empty means
    /// *no* module is allowed rather than "all are": a plugin runs with the
    /// user's permissions, so the default is a closed list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_modules: Vec<String>,
    /// Seconds a plugin may run before it is killed.
    ///
    /// Parity with agenthicc's `PluginSettings.timeout_seconds`. `0.0` means
    /// unbounded, matching agenthicc's treatment of an HTTP timeout.
    #[serde(
        default = "default_plugin_timeout",
        skip_serializing_if = "is_default_plugin_timeout"
    )]
    pub timeout_seconds: f64,
    /// Plugins that must not load, by name.
    ///
    /// Parity with agenthicc's `PluginSettings.disabled`. Checked before every
    /// other rule, so a disabled plugin cannot be re-enabled by `auto_trust`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
    /// Where trust decisions are recorded.
    ///
    /// Parity with agenthicc's `PluginSettings.trust_file`.
    #[serde(
        default = "default_trust_file",
        skip_serializing_if = "is_default_trust_file"
    )]
    pub trust_file: PathBuf,
    /// Where plugin loads are journaled.
    ///
    /// Parity with agenthicc's `PluginSettings.audit_file`. One JSON object per
    /// line, so a reader can stream a long history.
    #[serde(
        default = "default_audit_file",
        skip_serializing_if = "is_default_audit_file"
    )]
    pub audit_file: PathBuf,
    /// Fail instead of warn when a plugin shadows a built-in command.
    ///
    /// Parity with agenthicc's `PluginSettings.strict_cli_shadow`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub strict_cli_shadow: bool,
}

impl Default for PluginConfig {
    /// Matches agenthicc's `PluginSettings` dataclass defaults.
    ///
    /// Written out rather than derived because the agenthicc-parity fields
    /// carry non-zero defaults. A derived `Default` would give
    /// `install_target = ""` and `timeout_seconds = 0.0`, which are *not*
    /// agenthicc's `"venv"` and `30.0` -- so `XaftConfig::default()` (built
    /// from this) would disagree with a config that merely omits `[plugins]`
    /// (deserialized through the per-field `#[serde(default = ...)]` helpers).
    /// One `Default` implementation keeps those two paths from diverging.
    fn default() -> Self {
        Self {
            search_paths: Vec::new(),
            allow_dynamic: false,
            allow_wasm: false,
            security: PluginSecurityConfig::default(),
            auto_trust: false,
            auto_install: false,
            install_target: default_install_target(),
            allowed_modules: Vec::new(),
            timeout_seconds: default_plugin_timeout(),
            disabled: Vec::new(),
            trust_file: default_trust_file(),
            audit_file: default_audit_file(),
            strict_cli_shadow: false,
        }
    }
}

/// agenthicc's `PluginSettings.install_target` default (`config.py`).
fn default_install_target() -> String {
    "venv".to_string()
}

fn default_plugin_timeout() -> f64 {
    30.0
}

fn default_trust_file() -> PathBuf {
    PathBuf::from(".agenthicc/trusted_plugins.json")
}

fn default_audit_file() -> PathBuf {
    PathBuf::from(".agenthicc/plugin_audit.jsonl")
}

/// serde `skip_serializing_if` helpers for the agenthicc-parity fields.
///
/// Present so a config that never set these keys does not gain a dozen
/// explicit defaults on the way back out -- writing back `auto_trust = false`
/// that the user never asked for makes the file look edited.
fn is_false(value: &bool) -> bool {
    !*value
}

fn is_default_install_target(value: &String) -> bool {
    value == "venv"
}

fn is_default_plugin_timeout(value: &f64) -> bool {
    *value == 30.0
}

fn is_default_trust_file(value: &PathBuf) -> bool {
    value == &PathBuf::from(".agenthicc/trusted_plugins.json")
}

fn is_default_audit_file(value: &PathBuf) -> bool {
    value == &PathBuf::from(".agenthicc/plugin_audit.jsonl")
}

/// Plugin security settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// agenthicc's `McpServerConfig.reconnect_attempts` default.
fn default_reconnect_attempts() -> u32 {
    3
}

/// agenthicc's `McpServerConfig.reconnect_delay_seconds` default.
fn default_reconnect_delay() -> f64 {
    1.0
}

/// agenthicc's `McpServerConfig.transport` default (`agenthicc/tools/mcp.py`).
fn default_transport_stdio() -> String {
    "stdio".to_string()
}

/// agenthicc's `BehaviourSettings.resume_transcript_turns` default
/// (`agenthicc/config.py`: `resume_transcript_turns: int = 20`).
fn default_resume_transcript_turns() -> usize {
    20
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

// ── CompactionConfig ──────────────────────────────────────────────────────────

/// Context-window compaction settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    /// Enable automatic context compaction.
    pub enabled: bool,
    /// Trigger compaction when `input_tokens / context_window ≥ threshold_pct / 100`.
    /// Range: 1–99.  Default: 80.
    pub threshold_pct: u8,
    /// Number of complete user + assistant turns to keep verbatim after compaction.
    /// Default: 4.
    pub keep_recent_turns: usize,
    /// Maximum tokens for the summary LLM call.  Default: 1024.
    pub summary_max_tokens: usize,
    /// Agent preset to use for the summarisation call.
    /// Defaults to the fast tier if configured, else "default".
    pub summary_agent_preset: Option<String>,
}
