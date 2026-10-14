# xaft-agents crate PRD

## Problem

Domain agent logic is scattered and duplicated:

- `xaft-agent` has a **generic** `CODER_SYSTEM_PROMPT` constant and `AgentRole` enum
- `xaft-runtime/src/orchestrator.rs` has **specific** private `coder_prompt()`, `qa_prompt()`,
  `fixer_prompt()` functions, `NamedAgent` struct, `EditSummary`, and `RequestFixTool` — all
  mixed into a 500-line orchestration file
- There is no typed return value for QA verdict or fixer outcome
- Adding a new agent requires editing `orchestrator.rs`, not a dedicated module
- The `AgentRegistry` described in PRD 20 (dynamic handoffs) has nowhere to live

## Goal

Create `crates/xaft-agents` — a dedicated crate for **domain agent definitions**.
Every named agent (planner, coder, QA, fixer, summarizer) is its own module with:

- Owned system prompt builder (runtime parameters: task, working_dir, plan)
- Typed return struct (`EditSummary`, `QaVerdict`, `FixSummary`, …)
- Tool-set declaration (`ReadOnly`, `ReadWrite`, or a named subset)
- Default `max_turns`
- A builder that produces a `SubagentTool<R>` or `Arc<dyn Agent>` ready for use

`xaft-runtime` imports from `xaft-agents` and its `orchestrator.rs` shrinks to pure
**orchestration plumbing**, not agent content.

---

## Relationship to existing crates

```
xaft-agent          ← low-level: XaftAgent, PlanModeAgent, AgentBuilder, StreamSink
xaft-agents (NEW)   ← domain:    planner, coder, qa, fixer, summarizer + AgentRegistry
xaft-runtime        ← orchestration: imports from both
```

`xaft-agents` depends on `xaft-agent`, `xaft-tools`, `agtrs-runtime`.
`xaft-runtime` depends on `xaft-agents` (replaces the private `NamedAgent` + prompts).

---

## Crate layout

```
crates/xaft-agents/
├── Cargo.toml
└── src/
    ├── lib.rs           — re-exports; crate-level doc
    ├── named.rs         — NamedAgent (moved from orchestrator.rs)
    ├── registry.rs      — AgentRegistry + AgentDefinition (from PRD 20)
    ├── handoff.rs       — HandoffTool (generic, from PRD 20)
    ├── planner.rs       — planner agent module
    ├── coder.rs         — coder agent + EditSummary
    ├── qa.rs            — QA agent + QaVerdict + RequestFixTool
    ├── fixer.rs         — fixer agent + FixSummary
    └── summarizer.rs    — concluding-summary agent
```

---

## Module specifications

### `named.rs` — base agent struct

Move `NamedAgent` (currently private in `orchestrator.rs`) here verbatim and make it
`pub`. Only change: replace the `signals` field with a method (signals stay in the
builder not the struct, so we don't break the agtrs `Agent` trait).

```rust
pub struct NamedAgent {
    pub name: String,
    pub(crate) config: AgentConfig,
    pub(crate) tools: Vec<Arc<ErasedTool>>,
}

impl NamedAgent {
    pub fn new(name: impl Into<String>, system_prompt: impl Into<String>, max_turns: usize) -> Self { ... }
    pub fn with_tools(mut self, tools: Vec<Arc<ErasedTool>>) -> Self { ... }
    pub fn with_signals(mut self, signals: Arc<SignalBus>) -> Self { ... }
}

impl Agent for NamedAgent { ... }  // unchanged from current impl
```

---

### `coder.rs` — coder agent

```rust
/// Return type from the coder SubagentTool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditSummary {
    pub files_changed: Vec<String>,
    pub description: String,
    pub tests_passed: bool,
    pub notes: String,
}

/// Render the coder system prompt. Takes the planner's output as `plan_text`.
pub fn coder_system_prompt(plan_text: &str, working_dir: &str) -> String { ... }

/// Build a ready-to-run `SubagentTool<EditSummary>`.
pub fn build_coder_tool(
    plan_text: &str,
    working_dir: &str,
    write_tools: Vec<Arc<ErasedTool>>,
    llm: Arc<dyn LlmProvider>,
    resolve_ctx: Arc<ResolveContext>,
    signals: Arc<SignalBus>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
) -> SubagentTool<EditSummary> {
    SubagentTool::<EditSummary>::builder()
        .name(CODER_NAME)
        .description("Apply the coding plan to the workspace")
        .subagent(Arc::new(NamedAgent::new(...)) as Arc<dyn Agent>)
        .llm(llm)
        .resolve_ctx(resolve_ctx)
        .system_prompt(&coder_system_prompt(plan_text, working_dir))
        .max_turns(40)
        .return_mode(ReturnMode::StructuredLlm)
        .signals(signals)
        .with_approval_gate_opt(approval_gate)
        .build()
}
```

The system prompt is extracted verbatim from `orchestrator.rs::coder_prompt()`.

---

### `qa.rs` — QA agent

```rust
/// What the QA agent decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QaVerdict { Approved, RequestFix(String) }

/// Tool the QA agent calls to trigger a fixer handoff.
pub struct RequestFixTool { store: Arc<HandoffAgentStore> }

impl Tool for RequestFixTool {
    fn name(&self) -> &str { "request_fix" }
    // Input: { "summary": "issues description" }
    // On call: store.set_active_agent(conv_id, FIXER_NAME); store.set_pending_summary(conv_id, summary);
}

pub fn qa_system_prompt(task: &str, working_dir: &str) -> String { ... }

pub fn build_qa_agent(
    task: &str,
    working_dir: &str,
    read_tools: Vec<Arc<ErasedTool>>,
    handoff_store: Arc<HandoffAgentStore>,
    signals: Arc<SignalBus>,
) -> Arc<NamedAgent> {
    let mut tools = read_tools;
    tools.push(Arc::new(RequestFixTool::new(Arc::clone(&handoff_store))) as Arc<ErasedTool>);
    Arc::new(
        NamedAgent::new(QA_NAME, qa_system_prompt(task, working_dir), 25)
            .with_tools(tools)
            .with_signals(signals),
    )
}
```

---

### `fixer.rs` — fixer agent

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FixSummary {
    pub files_fixed: Vec<String>,
    pub description: String,
}

pub fn fixer_system_prompt(task: &str, working_dir: &str) -> String { ... }

pub fn build_fixer_agent(
    task: &str, working_dir: &str,
    write_tools: Vec<Arc<ErasedTool>>,
    signals: Arc<SignalBus>,
) -> Arc<NamedAgent> { ... }
```

---

### `planner.rs` — planner

The planner is already handled by `agtrs_runtime::planner::OneShotPlanner` (not a
NamedAgent). This module wraps the plan-building call and owns the plan-prompt string:

```rust
pub const PLANNER_NAME: &str = "planner";

/// Build a concise plan string for the coder. Calls OneShotPlanner internally.
pub async fn build_plan(
    task: &str,
    tool_names: Vec<String>,
    llm: Arc<dyn LlmProvider>,
    resolve_ctx: Arc<ResolveContext>,
) -> String { ... }  // moved verbatim from orchestrator.rs
```

---

### `summarizer.rs` — concluding summary

```rust
pub const SUMMARIZER_NAME: &str = "summary";

/// Ask an LLM to produce a human-readable task summary (single turn, no tools).
pub async fn build_concluding_summary(
    task: &str, edit_summary: &EditSummary, qa_verdict: &str,
    llm: Arc<dyn LlmProvider>, resolve_ctx: Arc<ResolveContext>,
) -> String { ... }  // moved verbatim from orchestrator.rs::build_concluding_summary
```

---

### `registry.rs` — AgentRegistry (from PRD 20)

```rust
pub enum AgentToolSet { ReadOnly, ReadWrite, Custom(Vec<String>) }

pub struct AgentDefinition {
    pub name: String,
    pub system_prompt_fn: Box<dyn Fn(&str, &str) -> String + Send + Sync>,
    pub tool_set: AgentToolSet,
    pub max_turns: usize,
    pub can_handoff_to: Vec<String>,
}

pub struct AgentRegistry { definitions: IndexMap<String, AgentDefinition> }

impl AgentRegistry {
    pub fn default_xaft() -> Self {
        // Registers planner, coder, qa, fixer, summarizer using the
        // prompt builders from their respective modules above.
        // Returns Self::new()
        //   .register(AgentDefinition { name: "coder", system_prompt_fn: |task, wd| coder::coder_system_prompt(...), ... })
        //   ...
    }

    pub fn register(mut self, def: AgentDefinition) -> Self { ... }

    pub fn build_agent(
        &self, name: &str, task: &str, working_dir: &str,
        read_tools: &[Arc<ErasedTool>], write_tools: &[Arc<ErasedTool>],
        handoff_store: Arc<HandoffAgentStore>, signals: Arc<SignalBus>,
    ) -> Result<Arc<dyn Agent>, AgentError> { ... }
}
```

---

### `handoff.rs` — generic HandoffTool (from PRD 20)

```rust
pub struct HandoffTool {
    store: Arc<HandoffAgentStore>,
    allowed_targets: Vec<String>,
}

impl Tool for HandoffTool {
    fn name(&self) -> &str { "handoff_to_agent" }
    // Input: { "target_agent": "name", "reason": "why" }
    // Validates target is in allowed_targets, then:
    //   store.set_active_agent(conv_id, target)
    //   store.set_pending_summary(conv_id, reason)
}
```

---

## Changes to existing files

### `xaft-runtime/src/orchestrator.rs`

Remove (now in xaft-agents):
- `struct NamedAgent` + impl
- `struct EditSummary` + impls  
- `fn coder_prompt`, `fn qa_prompt`, `fn fixer_prompt`
- `struct RequestFixTool` + impl
- `fn build_plan`, `fn build_concluding_summary`
- Constants `CODER_NAME`, `QA_NAME`, `FIXER_NAME`

Replace with:
```rust
use xaft_agents::{
    coder::{build_coder_tool, EditSummary},
    qa::{build_qa_agent, RequestFixTool},
    fixer::build_fixer_agent,
    planner::build_plan,
    summarizer::build_concluding_summary,
};
```

`run_workflow()` body stays the same; it just calls these builders instead of
constructing everything inline.

### `Cargo.toml` (workspace root)

```toml
[workspace]
members = [..., "crates/xaft-agents"]

[workspace.dependencies]
xaft-agents = { path = "crates/xaft-agents" }
```

### `crates/xaft-runtime/Cargo.toml`

```toml
xaft-agents = { workspace = true }
```

---

## Testing

### Unit tests (in each module file)

```
coder::tests::edit_summary_serializes
coder::tests::build_coder_tool_returns_subagent_tool
qa::tests::request_fix_tool_writes_to_handoff_store
qa::tests::request_fix_tool_empty_conv_id_does_not_panic
fixer::tests::fixer_agent_has_write_tools
registry::tests::default_xaft_has_five_agents
registry::tests::custom_agent_registered
registry::tests::unknown_agent_returns_error
```

### Integration tests (`tests/agent_builder_integration.rs`)

```
// Mock LLM — no API calls needed
coder_tool_runs_with_mock_llm_and_returns_edit_summary
qa_agent_calls_request_fix_on_bad_code
qa_agent_outputs_approved_on_clean_code
handoff_tool_transfers_to_allowed_target
handoff_tool_rejects_disallowed_target
registry_build_all_default_agents_no_panic
```

### Regression test

After the refactor, run existing `xaft-runtime/tests/session_e2e.rs` — all tests
must continue to pass with zero changes to the test file.

---

## Acceptance Criteria

1. `xaft-agents` compiles as a standalone crate.
2. `orchestrator.rs` no longer contains NamedAgent, EditSummary, coder_prompt, qa_prompt, fixer_prompt, RequestFixTool, build_plan, or build_concluding_summary.
3. `session_e2e.rs` tests pass without modification.
4. `AgentRegistry::default_xaft()` returns all 5 agents.
5. All unit + integration tests in `xaft-agents` pass.
6. No regressions in `xaft-tui` tests.
