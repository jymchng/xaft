//! Tests for the meta workflow: WorkflowConfig::Meta, MetaWorkflowConfig,
//! and XaftAgentFactory.

use std::sync::Arc;

use agtrs_runtime::agent::AgentBlueprint;
use agtrs_runtime::meta::BlueprintContext;
use agtrs_runtime::signals::SignalBus;

// ── WorkflowConfig::Meta tests ────────────────────────────────────────────────

#[test]
fn workflow_config_meta_variant() {
    use xaft_runtime::WorkflowConfig;

    let config = WorkflowConfig::Meta {
        meta_prompt: None,
        max_spawned_agents: 8,
        max_parallel_agents: 4,
        allow_nesting: false,
        max_nesting_depth: 0,
    };

    assert!(matches!(config, WorkflowConfig::Meta { .. }));
    assert!(!matches!(config, WorkflowConfig::Standard));
    assert!(!matches!(config, WorkflowConfig::Dynamic { .. }));
}

#[test]
fn meta_config_defaults() {
    use xaft_runtime::WorkflowConfig;

    // Standard is still the default
    let default_config = WorkflowConfig::default();
    assert!(matches!(default_config, WorkflowConfig::Standard));
}

#[test]
fn meta_workflow_config_from_meta_variant() {
    use xaft_runtime::MetaWorkflowConfig;
    use xaft_runtime::WorkflowConfig;

    let config = WorkflowConfig::Meta {
        meta_prompt: Some("custom prompt".into()),
        max_spawned_agents: 6,
        max_parallel_agents: 3,
        allow_nesting: true,
        max_nesting_depth: 1,
    };

    let meta_cfg =
        MetaWorkflowConfig::from_workflow_config(&config).expect("should extract Meta config");

    assert_eq!(meta_cfg.meta_prompt, Some("custom prompt".into()));
    assert_eq!(meta_cfg.max_spawned_agents, 6);
    assert_eq!(meta_cfg.max_parallel_agents, 3);
    assert!(meta_cfg.allow_nesting);
    assert_eq!(meta_cfg.max_nesting_depth, 1);
}

#[test]
fn meta_workflow_config_returns_none_for_standard() {
    use xaft_runtime::MetaWorkflowConfig;
    use xaft_runtime::WorkflowConfig;

    let config = WorkflowConfig::Standard;
    let result = MetaWorkflowConfig::from_workflow_config(&config);
    assert!(result.is_none());
}

// ── XaftAgentFactory tests ────────────────────────────────────────────────────

#[tokio::test]
async fn xaft_agent_factory_rejects_unknown_tools() {
    use agtrs_runtime::meta::AgentFactory;
    use xaft_runtime::XaftAgentFactory;

    // Build factory with empty master tool set
    let factory = XaftAgentFactory::from_master_tools(
        vec![], // no tools available
        // We need a dummy LLM — use the testing mock if available
        {
            use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
            Arc::new(MockLlmProvider::new(Arc::new(MockTransport::new())))
        },
        Arc::new(SignalBus::new()),
        std::path::PathBuf::from("/tmp/test-workspace"),
        false,
    )
    .expect("factory creation should succeed");

    let blueprint = AgentBlueprint {
        name: "test-agent".into(),
        role: "Test".into(),
        system_prompt: "You are a test agent.".into(),
        tools: vec!["nonexistent_tool".into()],
        max_turns: 5,
        model: None,
        terminates_on: agtrs_runtime::agent::TerminationCondition::AnyResponse,
        is_terminal: false,
    };

    let ctx = BlueprintContext {
        task: "test task".into(),
        working_dir: "/tmp/test-workspace".into(),
        nesting_depth: 0,
        session_id: "test-session".into(),
    };

    // Should succeed (warn-on-unknown, fail-open per AC6)
    let agent = factory
        .create(&blueprint, &ctx)
        .await
        .expect("factory should succeed even with unknown tools");

    assert_eq!(agent.name(), "test-agent");
    // Agent should have zero tools (unknown names were silently skipped)
    assert!(agent.tools().is_empty());
}

#[tokio::test]
async fn xaft_agent_factory_caps_max_turns() {
    use agtrs_runtime::meta::AgentFactory;
    use xaft_runtime::XaftAgentFactory;

    let factory = XaftAgentFactory::from_master_tools(
        vec![],
        {
            use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
            Arc::new(MockLlmProvider::new(Arc::new(MockTransport::new())))
        },
        Arc::new(SignalBus::new()),
        std::path::PathBuf::from("/tmp/test-workspace"),
        false,
    )
    .expect("factory creation should succeed");

    let blueprint = AgentBlueprint {
        name: "expensive-agent".into(),
        role: "Test".into(),
        system_prompt: "You are a test agent.".into(),
        tools: vec![],
        max_turns: 999, // way above ceiling
        model: None,
        terminates_on: agtrs_runtime::agent::TerminationCondition::AnyResponse,
        is_terminal: false,
    };

    let ctx = BlueprintContext {
        task: "test task".into(),
        working_dir: "/tmp/test-workspace".into(),
        nesting_depth: 0,
        session_id: "test-session".into(),
    };

    let agent = factory
        .create(&blueprint, &ctx)
        .await
        .expect("factory should succeed");

    // The factory caps at max_turns_ceiling (50)
    assert!(agent.config().max_turns <= 50);
}

#[test]
fn workflow_config_meta_prompt_injection() {
    // Verify that a meta_prompt override round-trips correctly
    let meta_prompt = "Custom meta prompt: you are specialized.";

    use xaft_runtime::MetaWorkflowConfig;
    use xaft_runtime::WorkflowConfig;

    let config = WorkflowConfig::Meta {
        meta_prompt: Some(meta_prompt.into()),
        max_spawned_agents: 4,
        max_parallel_agents: 2,
        allow_nesting: false,
        max_nesting_depth: 0,
    };

    let meta_cfg = MetaWorkflowConfig::from_workflow_config(&config).unwrap();
    assert_eq!(meta_cfg.meta_prompt.as_deref(), Some(meta_prompt));
}
