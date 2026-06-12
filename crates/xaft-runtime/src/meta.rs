//! xaft implementation of `AgentFactory` for the meta-agent workflow.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, warn};

use agtrs_runtime::agent::{Agent, AgentBlueprint};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::meta::{AgentFactory, BlueprintContext};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::tool::ErasedTool;

use crate::error::RuntimeError;

/// xaft's implementation of [`AgentFactory`].
///
/// Resolves `AgentBlueprint::tools` names against a pre-built master tool set.
/// Unknown tool names are warned and silently skipped (fail-open), matching
/// the PRD AC6 requirement.
pub struct XaftAgentFactory {
    /// The full set of tools any specialist may draw from.
    /// Shared across all factory calls; individual specialists get a filtered subset.
    master_tools: Vec<Arc<ErasedTool>>,
    /// LLM provider forwarded to specialists.
    pub llm: Arc<dyn LlmProvider>,
    /// Signal bus for lifecycle events.
    pub signals: Arc<SignalBus>,
    /// Workspace root — used to validate tool paths.
    pub working_dir: PathBuf,
    /// Maximum `max_turns` any specialist may request.
    pub max_turns_ceiling: usize,
    /// Whether spawned agents are allowed to themselves spawn agents.
    pub allow_nesting: bool,
}

impl XaftAgentFactory {
    /// Create a factory from a pre-built master tool list.
    ///
    /// The `master_tools` list should contain the union of all tools any
    /// specialist may need (read + write tools). Specialists receive a
    /// filtered subset based on their blueprint.
    pub fn from_master_tools(
        master_tools: Vec<Arc<ErasedTool>>,
        llm: Arc<dyn LlmProvider>,
        signals: Arc<SignalBus>,
        working_dir: PathBuf,
        allow_nesting: bool,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            master_tools,
            llm,
            signals,
            working_dir,
            max_turns_ceiling: 50,
            allow_nesting,
        })
    }

    /// Filter `master_tools` to only those whose `Tool::name()` matches
    /// names in `blueprint.tools`. Unknown names are warned and skipped.
    fn resolve_tools(&self, blueprint: &AgentBlueprint) -> Vec<Arc<ErasedTool>> {
        let mut resolved = Vec::new();
        for name in &blueprint.tools {
            if let Some(tool) = self.master_tools.iter().find(|t| t.name() == name.as_str()) {
                resolved.push(Arc::clone(tool));
            } else {
                warn!(
                    agent = %blueprint.name,
                    tool = %name,
                    "XaftAgentFactory: tool not found in master set, skipping"
                );
            }
        }
        resolved
    }
}

#[async_trait]
impl AgentFactory for XaftAgentFactory {
    async fn create(
        &self,
        blueprint: &AgentBlueprint,
        ctx: &BlueprintContext,
    ) -> Result<Arc<dyn Agent>, AgtrsError> {
        // Nesting guard
        if !self.allow_nesting && ctx.nesting_depth > 0 {
            return Err(AgtrsError::ToolCallFailed {
                tool_name: "spawn_agent".into(),
                reason: "XaftAgentFactory: nesting is disabled".into(),
            });
        }

        // Resolve system prompt placeholders
        let system_prompt = blueprint
            .system_prompt
            .replace("{{task}}", &ctx.task)
            .replace("{{working_dir}}", &ctx.working_dir);

        // Cap max_turns
        let max_turns = blueprint.max_turns.min(self.max_turns_ceiling);
        if blueprint.max_turns > self.max_turns_ceiling {
            warn!(
                agent = %blueprint.name,
                requested = blueprint.max_turns,
                capped = self.max_turns_ceiling,
                "XaftAgentFactory: max_turns capped at ceiling"
            );
        }

        // Resolve tools (warn on unknown, but don't error — AC6)
        let tools = self.resolve_tools(blueprint);
        debug!(
            agent = %blueprint.name,
            tool_count = tools.len(),
            "XaftAgentFactory: resolved tools for specialist"
        );

        // Build the specialist agent using xaft_agents::named::NamedAgent
        let agent = xaft_agents::named::NamedAgent::new(&blueprint.name, &system_prompt, max_turns)
            .with_tools(tools)
            .with_signals(Arc::clone(&self.signals));

        Ok(Arc::new(agent))
    }

    fn supports_tool(&self, tool_name: &str) -> bool {
        self.master_tools.iter().any(|t| t.name() == tool_name)
    }

    fn available_tools(&self) -> Vec<String> {
        self.master_tools
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }
}

/// Configuration for the meta workflow, extracted from `WorkflowConfig::Meta`.
pub struct MetaWorkflowConfig {
    /// System prompt override for the meta agent; `None` uses the built-in prompt.
    pub meta_prompt: Option<String>,
    /// Maximum total specialist agents the meta agent may spawn.
    pub max_spawned_agents: usize,
    /// Maximum simultaneous specialists.
    pub max_parallel_agents: usize,
    /// Whether spawned agents are allowed to themselves spawn.
    pub allow_nesting: bool,
    /// Maximum nesting depth.
    pub max_nesting_depth: usize,
}

impl Default for MetaWorkflowConfig {
    fn default() -> Self {
        Self {
            meta_prompt: None,
            max_spawned_agents: 8,
            max_parallel_agents: 4,
            allow_nesting: false,
            max_nesting_depth: 1,
        }
    }
}

impl MetaWorkflowConfig {
    /// Extract from a `WorkflowConfig::Meta` variant.
    ///
    /// Panics if called on a non-Meta variant (internal invariant).
    pub fn from_workflow_config(config: &crate::agent_registry::WorkflowConfig) -> Option<Self> {
        match config {
            crate::agent_registry::WorkflowConfig::Meta {
                meta_prompt,
                max_spawned_agents,
                max_parallel_agents,
                allow_nesting,
                max_nesting_depth,
            } => Some(Self {
                meta_prompt: meta_prompt.clone(),
                max_spawned_agents: *max_spawned_agents,
                max_parallel_agents: *max_parallel_agents,
                allow_nesting: *allow_nesting,
                max_nesting_depth: *max_nesting_depth,
            }),
            _ => None,
        }
    }
}
