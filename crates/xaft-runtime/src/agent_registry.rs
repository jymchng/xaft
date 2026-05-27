//! Agent registry for dynamic multi-agent handoff workflows.
//!
//! [`AgentRegistry`] maps agent names to [`AgentDefinition`]s and instantiates
//! `Arc<dyn Agent>` on demand. The default xaft registry pre-registers the
//! standard planner, coder, qa, and fixer agents.
//!
//! # Adding a custom agent
//!
//! ```rust,ignore
//! let registry = AgentRegistry::default_xaft()
//!     .register(AgentDefinition {
//!         name: "db_migrator".into(),
//!         system_prompt_fn: Box::new(|task, _wd| {
//!             format!("You are a DB migration specialist. Task: {task}")
//!         }),
//!         tool_set: AgentToolSet::ReadWrite,
//!         max_turns: 15,
//!         can_handoff_to: vec!["qa".into()],
//!     });
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use agtrs_runtime::agent::{Agent, AgentConfig};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmResponse;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::team::HandoffAgentStore;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_runtime::transport::Message;

use crate::error::RuntimeError;

// ── Tool-set declaration ──────────────────────────────────────────────────────

/// Which filesystem tool category an agent receives.
///
/// Passed to [`AgentRegistry::build_agent`] which resolves the concrete
/// `Arc<ErasedTool>` slice from the caller's `read_tools` / `write_tools`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentToolSet {
    /// Only list/read/grep tools — safe for reviewers.
    ReadOnly,
    /// Full edit/write/bash suite — used by coders and fixers.
    ReadWrite,
    /// Caller-supplied subset by tool name. Matched against tool.name() at
    /// build time; unknown names are silently ignored.
    Custom(Vec<String>),
}

// ── AgentDefinition ───────────────────────────────────────────────────────────

/// Everything needed to instantiate a named agent on demand.
pub struct AgentDefinition {
    /// Unique agent name used as the routing key.
    pub name: String,
    /// Produces the system prompt at instantiation time.
    ///
    /// Receives `(task, working_dir)` so prompts can embed runtime context
    /// without being pre-computed at registry build time.
    pub system_prompt_fn: Box<dyn Fn(&str, &str) -> String + Send + Sync>,
    /// Which tools the agent receives.
    pub tool_set: AgentToolSet,
    /// Maximum LLM turns per execution.
    pub max_turns: usize,
    /// Agents this agent is allowed to hand off to.
    ///
    /// Empty = any registered agent is a valid target.
    pub can_handoff_to: Vec<String>,
}

impl std::fmt::Debug for AgentDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentDefinition")
            .field("name", &self.name)
            .field("tool_set", &self.tool_set)
            .field("max_turns", &self.max_turns)
            .field("can_handoff_to", &self.can_handoff_to)
            .finish()
    }
}

// ── AgentRegistry ─────────────────────────────────────────────────────────────

/// Registry of all agents available to a dynamic handoff workflow.
///
/// Preserves insertion order (used when iterating agent names for the
/// `HandoffOrchestrator` builder).
#[derive(Debug, Default)]
pub struct AgentRegistry {
    /// Preserves insertion order so the orchestrator sees agents in a
    /// deterministic sequence.
    order: Vec<String>,
    definitions: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-populate with the four standard xaft agents.
    ///
    /// Registered in execution order: coder, qa, fixer (planner is handled
    /// separately by the planning step, not through the handoff loop).
    pub fn default_xaft() -> Self {
        Self::new()
            .register(AgentDefinition {
                name: "coder".into(),
                system_prompt_fn: Box::new(|_task, wd| {
                    format!(
                        "You are an expert software engineer.\n\
                         WORKING DIRECTORY: {wd}\nUse relative paths."
                    )
                }),
                tool_set: AgentToolSet::ReadWrite,
                max_turns: 40,
                can_handoff_to: vec!["qa".into()],
            })
            .register(AgentDefinition {
                name: "qa".into(),
                system_prompt_fn: Box::new(|task, wd| {
                    format!(
                        "You are a code reviewer. Task: {task}\n\
                         WORKING DIRECTORY: {wd}\n\
                         Read key files. Output APPROVED or call handoff_to_agent \
                         with target=fixer and a detailed issues summary."
                    )
                }),
                tool_set: AgentToolSet::ReadOnly,
                max_turns: 25,
                can_handoff_to: vec!["fixer".into()],
            })
            .register(AgentDefinition {
                name: "fixer".into(),
                system_prompt_fn: Box::new(|task, wd| {
                    format!(
                        "You are a bug fixer. Task: {task}\n\
                         WORKING DIRECTORY: {wd}\n\
                         Fix all reported issues using write_file. \
                         After fixing, call handoff_to_agent with target=qa."
                    )
                }),
                tool_set: AgentToolSet::ReadWrite,
                max_turns: 25,
                can_handoff_to: vec!["qa".into()],
            })
    }

    /// Register (or replace) an agent definition.
    pub fn register(mut self, def: AgentDefinition) -> Self {
        let name = def.name.clone();
        if !self.definitions.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.definitions.insert(name, def);
        self
    }

    /// Ordered list of registered agent names.
    pub fn agent_names(&self) -> &[String] {
        &self.order
    }

    /// Look up a definition by name.
    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.definitions.get(name)
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    /// `true` if no agents are registered.
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// Instantiate a ready-to-run `Arc<dyn Agent>` for the named agent.
    ///
    /// `HandoffTool` is injected automatically for every agent so they can
    /// hand off to each other.  The `allowed_targets` list comes from
    /// [`AgentDefinition::can_handoff_to`] (empty = any).
    pub fn build_agent(
        &self,
        name: &str,
        task: &str,
        working_dir: &str,
        read_tools: &[Arc<ErasedTool>],
        write_tools: &[Arc<ErasedTool>],
        handoff_store: Arc<HandoffAgentStore>,
        signals: Arc<SignalBus>,
    ) -> Result<Arc<dyn Agent>, RuntimeError> {
        let def = self.definitions.get(name).ok_or_else(|| {
            RuntimeError::Agent(format!("agent '{name}' not registered in AgentRegistry"))
        })?;

        let system_prompt = (def.system_prompt_fn)(task, working_dir);

        // Resolve the concrete tool list.
        let mut tools: Vec<Arc<ErasedTool>> = match &def.tool_set {
            AgentToolSet::ReadOnly => read_tools.to_vec(),
            AgentToolSet::ReadWrite => write_tools.to_vec(),
            AgentToolSet::Custom(names) => {
                let all: Vec<Arc<ErasedTool>> = read_tools
                    .iter()
                    .chain(write_tools.iter())
                    .cloned()
                    .collect();
                all.into_iter()
                    .filter(|t| names.iter().any(|n| n == t.name()))
                    .collect()
            }
        };

        // Inject HandoffTool so the agent can hand off to its allowed targets.
        let handoff_tool = Arc::new(HandoffTool {
            triggered: None,
            store: Arc::clone(&handoff_store),
            allowed_targets: def.can_handoff_to.clone(),
        }) as Arc<ErasedTool>;
        tools.push(handoff_tool);

        let agent = DynamicNamedAgent {
            name: name.to_string(),
            config: AgentConfig {
                system_prompt,
                max_turns: def.max_turns,
                strict_capability_check: false,
                parallel_tool_calls: false,
                ..Default::default()
            },
            tools,
            signals: Some(Arc::clone(&signals)),
        };

        Ok(Arc::new(agent))
    }
}

// ── HandoffTool ───────────────────────────────────────────────────────────────

/// Generic handoff tool injected into every agent in a dynamic workflow.
///
/// When the LLM calls `handoff_to_agent`, the tool writes to the shared
/// [`HandoffAgentStore`] so [`HandoffOrchestrator`] can switch agents
/// after the current turn finishes.
///
/// [`HandoffOrchestrator`]: agtrs_runtime::team::HandoffOrchestrator
pub struct HandoffTool {
    pub(crate) store: Arc<HandoffAgentStore>,
    /// Allowed target agent names.  Empty slice = any registered agent.
    pub(crate) allowed_targets: Vec<String>,
    /// Set to `true` when this tool fires.  Shared with the owning [`NamedAgent`]
    /// so `before_llm_call` can abort the next LLM call, preventing the agent
    /// from looping after a successful handoff.
    pub(crate) triggered: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl HandoffTool {
    /// Create a new `HandoffTool` with a specific set of allowed targets.
    ///
    /// Pass an empty `allowed_targets` to allow handoffs to any agent.
    pub fn new(store: Arc<HandoffAgentStore>, allowed_targets: Vec<String>) -> Self {
        Self {
            store,
            allowed_targets,
            triggered: None,
        }
    }

    /// Create a `HandoffTool` that also sets `flag` to `true` when it fires.
    ///
    /// The flag must be shared with the owning agent so `before_llm_call` can
    /// detect the handoff and abort the next LLM call.
    pub fn new_with_flag(
        store: Arc<HandoffAgentStore>,
        allowed_targets: Vec<String>,
        flag: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            store,
            allowed_targets,
            triggered: Some(flag),
        }
    }
}

impl std::fmt::Debug for HandoffTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoffTool")
            .field("allowed_targets", &self.allowed_targets)
            .finish()
    }
}

#[async_trait::async_trait]
impl Tool for HandoffTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "handoff_to_agent"
    }

    fn description(&self) -> &str {
        "Transfer the conversation to another agent when the current task \
         is better handled by a specialist. Provide a concise reason."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target_agent": {
                    "type": "string",
                    "description": "Name of the agent to hand off to."
                },
                "reason": {
                    "type": "string",
                    "description": "Why this handoff is needed. The next agent will receive this as context."
                }
            },
            "required": ["target_agent", "reason"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let target = input["target_agent"].as_str().unwrap_or("").to_string();
        let reason = input["reason"]
            .as_str()
            .unwrap_or("Agent handoff")
            .to_string();

        if target.is_empty() {
            return Ok(ToolResult::ok(
                "handoff failed: target_agent is required".to_string(),
                &ctx.tool_use_id,
            ));
        }

        // Validate against allowed_targets when the list is non-empty.
        if !self.allowed_targets.is_empty() && !self.allowed_targets.contains(&target) {
            return Ok(ToolResult::ok(
                format!(
                    "handoff to '{target}' not permitted. Allowed: {:?}",
                    self.allowed_targets
                ),
                &ctx.tool_use_id,
            ));
        }

        let conv_id = ctx
            .state
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !conv_id.is_empty() {
            self.store.set_active_agent(&conv_id, &target).await;
            self.store.set_pending_summary(&conv_id, &reason).await;
        }

        // Signal the owning agent to terminate after this tool result so it
        // cannot loop calling handoff_to_agent a second time.
        if let Some(ref flag) = self.triggered {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }

        Ok(ToolResult::ok(
            format!(
                "Handoff to '{target}' initiated. \
                 Your task is complete — do not call any more tools.",
            ),
            &ctx.tool_use_id,
        ))
    }
}

// ── WorkflowConfig ────────────────────────────────────────────────────────────

/// Selects which orchestration strategy to use for a run.
///
/// Defaults to [`WorkflowConfig::Standard`] which preserves 100% backwards
/// compatibility with the existing plan→coder→QA↔Fixer pipeline.
#[derive(Debug, Clone, Default)]
pub enum WorkflowConfig {
    /// Classic fixed pipeline: plan → coder → QA ↔ fixer (default).
    #[default]
    Standard,
    /// Dynamic handoff: any registered agent can hand off to any other.
    Dynamic {
        /// Name of the first agent to run.
        initial_agent: String,
        /// Maximum total agent transitions (not full cycles).
        max_handoffs: usize,
        /// Restrict to a named subset of the registry (`None` = use all).
        agent_subset: Option<Vec<String>>,
    },
}

// ── DynamicNamedAgent (internal) ──────────────────────────────────────────────

/// A minimal `Agent` impl used by the dynamic workflow.
///
/// Mirrors `NamedAgent` in `orchestrator.rs` but is public to this crate so
/// `agent_registry.rs` can construct it without a circular dep.
pub(crate) struct DynamicNamedAgent {
    pub(crate) name: String,
    pub(crate) config: AgentConfig,
    pub(crate) tools: Vec<Arc<ErasedTool>>,
    pub(crate) signals: Option<Arc<SignalBus>>,
}

#[async_trait::async_trait]
impl Agent for DynamicNamedAgent {
    fn name(&self) -> &str {
        &self.name
    }
    fn system_prompt(&self) -> String {
        self.config.system_prompt.clone()
    }
    fn tools(&self) -> Vec<Arc<ErasedTool>> {
        self.tools.clone()
    }
    fn config(&self) -> &AgentConfig {
        &self.config
    }

    async fn before_llm_call(
        &self,
        _messages: &mut Vec<Message>,
        _options: &mut agtrs_runtime::llm::LlmOptions,
    ) -> Result<(), AgtrsError> {
        if let Some(ref bus) = self.signals {
            let bus = Arc::clone(bus);
            let agent_name = self.name.clone();
            tokio::spawn(async move {
                bus.emit(xaft_agent::signals::XaftLlmCallStarting {
                    agent_name,
                    call_index: 0,
                })
                .await;
            });
        }
        Ok(())
    }

    async fn after_llm_call(&self, response: &LlmResponse) -> Result<(), AgtrsError> {
        if let Some(ref bus) = self.signals {
            let text = response.message.text();
            if !text.trim().is_empty() {
                let bus = Arc::clone(bus);
                let agent_name = self.name.clone();
                tokio::spawn(async move {
                    bus.emit(xaft_agent::XaftAgentOutput {
                        agent_name,
                        content: text,
                    })
                    .await;
                });
            }
        }
        Ok(())
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_tools() -> (Vec<Arc<ErasedTool>>, Vec<Arc<ErasedTool>>) {
        (vec![], vec![])
    }

    fn dummy_store() -> Arc<HandoffAgentStore> {
        Arc::new(HandoffAgentStore::new())
    }

    fn dummy_signals() -> Arc<SignalBus> {
        Arc::new(SignalBus::new())
    }

    #[test]
    fn registry_starts_empty() {
        let r = AgentRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn register_and_lookup() {
        let r = AgentRegistry::new().register(AgentDefinition {
            name: "my_agent".into(),
            system_prompt_fn: Box::new(|_, _| "You are my agent.".into()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 5,
            can_handoff_to: vec![],
        });
        assert_eq!(r.len(), 1);
        assert!(r.get("my_agent").is_some());
        assert!(r.get("unknown").is_none());
    }

    #[test]
    fn agent_names_preserves_insertion_order() {
        let r = AgentRegistry::new()
            .register(AgentDefinition {
                name: "a".into(),
                system_prompt_fn: Box::new(|_, _| String::new()),
                tool_set: AgentToolSet::ReadOnly,
                max_turns: 1,
                can_handoff_to: vec![],
            })
            .register(AgentDefinition {
                name: "b".into(),
                system_prompt_fn: Box::new(|_, _| String::new()),
                tool_set: AgentToolSet::ReadOnly,
                max_turns: 1,
                can_handoff_to: vec![],
            });
        assert_eq!(r.agent_names(), &["a", "b"]);
    }

    #[test]
    fn register_replaces_existing() {
        let r = AgentRegistry::new()
            .register(AgentDefinition {
                name: "agent".into(),
                system_prompt_fn: Box::new(|_, _| "v1".into()),
                tool_set: AgentToolSet::ReadOnly,
                max_turns: 1,
                can_handoff_to: vec![],
            })
            .register(AgentDefinition {
                name: "agent".into(),
                system_prompt_fn: Box::new(|_, _| "v2".into()),
                tool_set: AgentToolSet::ReadWrite,
                max_turns: 5,
                can_handoff_to: vec![],
            });
        // len stays 1, not 2
        assert_eq!(r.len(), 1);
        assert_eq!(r.get("agent").unwrap().max_turns, 5);
    }

    #[test]
    fn default_xaft_has_three_agents() {
        let r = AgentRegistry::default_xaft();
        assert_eq!(r.len(), 3);
        assert!(r.get("coder").is_some());
        assert!(r.get("qa").is_some());
        assert!(r.get("fixer").is_some());
    }

    #[test]
    fn unknown_agent_build_returns_error() {
        let r = AgentRegistry::new();
        let (rt, wt) = dummy_tools();
        let result = r.build_agent(
            "ghost",
            "task",
            "/tmp",
            &rt,
            &wt,
            dummy_store(),
            dummy_signals(),
        );
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("ghost"),
            "error must mention agent name: {msg}"
        );
    }

    #[test]
    fn build_agent_injects_handoff_tool() {
        let r = AgentRegistry::new().register(AgentDefinition {
            name: "reviewer".into(),
            system_prompt_fn: Box::new(|_, _| "review code".into()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 5,
            can_handoff_to: vec!["fixer".into()],
        });
        let (rt, wt) = dummy_tools();
        let agent = r
            .build_agent(
                "reviewer",
                "task",
                "/tmp",
                &rt,
                &wt,
                dummy_store(),
                dummy_signals(),
            )
            .unwrap();
        // HandoffTool must be in the tool list
        assert!(
            agent.tools().iter().any(|t| t.name() == "handoff_to_agent"),
            "HandoffTool must be injected"
        );
    }

    #[test]
    fn build_agent_readonly_gets_read_tools_only() {
        // Build a registry with a read-only agent and supply both read and write tools.
        // The agent must NOT receive write tools.
        use agtrs_runtime::tool::Tool as AgtrsToolTrait;

        // We'll check that no write-named tool appears via the tool_set filtering.
        let r = AgentRegistry::new().register(AgentDefinition {
            name: "reviewer".into(),
            system_prompt_fn: Box::new(|_, _| String::new()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 1,
            can_handoff_to: vec![],
        });

        let (rt, wt) = dummy_tools(); // both empty in unit test
        let agent = r
            .build_agent(
                "reviewer",
                "task",
                "/tmp",
                &rt,
                &wt,
                dummy_store(),
                dummy_signals(),
            )
            .unwrap();
        // Only HandoffTool should be present (read tools slice is empty in unit test)
        assert_eq!(
            agent
                .tools()
                .iter()
                .filter(|t| t.name() != "handoff_to_agent")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn handoff_tool_sets_flag_on_call() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let store = Arc::new(HandoffAgentStore::new());
        let flag = Arc::new(AtomicBool::new(false));
        let tool =
            HandoffTool::new_with_flag(Arc::clone(&store), vec!["coder".into()], Arc::clone(&flag));
        let mut ctx = ToolContext::new("tid-flag");
        ctx.state
            .insert("conversation_id".into(), serde_json::json!("conv-flag"));

        assert!(!flag.load(Ordering::Acquire), "flag must start false");

        tool.call(
            serde_json::json!({"target_agent": "coder", "reason": "plan ready"}),
            &ctx,
        )
        .await
        .unwrap();

        assert!(
            flag.load(Ordering::Acquire),
            "flag must be true after handoff fires"
        );
        assert_eq!(
            store.get_active_agent("conv-flag").await,
            Some("coder".to_string())
        );
    }

    #[tokio::test]
    async fn handoff_tool_writes_to_store() {
        let store = Arc::new(HandoffAgentStore::new());
        let tool = HandoffTool {
            triggered: None,
            store: Arc::clone(&store),
            allowed_targets: vec!["fixer".into()],
        };
        let mut ctx = ToolContext::new("tid-1");
        ctx.state
            .insert("conversation_id".into(), serde_json::json!("conv-1"));

        let result = tool
            .call(
                serde_json::json!({"target_agent": "fixer", "reason": "found bugs"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(
            store.get_active_agent("conv-1").await,
            Some("fixer".to_string())
        );
        assert_eq!(
            store.get_and_clear_summary("conv-1").await,
            Some("found bugs".to_string())
        );
    }

    #[tokio::test]
    async fn handoff_tool_rejects_disallowed_target() {
        let store = Arc::new(HandoffAgentStore::new());
        let tool = HandoffTool {
            triggered: None,
            store: Arc::clone(&store),
            allowed_targets: vec!["fixer".into()],
        };
        let mut ctx = ToolContext::new("tid-2");
        ctx.state
            .insert("conversation_id".into(), serde_json::json!("conv-2"));

        let result = tool
            .call(
                serde_json::json!({"target_agent": "hacker", "reason": "escape"}),
                &ctx,
            )
            .await
            .unwrap();

        // Should NOT have set the active agent
        assert!(
            !result.is_error,
            "tool returns ok with error message inside"
        );
        assert!(
            result.content.contains("not permitted"),
            "response must say 'not permitted'"
        );
        assert_eq!(store.get_active_agent("conv-2").await, None);
    }

    #[tokio::test]
    async fn handoff_tool_empty_allowed_targets_permits_any() {
        let store = Arc::new(HandoffAgentStore::new());
        let tool = HandoffTool {
            triggered: None,
            store: Arc::clone(&store),
            allowed_targets: vec![], // empty = unrestricted
        };
        let mut ctx = ToolContext::new("tid-3");
        ctx.state
            .insert("conversation_id".into(), serde_json::json!("conv-3"));

        let result = tool
            .call(
                serde_json::json!({"target_agent": "any_agent", "reason": "reason"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(
            store.get_active_agent("conv-3").await,
            Some("any_agent".to_string())
        );
    }

    #[tokio::test]
    async fn handoff_tool_empty_conv_id_does_not_panic() {
        let store = Arc::new(HandoffAgentStore::new());
        let tool = HandoffTool {
            triggered: None,
            store: Arc::clone(&store),
            allowed_targets: vec![],
        };
        let ctx = ToolContext::new("tid-4"); // no conversation_id in state

        let result = tool
            .call(
                serde_json::json!({"target_agent": "agent", "reason": "test"}),
                &ctx,
            )
            .await
            .unwrap();

        // Must not panic; result can be success or error but must be Ok(...)
        assert!(!result.is_error);
    }

    #[test]
    fn workflow_config_default_is_standard() {
        assert!(matches!(
            WorkflowConfig::default(),
            WorkflowConfig::Standard
        ));
    }
}
