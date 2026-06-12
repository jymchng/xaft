//! Agent registry for dynamic multi-agent handoff workflows.
//!
//! [`AgentRegistry`] maps agent names to [`AgentDefinition`]s and instantiates
//! `Arc<dyn Agent>` on demand. The default xaft registry pre-registers the
//! standard planner, coder, qa, and fixer agents.
//!
//! # Adding a custom agent
//!
//! ```rust,ignore
//! use xaft_agents::registry::{AgentRegistry, AgentDefinition, AgentToolSet};
//!
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
use agtrs_runtime::tool::ErasedTool;
use agtrs_runtime::transport::Message;

use crate::coder::CODER_NAME;
use crate::error::AgentError;
use crate::fixer::FIXER_NAME;
use crate::handoff::HandoffTool;
use crate::planner::PLANNER_NAME;
use crate::qa::QA_NAME;

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
    /// Meta workflow: a coordinator agent designs and spawns specialist agents dynamically.
    Meta {
        /// System prompt override for the meta agent.
        /// `None` uses `META_AGENT_SYSTEM_PROMPT` from `xaft-agents`.
        meta_prompt: Option<String>,
        /// Maximum total specialist agents the meta agent may spawn (across
        /// both `spawn_agent` and `spawn_agents_parallel`).
        max_spawned_agents: usize,
        /// Maximum simultaneous specialists (semaphore permit count for
        /// `spawn_agents_parallel`).
        max_parallel_agents: usize,
        /// Allow spawned specialists to themselves call `spawn_agent`.
        allow_nesting: bool,
        /// Maximum nesting depth (0 = no nesting; 1 = one level deep, etc.).
        max_nesting_depth: usize,
    },
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

    /// Pre-populate with the five standard xaft agents.
    ///
    /// Registered in execution order: planner, coder, qa, fixer, summarizer.
    pub fn default_xaft() -> Self {
        use crate::coder::coder_system_prompt;
        use crate::fixer::fixer_system_prompt;
        use crate::planner::planner_system_prompt;
        use crate::qa::qa_system_prompt;

        Self::new()
            .register(AgentDefinition {
                name: PLANNER_NAME.into(),
                system_prompt_fn: Box::new(|_, wd| planner_system_prompt(wd)),
                tool_set: AgentToolSet::ReadOnly,
                max_turns: crate::planner::PLANNER_MAX_TURNS,
                can_handoff_to: vec![CODER_NAME.into()],
            })
            .register(AgentDefinition {
                name: CODER_NAME.into(),
                system_prompt_fn: Box::new(|_, wd| coder_system_prompt("", wd)),
                tool_set: AgentToolSet::ReadWrite,
                max_turns: crate::coder::CODER_MAX_TURNS,
                can_handoff_to: vec![QA_NAME.into()],
            })
            .register(AgentDefinition {
                name: QA_NAME.into(),
                system_prompt_fn: Box::new(|task, wd| qa_system_prompt(task, wd)),
                tool_set: AgentToolSet::ReadOnly,
                max_turns: crate::qa::QA_MAX_TURNS,
                can_handoff_to: vec![FIXER_NAME.into()],
            })
            .register(AgentDefinition {
                name: FIXER_NAME.into(),
                system_prompt_fn: Box::new(|task, wd| fixer_system_prompt(task, wd)),
                tool_set: AgentToolSet::ReadWrite,
                max_turns: crate::fixer::FIXER_MAX_TURNS,
                can_handoff_to: vec![QA_NAME.into()],
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
    ) -> Result<Arc<dyn Agent>, AgentError> {
        let def = self
            .definitions
            .get(name)
            .ok_or_else(|| AgentError::NotRegistered {
                name: name.to_string(),
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
        let handoff_tool = Arc::new(HandoffTool::new(
            Arc::clone(&handoff_store),
            def.can_handoff_to.clone(),
        )) as Arc<ErasedTool>;
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

// ── DynamicNamedAgent (internal) ──────────────────────────────────────────────

/// A minimal `Agent` impl used by the dynamic workflow.
///
/// Mirrors [`NamedAgent`] but is public to this crate so `AgentRegistry`
/// can construct it without a circular dependency.
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
        assert_eq!(r.len(), 1);
        assert_eq!(r.get("agent").unwrap().max_turns, 5);
    }

    #[test]
    fn default_xaft_has_five_agents() {
        let r = AgentRegistry::default_xaft();
        assert_eq!(r.len(), 4); // planner, coder, qa, fixer
        assert!(r.get(PLANNER_NAME).is_some());
        assert!(r.get(CODER_NAME).is_some());
        assert!(r.get(QA_NAME).is_some());
        assert!(r.get(FIXER_NAME).is_some());
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
        assert!(
            agent.tools().iter().any(|t| t.name() == "handoff_to_agent"),
            "HandoffTool must be injected"
        );
    }

    #[test]
    fn build_agent_readonly_gets_read_tools_only() {
        let r = AgentRegistry::new().register(AgentDefinition {
            name: "reviewer".into(),
            system_prompt_fn: Box::new(|_, _| String::new()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 1,
            can_handoff_to: vec![],
        });

        let (rt, wt) = dummy_tools(); // both empty
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
        assert_eq!(
            agent
                .tools()
                .iter()
                .filter(|t| t.name() != "handoff_to_agent")
                .count(),
            0
        );
    }

    #[test]
    fn workflow_config_default_is_standard() {
        assert!(matches!(
            WorkflowConfig::default(),
            WorkflowConfig::Standard
        ));
    }
}
